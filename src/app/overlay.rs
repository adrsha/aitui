//! Overlay (modal) state: fuzzy pickers, the slash-command palette, the settings
//! panel, the agent permission prompt, and the inline `@file` mention popup.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::agent::{
    normalize_lexical, Permission, PermissionDecision, PermissionLifetime, PermissionMemory,
    PermissionRuleDraft, ToolCall, ToolKind,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    Model,
    Session,
    Skill,
    Access,
}

// ── Vim-navigable file browser ────────────────────────────────────────────────

/// What confirming a file in the browser does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowsePurpose {
    /// Attach a single file as message context.
    Attach,
    /// Open the selected file(s) in `$EDITOR`.
    Edit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderPicker {
    pub dir: PathBuf,
    pub entries: Vec<FileEntry>,
    pub cursor: usize,
}

/// One row of the folder picker, rendered by the UI.
pub enum FolderRow {
    /// Navigate to the parent directory.
    Parent,
    /// Descend into a subdirectory.
    Directory(FileEntry),
    /// Choose the current directory, non-recursive (`*`).
    Glob,
    /// Choose the current directory recursively (`**`).
    GlobRecursive,
}

impl FolderPicker {
    pub fn open(dir: PathBuf) -> Self {
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        let entries = read_entries(&dir)
            .into_iter()
            .filter(|entry| entry.is_dir)
            .collect();
        Self {
            dir,
            entries,
            cursor: 0,
        }
    }

    /// Rows: `..` (when a parent exists), subdirectories, then the `*` and `**`
    /// glob options for the current directory.
    pub fn option_count(&self) -> usize {
        self.entries.len() + 2 + usize::from(self.has_parent())
    }

    pub fn row(&self, index: usize) -> Option<FolderRow> {
        let mut row = 0usize;
        if self.has_parent() {
            if index == 0 {
                return Some(FolderRow::Parent);
            }
            row = 1;
        }
        if let Some(entry) = self.entries.get(index.saturating_sub(row)) {
            return Some(FolderRow::Directory(entry.clone()));
        }
        match index.saturating_sub(self.entries.len() + row) {
            0 => Some(FolderRow::Glob),
            1 => Some(FolderRow::GlobRecursive),
            _ => None,
        }
    }

    pub fn has_parent(&self) -> bool {
        self.dir.parent().is_some()
    }

    pub fn down(&mut self) {
        if self.cursor + 1 < self.option_count() {
            self.cursor += 1;
        }
    }

    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Navigate or choose. `None` means the picker stays open (moved up or
    /// descended); `Some((dir, recursive))` is a confirmed glob choice.
    pub fn enter(&mut self) -> Option<(PathBuf, bool)> {
        match self.row(self.cursor) {
            Some(FolderRow::Parent) => {
                self.parent();
                None
            }
            Some(FolderRow::Directory(entry)) => {
                self.set_dir(entry.path);
                None
            }
            Some(FolderRow::Glob) => Some((self.dir.clone(), false)),
            Some(FolderRow::GlobRecursive) => Some((self.dir.clone(), true)),
            None => None,
        }
    }

    fn parent(&mut self) {
        if let Some(parent) = self.dir.parent().map(|p| p.to_path_buf()) {
            let from = self.dir.clone();
            let previous = self.dir.clone();
            self.set_dir(parent);
            // Land the cursor on the directory we came from.
            if let Some(i) = self.entries.iter().position(|e| e.path == from) {
                self.cursor = i + usize::from(previous.parent().is_some());
            }
        }
    }

    fn set_dir(&mut self, dir: PathBuf) {
        self.dir = dir;
        self.entries = read_entries(&self.dir)
            .into_iter()
            .filter(|entry| entry.is_dir)
            .collect();
        self.cursor = 0;
    }
}

/// A directory browser navigated with vim keys (h/j/k/l), with space to
/// multi-select files (selection persists across directories).
#[derive(Debug, Clone, PartialEq)]
pub struct FileBrowser {
    pub purpose: BrowsePurpose,
    pub dir: PathBuf,
    pub entries: Vec<FileEntry>,
    pub cursor: usize,
    pub selected: BTreeSet<PathBuf>,
}

impl FileBrowser {
    pub fn open(dir: PathBuf, purpose: BrowsePurpose, preselect: Vec<PathBuf>) -> Self {
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        let entries = read_entries(&dir);
        let selected: BTreeSet<PathBuf> = preselect
            .into_iter()
            .filter_map(|p| std::fs::canonicalize(&p).ok())
            .collect();
        // Land the cursor on the first selected file in this directory (if any),
        // so a single Enter opens the pre-selected set.
        let cursor = entries
            .iter()
            .position(|e| selected.contains(&e.path))
            .unwrap_or(0);
        Self {
            purpose,
            dir,
            entries,
            cursor,
            selected,
        }
    }

    pub fn current(&self) -> Option<&FileEntry> {
        self.entries.get(self.cursor)
    }

    pub fn down(&mut self) {
        if self.cursor + 1 < self.entries.len() {
            self.cursor += 1;
        }
    }
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Descend into the directory under the cursor.
    pub fn enter_dir(&mut self) {
        if let Some(e) = self.current() {
            if e.is_dir {
                self.set_dir(e.path.clone());
            }
        }
    }

    /// Go up to the parent directory.
    pub fn parent(&mut self) {
        if let Some(parent) = self.dir.parent().map(|p| p.to_path_buf()) {
            let from = self.dir.clone();
            self.set_dir(parent);
            // Land the cursor on the directory we came from.
            if let Some(i) = self.entries.iter().position(|e| e.path == from) {
                self.cursor = i;
            }
        }
    }

    fn set_dir(&mut self, dir: PathBuf) {
        self.dir = dir;
        self.entries = read_entries(&self.dir);
        self.cursor = 0;
    }

    /// Toggle selection of the file under the cursor (directories aren't selectable).
    pub fn toggle_select(&mut self) {
        let target = self.current().filter(|e| !e.is_dir).map(|e| e.path.clone());
        if let Some(path) = target {
            if !self.selected.remove(&path) {
                self.selected.insert(path);
            }
        }
    }

    pub fn is_selected(&self, path: &PathBuf) -> bool {
        self.selected.contains(path)
    }

    /// The files to open/attach on confirm: the selection, or the current file.
    pub fn resolve_targets(&self) -> Vec<PathBuf> {
        if !self.selected.is_empty() {
            self.selected.iter().cloned().collect()
        } else if let Some(e) = self.current() {
            if !e.is_dir {
                return vec![e.path.clone()];
            }
            Vec::new()
        } else {
            Vec::new()
        }
    }
}

/// List a directory: directories first, then files, each sorted case-insensitively.
fn read_entries(dir: &PathBuf) -> Vec<FileEntry> {
    let mut dirs: Vec<FileEntry> = Vec::new();
    let mut files: Vec<FileEntry> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let path = e.path();
            let is_dir = path.is_dir();
            let name = e.file_name().to_string_lossy().to_string();
            let entry = FileEntry {
                name: if is_dir { format!("{}/", name) } else { name },
                is_dir,
                path,
            };
            if is_dir {
                dirs.push(entry)
            } else {
                files.push(entry)
            }
        }
    }
    let key = |e: &FileEntry| e.name.to_lowercase();
    dirs.sort_by_key(key);
    files.sort_by_key(key);
    dirs.into_iter().chain(files).collect()
}

/// A fuzzy-filtered list picker (models or sessions).
#[derive(Debug, Clone, PartialEq)]
pub struct Picker {
    pub kind: PickerKind,
    pub query: String,
    pub items: Vec<String>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub dir: PathBuf,
}

impl Picker {
    pub fn models(items: Vec<String>) -> Self {
        let filtered = (0..items.len()).collect();
        Self {
            kind: PickerKind::Model,
            query: String::new(),
            items,
            filtered,
            selected: 0,
            dir: PathBuf::new(),
        }
    }

    pub fn sessions(items: Vec<String>, active: usize) -> Self {
        let filtered = (0..items.len()).collect();
        let selected = active.min(items.len().saturating_sub(1));
        Self {
            kind: PickerKind::Session,
            query: String::new(),
            items,
            filtered,
            selected,
            dir: PathBuf::new(),
        }
    }

    pub fn skills(items: Vec<String>) -> Self {
        let filtered = (0..items.len()).collect();
        Self {
            kind: PickerKind::Skill,
            query: String::new(),
            items,
            filtered,
            selected: 0,
            dir: PathBuf::new(),
        }
    }

    pub fn access(items: Vec<String>) -> Self {
        let filtered = (0..items.len()).collect();
        Self {
            kind: PickerKind::Access,
            query: String::new(),
            items,
            filtered,
            selected: 0,
            dir: PathBuf::new(),
        }
    }

    /// The original (unfiltered) index of the current selection.
    pub fn selected_index(&self) -> Option<usize> {
        self.filtered.get(self.selected).copied()
    }

    pub fn refilter(&mut self) {
        let q = self.query.to_lowercase();
        self.filtered = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| q.is_empty() || it.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }

    pub fn selected_item(&self) -> Option<&str> {
        self.filtered
            .get(self.selected)
            .map(|&i| self.items[i].as_str())
    }
    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
    pub fn down(&mut self) {
        if self.selected + 1 < self.filtered.len() {
            self.selected += 1;
        }
    }
}

pub use crate::app::commands::SlashCommand;

pub fn slash_commands() -> Vec<&'static SlashCommand> {
    crate::app::commands::slash_commands().collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub query: String,
    pub filtered: Vec<usize>,
    pub selected: usize,
}

impl Palette {
    pub fn new() -> Self {
        let n = slash_commands().len();
        Self {
            query: String::new(),
            filtered: (0..n).collect(),
            selected: 0,
        }
    }
    pub fn refilter(&mut self) {
        let q = self.query.to_lowercase();
        self.filtered = slash_commands()
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                q.is_empty() || c.name.contains(&q) || c.desc.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }
    pub fn selected_cmd(&self) -> Option<&'static SlashCommand> {
        self.filtered
            .get(self.selected)
            .and_then(|&i| slash_commands().get(i).copied())
    }
    pub fn up(&mut self) {
        if self.selected > 0 {
            self.selected = self.selected.saturating_sub(1);
        }
    }
    pub fn down(&mut self) {
        if self.selected + 1 < self.filtered.len() {
            self.selected += 1;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsRow {
    AutoApprove,
    AccessReview,
    InputHeight,
    ReasoningEffort,
    ReasoningMode,
    SystemPrompt,
}

impl SettingsRow {
    pub fn all() -> [SettingsRow; 6] {
        [
            SettingsRow::AutoApprove,
            SettingsRow::AccessReview,
            SettingsRow::InputHeight,
            SettingsRow::ReasoningEffort,
            SettingsRow::ReasoningMode,
            SettingsRow::SystemPrompt,
        ]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub selected: usize,
    pub editing: bool,
    pub edit_buf: String,
}

/// Vim-style `:` command line — simple text input, no filtering/suggestions.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandLine {
    pub input: String,
    pub filtered: Vec<usize>,
    pub selected: usize,
}

impl CommandLine {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            filtered: Vec::new(),
            selected: 0,
        }
    }
    pub fn push(&mut self, c: char) {
        self.input.push(c);
        self.refilter();
    }
    pub fn pop(&mut self) {
        self.input.pop();
        self.refilter();
    }
    pub fn refilter(&mut self) {
        let first_word = self
            .input
            .split_once(' ')
            .map(|(w, _)| w)
            .unwrap_or(&self.input);
        if first_word.is_empty() {
            self.filtered = (0..crate::app::commands::COMMAND_DOCS.len()).collect();
        } else {
            let q = first_word.to_lowercase();
            self.filtered = crate::app::commands::COMMAND_DOCS
                .iter()
                .enumerate()
                .filter(|(_, doc)| doc.name.contains(&q) || doc.aliases.to_lowercase().contains(&q))
                .map(|(i, _)| i)
                .collect();
        }
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }
    pub fn selected_name(&self) -> Option<&'static str> {
        self.filtered
            .get(self.selected)
            .and_then(|&i| crate::app::commands::COMMAND_DOCS.get(i))
            .map(|doc| doc.name)
    }
    pub fn accept_completion(&mut self, name: &str) {
        let rest = self.input.split_once(' ').map(|(_, r)| r).unwrap_or("");
        self.input = if rest.is_empty() {
            format!("{} ", name)
        } else {
            format!("{} {}", name, rest)
        };
        self.filtered.clear();
    }
    pub fn next(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + 1) % self.filtered.len();
        }
    }
    pub fn prev(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = if self.selected == 0 {
                self.filtered.len() - 1
            } else {
                self.selected - 1
            };
        }
    }
    pub fn has_completions(&self) -> bool {
        !self.filtered.is_empty() && !self.input.is_empty()
    }
}

/// Pending tool call(s) awaiting the user's permission decision.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequest {
    pub calls: Vec<ToolCall>,
    pub cwd: PathBuf,
    pub selected: usize,
    pub scroll: usize,
    pub horizontal_scroll: usize,
    pub deny: Option<DenyDraft>,
    pub decision: PermissionDecision,
    pub tool_index: usize,
    pub location_index: usize,
    pub lifetime_index: usize,
    pub include_children: bool,
    pub selecting: bool,
    pub folder_picker: Option<FolderPicker>,
    pub lifetime_explicit: bool,
    pub editing_access: Option<usize>,
    pub custom_directory: String,
    pub custom_value: String,
    pub editing_custom: bool,
}

/// A deny the user has chosen but not yet confirmed, plus the optional reason that
/// goes back to the model. Telling it *why* is what stops it from immediately
/// retrying the same call — a bare "denied" reads as a transient failure.
#[derive(Debug, Clone, PartialEq)]
pub struct DenyDraft {
    pub perm: Permission,
    pub reason: String,
}

/// Plain-ASCII fence lines wrapping every field value in the editable buffer.
/// A value line has to be *exactly* `>>>` to collide, which effectively never
/// happens in real command / code content.
const FIELD_OPEN: &str = "<<<";
const FIELD_CLOSE: &str = ">>>";

impl PermissionRequest {
    pub fn new(calls: Vec<ToolCall>, cwd: PathBuf) -> Self {
        let current_kind = calls.first().and_then(ToolCall::kind);
        let tool_index = current_kind
            .and_then(|kind| {
                ToolKind::all()
                    .iter()
                    .position(|candidate| *candidate == kind)
            })
            .map(|index| index + 1)
            .unwrap_or(0);
        let selected = usize::from(current_kind == Some(ToolKind::Edit));
        let requested_directory = calls
            .first()
            .and_then(|call| call.permission_directory(&cwd))
            .map(|directory| normalize_lexical(&directory));
        let location_index = usize::from(requested_directory.is_some());
        let custom_directory = requested_directory
            .as_ref()
            .map(|directory| directory.to_string_lossy().to_string())
            .unwrap_or_default();
        Self {
            calls,
            cwd,
            selected,
            scroll: 0,
            horizontal_scroll: 0,
            deny: None,
            decision: PermissionDecision::Allow,
            tool_index,
            location_index,
            // Allowing every tool defaults to a session-scoped rule.
            lifetime_index: usize::from(tool_index == 0),
            include_children: false,
            selecting: false,
            folder_picker: None,
            lifetime_explicit: false,
            editing_access: None,
            custom_directory,
            custom_value: "10".into(),
            editing_custom: false,
        }
    }

    /// Build a single-call request. Test-only (production batches via the queue).
    #[cfg(test)]
    pub fn single(call: ToolCall) -> Self {
        Self::new(vec![call], std::env::current_dir().unwrap_or_default())
    }

    pub fn scroll_up(&mut self) {
        self.scroll = self.scroll.saturating_sub(1);
    }
    pub fn scroll_down(&mut self) {
        self.scroll = self.scroll.saturating_add(1);
    }
    pub fn scroll_left(&mut self) {
        self.horizontal_scroll = self.horizontal_scroll.saturating_sub(4);
    }
    pub fn scroll_right(&mut self) {
        self.horizontal_scroll = self.horizontal_scroll.saturating_add(4);
    }

    /// Render the batch as an editable plain-text buffer for `$EDITOR`. Each call
    /// is a `### N tool` block; each editable field is `key:` then its value fenced
    /// between [`FIELD_OPEN`]/[`FIELD_CLOSE`] on their own lines, so multi-line
    /// values (an edit's old→new, a file's content) survive intact. Deleting a
    /// whole block skips (denies) that call.
    pub fn edit_buffer(&self) -> String {
        let mut out = String::new();
        out.push_str(
            "# AiTUI — review & edit these tool calls, then save & quit to run them.\n\
             # Edit any field's value between the <<< and >>> fence lines. Delete a\n\
             # whole \"### N …\" block to skip (deny) that call. Lines starting with # are ignored.\n\n",
        );
        for (i, call) in self.calls.iter().enumerate() {
            let kind = call.kind().map(|k| k.name()).unwrap_or(&call.name);
            out.push_str(&format!("### {} {}\n", i + 1, kind));
            for key in call.editable_arg_keys() {
                let val = call.get_arg(key).unwrap_or("");
                out.push_str(&format!(
                    "{}:\n{}\n{}\n{}\n",
                    key, FIELD_OPEN, val, FIELD_CLOSE
                ));
            }
            out.push('\n');
        }
        out
    }

    /// Apply edits from a buffer produced by [`edit_buffer`]. Field values are
    /// written back onto the matching call (matched by the block's `N`); calls
    /// whose block was deleted are returned as their original indices so the caller
    /// can deny them. Unknown keys / malformed blocks are ignored.
    pub fn apply_edits(&mut self, text: &str) -> Vec<usize> {
        let mut seen = vec![false; self.calls.len()];
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;
        let mut cur: Option<usize> = None;
        while i < lines.len() {
            let line = lines[i];
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("### ") {
                // "### N tool" — take N, remember which call this block edits.
                cur = rest
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse::<usize>().ok())
                    .filter(|&n| n >= 1 && n <= self.calls.len())
                    .map(|n| n - 1);
                if let Some(idx) = cur {
                    seen[idx] = true;
                }
                i += 1;
                continue;
            }
            // "key:" followed by a fenced value.
            if let (Some(idx), Some(key)) = (cur, trimmed.strip_suffix(':')) {
                let key = key.trim();
                if self.calls[idx].editable_arg_keys().contains(&key)
                    && lines.get(i + 1).map(|l| l.trim()) == Some(FIELD_OPEN)
                {
                    let mut j = i + 2;
                    let mut value_lines: Vec<&str> = Vec::new();
                    while j < lines.len() && lines[j].trim() != FIELD_CLOSE {
                        value_lines.push(lines[j]);
                        j += 1;
                    }
                    self.calls[idx].set_arg(key, value_lines.join("\n"));
                    i = j + 1; // skip past the closing fence
                    continue;
                }
            }
            i += 1;
        }
        (0..self.calls.len()).filter(|&k| !seen[k]).collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecisionRequest {
    pub call: ToolCall,
    pub question: String,
    pub options: Vec<String>,
    pub selected: usize,
    pub chosen: BTreeSet<usize>,
    pub multi: bool,
    pub answer: String,
    pub custom_editing: bool,
}

impl DecisionRequest {
    pub fn free_form(&self) -> bool {
        self.options.is_empty()
    }
    pub fn custom_selected(&self) -> bool {
        !self.free_form() && self.selected == self.options.len()
    }
    pub fn editing_answer(&self) -> bool {
        self.free_form() || self.custom_editing
    }
    pub fn option_count(&self) -> usize {
        self.options.len() + usize::from(!self.free_form())
    }
    pub fn toggle_custom_editor(&mut self) {
        if self.free_form() {
            return;
        }
        self.custom_editing = !self.custom_editing;
        if self.custom_editing {
            self.selected = self.options.len();
        }
    }
    pub fn up(&mut self) {
        if !self.custom_editing {
            self.selected = self.selected.saturating_sub(1);
        }
    }
    pub fn down(&mut self) {
        if !self.custom_editing && self.selected + 1 < self.option_count() {
            self.selected += 1;
        }
    }
    pub fn toggle(&mut self) {
        if self.free_form() || self.custom_selected() {
            return;
        }
        if self.multi {
            if !self.chosen.remove(&self.selected) {
                self.chosen.insert(self.selected);
            }
        } else {
            self.chosen.clear();
            self.chosen.insert(self.selected);
        }
    }
    pub fn push(&mut self, c: char) {
        if self.editing_answer() {
            self.answer.push(c);
        }
    }
    pub fn backspace(&mut self) {
        if self.editing_answer() {
            self.answer.pop();
        }
    }
    pub fn labels(&self) -> Vec<String> {
        if self.custom_selected() {
            return (!self.answer.trim().is_empty())
                .then(|| self.answer.trim().to_string())
                .into_iter()
                .collect();
        }
        if self.multi {
            self.chosen
                .iter()
                .filter_map(|&i| self.options.get(i).cloned())
                .collect()
        } else {
            self.options
                .get(self.selected)
                .cloned()
                .into_iter()
                .collect()
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanRequest {
    pub call: ToolCall,
    pub path: PathBuf,
}

pub const PERMISSION_OPTIONS: usize = 6;

impl PermissionRequest {
    const LIFETIME_OPTIONS: usize = 5;
    /// Location scopes: 0 anywhere, 1 requested directory, 2 cwd, 3 cwd and all
    /// descendants, 4 custom directory (typed or folder-picked).
    const LOCATION_OPTIONS: usize = 5;

    pub fn up(&mut self) {
        if self.deny.is_none() && !self.editing_custom {
            if self.selecting {
                self.adjust(-1);
            } else {
                self.selected = self.selected.saturating_sub(1);
            }
        }
    }
    pub fn down(&mut self) {
        if self.deny.is_none() && !self.editing_custom {
            if self.selecting {
                self.adjust(1);
            } else if self.selected + 1 < PERMISSION_OPTIONS {
                self.selected += 1;
            }
        }
    }
    pub fn adjust(&mut self, direction: i32) {
        if self.editing_custom || self.deny.is_some() {
            return;
        }
        let forward = direction >= 0;
        match self.selected {
            0 => {
                self.decision = match self.decision {
                    PermissionDecision::Allow => PermissionDecision::Deny,
                    PermissionDecision::Deny => PermissionDecision::Allow,
                }
            }
            1 => {
                let count = ToolKind::all().len() + 1;
                let old = self.tool_index;
                self.tool_index = cycle_index(self.tool_index, count, forward);
                if old != 0 && self.tool_index == 0 && !self.lifetime_explicit {
                    self.lifetime_index = 1;
                }
            }
            2 => self.include_children = !self.include_children,
            3 => {
                self.location_index =
                    cycle_index(self.location_index, Self::LOCATION_OPTIONS, forward);
            }
            4 => {
                self.lifetime_index =
                    cycle_index(self.lifetime_index, Self::LIFETIME_OPTIONS, forward);
                self.lifetime_explicit = true;
            }
            _ => {}
        }
    }
    pub fn permission(&self) -> Permission {
        let current_kind = self.calls.first().and_then(ToolCall::kind);
        let kind = if self.tool_index == 0 {
            None
        } else {
            ToolKind::all()
                .get(self.tool_index - 1)
                .copied()
                .or(current_kind)
        };
        let custom_dir = (!self.custom_directory.trim().is_empty()).then(|| {
            let path = PathBuf::from(self.custom_directory.trim());
            if path.is_absolute() {
                normalize_lexical(&path)
            } else {
                normalize_lexical(&self.cwd.join(path))
            }
        });
        let requested = self
            .calls
            .first()
            .and_then(|call| call.permission_directory(&self.cwd));
        let (directory, include_children) = match self.location_index {
            0 => (None, false),
            1 => (
                requested.clone(),
                self.include_children && requested.is_some(),
            ),
            2 => (Some(self.cwd.clone()), self.include_children),
            3 => (Some(self.cwd.clone()), true),
            _ => (custom_dir, self.include_children),
        };
        let value = self.custom_value.trim().parse::<u64>().unwrap_or(1).max(1);
        let lifetime = match self.lifetime_index {
            0 => PermissionLifetime::Once,
            1 => PermissionLifetime::Session,
            2 => PermissionLifetime::Minutes(value),
            3 => PermissionLifetime::MatchingRequests(value.min(u32::MAX as u64) as u32),
            _ => PermissionLifetime::GeneralRequests(value.min(u32::MAX as u64) as u32),
        };
        let include_children = include_children && directory.is_some();
        Permission::Custom(PermissionRuleDraft {
            decision: self.decision,
            kind,
            directory,
            include_children,
            lifetime,
        })
    }

    pub fn tool_label(&self) -> String {
        if self.tool_index == 0 {
            "all access types".into()
        } else {
            ToolKind::all()
                .get(self.tool_index - 1)
                .map(|kind| kind.name().to_string())
                .unwrap_or_else(|| "all access types".into())
        }
    }

    pub fn location_label(&self) -> String {
        match self.location_index {
            0 => "anywhere".into(),
            1 => self
                .calls
                .first()
                .and_then(|call| call.permission_directory(&self.cwd))
                .map(|directory| crate::render::path::display_path(&directory))
                .unwrap_or_else(|| "anywhere".into()),
            2 => crate::render::path::display_path(&self.cwd),
            3 => format!(
                "{} (all descendants)",
                crate::render::path::display_path(&self.cwd)
            ),
            _ if !self.custom_directory.is_empty() => {
                crate::render::path::display_path(std::path::Path::new(&self.custom_directory))
            }
            _ => "custom directory".into(),
        }
    }

    pub fn lifetime_label(&self) -> String {
        match self.lifetime_index {
            0 => "this request only".into(),
            1 => "current session".into(),
            2 => format!("{} minutes", self.custom_value),
            3 => format!("next {} matching requests", self.custom_value),
            _ => format!("next {} total requests", self.custom_value),
        }
    }

    pub fn custom_editable(&self) -> bool {
        self.selected == 4 && self.lifetime_index >= 2
    }

    /// Natural-language policy used when Enter delegates this batch to the access
    /// review model. Matching calls receive the selected decision; everything else
    /// remains an explicit human decision.
    pub fn review_policy(&self) -> String {
        let decision = if self.decision == PermissionDecision::Allow {
            "ALLOW"
        } else {
            "DENY"
        };
        let children = if self.include_children && self.location_index != 0 {
            " including all child directories"
        } else {
            ""
        };
        format!(
            "For this pending batch, return {decision} only for calls using {} in {}{children}. Return ASK for every non-matching or uncertain call. The selected rule lifetime is {}.",
            self.tool_label(),
            self.location_label(),
            self.lifetime_label(),
        )
    }

    pub fn toggle_selector(&mut self) {
        if self.selected == 3 && !self.editing_custom {
            if self.folder_picker.is_none() {
                let start = if self.custom_directory.is_empty() {
                    self.cwd.clone()
                } else {
                    PathBuf::from(&self.custom_directory)
                };
                self.folder_picker = Some(FolderPicker::open(start));
            } else if let Some((directory, recursive)) =
                self.folder_picker.as_mut().and_then(FolderPicker::enter)
            {
                self.custom_directory = directory.to_string_lossy().to_string();
                self.location_index = 4;
                self.include_children = recursive;
                self.folder_picker = None;
            }
            return;
        }
        if self.selected < PERMISSION_OPTIONS - 1 && !self.editing_custom {
            self.selecting = !self.selecting;
        }
    }

    pub fn selector_up(&mut self) {
        if let Some(picker) = self.folder_picker.as_mut() {
            picker.up();
        } else {
            self.up();
        }
    }

    pub fn selector_down(&mut self) {
        if let Some(picker) = self.folder_picker.as_mut() {
            picker.down();
        } else {
            self.down();
        }
    }

    pub fn selector_parent(&mut self) {
        if let Some(picker) = self.folder_picker.as_mut() {
            picker.parent();
        }
    }

    pub fn selecting_folder(&self) -> bool {
        self.folder_picker.is_some()
    }

    pub fn close_selector(&mut self) {
        self.selecting = false;
        self.folder_picker = None;
    }

    pub fn toggle_custom_edit(&mut self) {
        if self.custom_editable() {
            self.editing_custom = !self.editing_custom;
        }
    }

    pub fn writing_reason(&self) -> bool {
        self.deny.is_some()
    }

    /// Open the reason box for `perm`. Denies route through here rather than
    /// applying immediately, so the model can be told why.
    pub fn begin_deny(&mut self, perm: Permission) {
        self.deny = Some(DenyDraft {
            perm,
            reason: String::new(),
        });
    }

    /// Back out of the reason box, returning to the menu with nothing decided.
    pub fn cancel_deny(&mut self) {
        self.deny = None;
    }

    pub fn push(&mut self, c: char) {
        if let Some(draft) = self.deny.as_mut() {
            draft.reason.push(c);
        } else if self.editing_custom {
            if self.selected == 3 {
                self.custom_directory.push(c);
            } else {
                self.custom_value.push(c);
            }
        }
    }

    pub fn backspace(&mut self) {
        if let Some(draft) = self.deny.as_mut() {
            draft.reason.pop();
        } else if self.editing_custom {
            if self.selected == 3 {
                self.custom_directory.pop();
            } else {
                self.custom_value.pop();
            }
        }
    }

    /// The confirmed deny: its permission plus the reason, empty text meaning none.
    pub fn deny_choice(&self) -> Option<(Permission, Option<String>)> {
        let draft = self.deny.as_ref()?;
        let reason = draft.reason.trim();
        Some((
            draft.perm.clone(),
            (!reason.is_empty()).then(|| reason.to_string()),
        ))
    }
}

fn cycle_index(current: usize, count: usize, forward: bool) -> usize {
    if count == 0 {
        return 0;
    }
    if forward {
        (current + 1) % count
    } else {
        current.checked_sub(1).unwrap_or(count - 1)
    }
}

/// The model emitted tool call(s) while agent mode is off. Ask whether to enable
/// agent mode and run them, or decline and let the model answer without tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRequest {
    /// Session whose streamed reply contains the pending tool call(s).
    pub sid: usize,
    /// How many tool calls the model asked for.
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApiSetup {
    pub endpoint: String,
    pub api_key: String,
    /// Which field is focused: 0 = endpoint, 1 = api key.
    pub field: usize,
}

impl ApiSetup {
    pub fn new(endpoint: String, api_key: String) -> Self {
        Self {
            endpoint,
            api_key,
            field: 0,
        }
    }
    pub fn next_field(&mut self) {
        self.field = (self.field + 1) % 2;
    }
    fn current_mut(&mut self) -> &mut String {
        if self.field == 0 {
            &mut self.endpoint
        } else {
            &mut self.api_key
        }
    }
    pub fn push(&mut self, c: char) {
        self.current_mut().push(c);
    }
    pub fn backspace(&mut self) {
        self.current_mut().pop();
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Overlay {
    None,
    Picker(Picker),
    Browser(FileBrowser),
    Palette(Palette),
    Settings(Settings),
    Permission(PermissionRequest),
    Decision(DecisionRequest),
    Plan(PlanRequest),
    /// Model asked for tools while agent mode is off — enable & run, or decline.
    ToolRequest(ToolRequest),
    /// Enter API endpoint + key (on a connection/base-URL failure, or `:setup`).
    ApiSetup(ApiSetup),
    /// Scrollable live detail for one parallel child agent.
    SubtaskDetail {
        task_id: u64,
        scroll: usize,
    },
    /// Vim-style `:` command line (simple text input, no filtering).
    CommandLine(CommandLine),
    /// A transient informational dialog (title + body). Dismissed by any key.
    Notice {
        title: String,
        body: String,
    },
}

impl Overlay {
    pub fn is_browser(&self) -> bool {
        matches!(self, Overlay::Browser(_))
    }
}

/// Inline `@file` mention completion.
#[derive(Debug, Clone, Default)]
pub struct Mention {
    pub active: bool,
    pub query: String,
    pub anchor_row: usize,
    pub anchor_col: usize,
    pub matches: Vec<String>,
    pub selected: usize,
}

impl Mention {
    pub fn reset(&mut self) {
        *self = Mention::default();
    }
    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
    pub fn down(&mut self) {
        if self.selected + 1 < self.matches.len() {
            self.selected += 1;
        }
    }
}

/// Seed/clear the read-only auto-approvals based on config.
pub fn sync_auto_approvals(mem: &mut PermissionMemory, enabled: bool) {
    let reads = [ToolKind::Read, ToolKind::List, ToolKind::Search];
    if enabled {
        for k in reads {
            mem.remember_allow(k);
        }
    } else {
        mem.always_allow.retain(|k| !reads.contains(k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── FileBrowser ──────────────────────────────────────────────────────────────

    fn browser(entries: &[(&str, bool)]) -> FileBrowser {
        let entries = entries
            .iter()
            .map(|(n, d)| FileEntry {
                name: n.to_string(),
                is_dir: *d,
                path: PathBuf::from(n),
            })
            .collect();
        FileBrowser {
            purpose: BrowsePurpose::Edit,
            dir: PathBuf::from("/x"),
            entries,
            cursor: 0,
            selected: BTreeSet::new(),
        }
    }

    #[test]
    fn browser_navigation_stays_in_bounds() {
        let mut b = browser(&[("a/", true), ("b.rs", false), ("c.rs", false)]);
        b.up();
        assert_eq!(b.cursor, 0);
        b.down();
        b.down();
        b.down();
        assert_eq!(b.cursor, 2); // clamped at last
    }

    #[test]
    fn browser_selects_files_not_dirs() {
        let mut b = browser(&[("dir/", true), ("f.rs", false)]);
        b.toggle_select(); // on a dir → no-op
        assert!(b.selected.is_empty());
        b.down();
        b.toggle_select(); // on a file → selected
        assert_eq!(b.selected.len(), 1);
        b.toggle_select(); // toggle off
        assert!(b.selected.is_empty());
    }

    #[test]
    fn browser_resolve_targets_prefers_selection_else_current() {
        let mut b = browser(&[("a.rs", false), ("b.rs", false)]);
        // No selection → the current file.
        assert_eq!(b.resolve_targets(), vec![PathBuf::from("a.rs")]);
        b.toggle_select();
        b.down();
        b.toggle_select();
        // Selection → all selected files.
        let mut got = b.resolve_targets();
        got.sort();
        assert_eq!(got, vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")]);
    }

    #[test]
    fn browser_resolve_targets_empty_on_directory() {
        let b = browser(&[("dir/", true)]);
        assert!(b.resolve_targets().is_empty());
    }

    #[test]
    fn folder_picker_parent_returns_to_previous_directory() {
        let base = std::env::temp_dir().join(format!(
            "aitui-folder-picker-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let child = base.join("child");
        std::fs::create_dir_all(&child).unwrap();
        let mut picker = FolderPicker::open(child.clone());

        picker.parent();

        assert_eq!(picker.dir, std::fs::canonicalize(&base).unwrap());
        assert!(
            matches!(picker.row(picker.cursor), Some(FolderRow::Directory(entry)) if entry.path == std::fs::canonicalize(&child).unwrap())
        );
        let _ = std::fs::remove_dir_all(base);
    }

    // ── Picker ─────────────────────────────────────────────────────────────────

    #[test]
    fn picker_filters_by_query() {
        let mut p = Picker::models(vec!["main.rs".into(), "lib.rs".into(), "README.md".into()]);
        p.query = "rs".into();
        p.refilter();
        assert_eq!(p.filtered.len(), 2);
        p.query = "main".into();
        p.refilter();
        assert_eq!(p.filtered.len(), 1);
        assert_eq!(p.selected_item(), Some("main.rs"));
    }

    #[test]
    fn picker_empty_query_shows_all() {
        let items = vec!["a.rs".into(), "b.rs".into()];
        let p = Picker::models(items.clone());
        assert_eq!(p.filtered.len(), 2);
    }

    #[test]
    fn picker_navigation_cycles_within_bounds() {
        let mut p = Picker::models(vec!["m1".into(), "m2".into(), "m3".into()]);
        assert_eq!(p.selected, 0);
        p.up();
        assert_eq!(p.selected, 0); // stays at 0
        p.down();
        assert_eq!(p.selected, 1);
        p.down();
        assert_eq!(p.selected, 2);
        p.down();
        assert_eq!(p.selected, 2); // stays at max
    }

    #[test]
    fn picker_selected_item_none_when_empty() {
        let p = Picker::models(vec![]);
        assert!(p.selected_item().is_none());
    }

    // ── Palette ────────────────────────────────────────────────────────────────

    #[test]
    fn palette_filters_by_name_and_description() {
        let mut p = Palette::new();
        assert!(!p.filtered.is_empty());
        p.query = "model".into();
        p.refilter();
        let cmd = p.selected_cmd().unwrap();
        assert_eq!(cmd.name, "model");
    }

    #[test]
    fn palette_selected_clamps_to_filtered() {
        let mut p = Palette::new();
        p.query = "zzz_nonexistent".into();
        p.refilter();
        assert!(p.filtered.is_empty());
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn palette_navigation() {
        let mut p = Palette::new();
        let initial = p.selected;
        p.down();
        assert_eq!(p.selected, initial + 1);
        p.up();
        assert_eq!(p.selected, initial);
    }

    #[test]
    fn custom_response_is_virtual_last_option_and_overrides_labels() {
        let mut req = DecisionRequest {
            call: ToolCall {
                name: "ask".into(),
                args: serde_json::json!({}),
                id: None,
            },
            question: "Choose".into(),
            options: vec!["A".into(), "B".into()],
            selected: 0,
            chosen: BTreeSet::new(),
            multi: false,
            answer: String::new(),
            custom_editing: false,
        };
        req.down();
        req.down();
        assert!(req.custom_selected());
        req.toggle_custom_editor();
        for c in "different path".chars() {
            req.push(c);
        }
        assert_eq!(req.labels(), vec!["different path"]);
    }

    #[test]
    fn custom_editor_freezes_option_navigation_until_tab_closes_it() {
        let mut req = DecisionRequest {
            call: ToolCall {
                name: "ask".into(),
                args: serde_json::json!({}),
                id: None,
            },
            question: "Choose".into(),
            options: vec!["A".into(), "B".into()],
            selected: 0,
            chosen: BTreeSet::new(),
            multi: false,
            answer: String::new(),
            custom_editing: false,
        };
        req.toggle_custom_editor();
        req.up();
        assert!(req.custom_selected());
        req.toggle_custom_editor();
        req.up();
        assert_eq!(req.selected, 1);
    }

    // ── PermissionRequest ──────────────────────────────────────────────────────

    #[test]
    fn permission_defaults_to_current_tool_once() {
        let req = PermissionRequest::single(ToolCall {
            name: "read_file".into(),
            args: serde_json::json!({}),
            id: None,
        });
        assert_eq!(
            req.permission(),
            Permission::Custom(PermissionRuleDraft {
                decision: PermissionDecision::Allow,
                kind: Some(ToolKind::Read),
                directory: None,
                include_children: false,
                lifetime: PermissionLifetime::Once,
            })
        );
    }

    #[test]
    fn permission_dimensions_compose_a_custom_rule() {
        let mut req = PermissionRequest::single(ToolCall {
            name: "read_file".into(),
            args: serde_json::json!({ "path": "src/main.rs" }),
            id: None,
        });
        req.adjust(1);
        req.down();
        req.adjust(1);
        req.down();
        req.down();
        req.adjust(1);
        req.down();
        req.adjust(1);

        assert_eq!(req.tool_label(), "list");
        assert_eq!(
            req.location_label(),
            crate::render::path::display_path(&std::env::current_dir().unwrap())
        );
        assert_eq!(req.lifetime_label(), "current session");
        assert_eq!(
            req.permission(),
            Permission::Custom(PermissionRuleDraft {
                decision: PermissionDecision::Deny,
                kind: Some(ToolKind::List),
                directory: Some(std::env::current_dir().unwrap()),
                include_children: false,
                lifetime: PermissionLifetime::Session,
            })
        );
    }

    #[test]
    fn permission_custom_request_limit_is_editable() {
        let mut req = PermissionRequest::single(ToolCall {
            name: "shell".into(),
            args: serde_json::json!({ "command": "cargo test" }),
            id: None,
        });
        req.selected = 4;
        for _ in 0..3 {
            req.adjust(1);
        }
        req.custom_value = "3".into();
        assert!(matches!(
            req.permission(),
            Permission::Custom(PermissionRuleDraft {
                lifetime: PermissionLifetime::MatchingRequests(3),
                ..
            })
        ));
    }

    #[test]
    fn edit_requests_focus_access_type_and_scope_requested_directory() {
        let req = PermissionRequest::single(ToolCall {
            name: "edit".into(),
            args: serde_json::json!({
                "path": "src/main.rs",
                "old": "old",
                "new": "new"
            }),
            id: None,
        });
        assert_eq!(req.selected, 1);
        assert_eq!(req.tool_label(), "edit");
        assert!(req.location_label().contains("src"));
    }

    #[test]
    fn every_permission_dimension_combination_builds_a_rule() {
        let mut req = PermissionRequest::single(ToolCall {
            name: "read".into(),
            args: serde_json::json!({ "path": "src/main.rs" }),
            id: None,
        });
        req.custom_directory = "src".into();
        req.custom_value = "3".into();
        for decision in [PermissionDecision::Allow, PermissionDecision::Deny] {
            for tool_index in 0..=ToolKind::all().len() {
                for location_index in 0..PermissionRequest::LOCATION_OPTIONS {
                    for include_children in [false, true] {
                        for lifetime_index in 0..PermissionRequest::LIFETIME_OPTIONS {
                            req.decision = decision;
                            req.tool_index = tool_index;
                            req.location_index = location_index;
                            req.include_children = include_children;
                            req.lifetime_index = lifetime_index;
                            let Permission::Custom(rule) = req.permission() else {
                                panic!("custom editor must always emit a custom rule");
                            };
                            assert_eq!(rule.decision, decision);
                            assert_eq!(
                                rule.include_children,
                                location_index == 3 || (include_children && location_index != 0)
                            );
                            assert!(!req.tool_label().is_empty());
                            assert!(!req.location_label().is_empty());
                            assert!(!req.lifetime_label().is_empty());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn child_directory_option_only_applies_with_a_location_scope() {
        let mut req = PermissionRequest::single(ToolCall {
            name: "read".into(),
            args: serde_json::json!({ "path": "src/main.rs" }),
            id: None,
        });
        req.include_children = true;
        req.location_index = 0;
        let Permission::Custom(anywhere) = req.permission() else {
            panic!("custom rule");
        };
        assert!(!anywhere.include_children);

        req.location_index = 2;
        let Permission::Custom(scoped) = req.permission() else {
            panic!("custom rule");
        };
        assert!(scoped.include_children);
        assert_eq!(scoped.directory, Some(std::env::current_dir().unwrap()));
    }

    #[test]
    fn every_lifetime_option_maps_to_its_individual_behavior() {
        let mut req = PermissionRequest::single(ToolCall {
            name: "shell".into(),
            args: serde_json::json!({ "command": "cargo test" }),
            id: None,
        });
        req.custom_value = "7".into();
        let expected = [
            PermissionLifetime::Once,
            PermissionLifetime::Session,
            PermissionLifetime::Minutes(7),
            PermissionLifetime::MatchingRequests(7),
            PermissionLifetime::GeneralRequests(7),
        ];
        for (index, lifetime) in expected.into_iter().enumerate() {
            req.lifetime_index = index;
            let Permission::Custom(rule) = req.permission() else {
                panic!("custom rule");
            };
            assert_eq!(rule.lifetime, lifetime);
        }
    }

    #[test]
    fn automated_review_policy_contains_every_selected_condition() {
        let mut req = PermissionRequest::single(ToolCall {
            name: "read".into(),
            args: serde_json::json!({ "path": "src/main.rs" }),
            id: None,
        });
        req.decision = PermissionDecision::Deny;
        req.tool_index = ToolKind::all()
            .iter()
            .position(|kind| *kind == ToolKind::Read)
            .unwrap()
            + 1;
        req.location_index = 4;
        req.custom_directory = "src".into();
        req.include_children = true;
        req.lifetime_index = 3;
        req.custom_value = "4".into();

        let policy = req.review_policy();
        assert!(policy.contains("DENY"));
        assert!(policy.contains("read"));
        assert!(policy.contains("src"));
        assert!(policy.contains("including all child directories"));
        assert!(policy.contains("next 4 matching requests"));
        assert!(policy.contains("ASK"));
    }

    #[test]
    fn permission_locations_use_the_session_cwd_not_the_process_cwd() {
        let session_cwd = std::env::temp_dir().join("aitui-permission-session-cwd");
        let req = PermissionRequest::new(
            vec![ToolCall {
                name: "read".into(),
                args: serde_json::json!({ "path": "src/main.rs" }),
                id: None,
            }],
            session_cwd.clone(),
        );

        assert_eq!(req.cwd, session_cwd);
        assert!(req.location_label().contains("src"));
        let Permission::Custom(rule) = req.permission() else {
            panic!("custom rule");
        };
        assert_eq!(rule.directory, Some(req.cwd.join("src")));
    }

    #[test]
    fn permission_down_bounded() {
        let mut req = PermissionRequest::single(ToolCall {
            name: "read_file".into(),
            args: serde_json::json!({}),
            id: None,
        });
        for _ in 0..20 {
            req.down();
        }
        assert_eq!(req.selected, PERMISSION_OPTIONS - 1);
    }

    #[test]
    fn edit_buffer_roundtrips_multiline_edits() {
        let mut req = PermissionRequest::new(
            vec![
                ToolCall {
                    name: "shell".into(),
                    args: serde_json::json!({ "command": "cargo test" }),
                    id: None,
                },
                ToolCall {
                    name: "edit".into(),
                    args: serde_json::json!({
                        "path": "src/main.rs",
                        "old": "let x = 1;\nlet y = 2;",
                        "new": "let x = 10;",
                    }),
                    id: None,
                },
            ],
            std::env::current_dir().unwrap(),
        );
        // Simulate the user editing the shell command and the edit's `new` body.
        let edited = req
            .edit_buffer()
            .replace("cargo test", "cargo test --release")
            .replace("let x = 10;", "let x = 10;\nlet z = 3;");
        let dropped = req.apply_edits(&edited);
        assert!(dropped.is_empty());
        assert_eq!(
            req.calls[0].get_arg("command"),
            Some("cargo test --release")
        );
        // Multi-line `old` survives untouched; `new` gains its second line.
        assert_eq!(req.calls[1].get_arg("old"), Some("let x = 1;\nlet y = 2;"));
        assert_eq!(req.calls[1].get_arg("new"), Some("let x = 10;\nlet z = 3;"));
    }

    #[test]
    fn apply_edits_reports_deleted_blocks_as_dropped() {
        let mut req = PermissionRequest::new(
            vec![
                ToolCall {
                    name: "shell".into(),
                    args: serde_json::json!({ "command": "a" }),
                    id: None,
                },
                ToolCall {
                    name: "shell".into(),
                    args: serde_json::json!({ "command": "b" }),
                    id: None,
                },
            ],
            std::env::current_dir().unwrap(),
        );
        // Keep only the first block; the user removed the second entirely.
        let kept: String = req
            .edit_buffer()
            .lines()
            .take_while(|l| !l.starts_with("### 2"))
            .collect::<Vec<_>>()
            .join("\n");
        let dropped = req.apply_edits(&kept);
        assert_eq!(dropped, vec![1]);
        assert_eq!(req.calls[0].get_arg("command"), Some("a"));
    }

    // ── Session picker ───────────────────────────────────────────────────────────

    #[test]
    fn session_picker_selects_active_and_maps_index() {
        let p = Picker::sessions(vec!["a".into(), "b".into(), "c".into()], 2);
        assert_eq!(p.selected, 2);
        assert_eq!(p.selected_index(), Some(2));
    }

    // ── Mention ────────────────────────────────────────────────────────────────

    #[test]
    fn mention_reset_clears_state() {
        let mut m = Mention {
            active: true,
            query: "foo".into(),
            anchor_row: 1,
            anchor_col: 2,
            matches: vec!["a".into()],
            selected: 0,
        };
        m.reset();
        assert!(!m.active);
        assert!(m.query.is_empty());
        assert!(m.matches.is_empty());
    }

    #[test]
    fn mention_navigation_stays_bounded() {
        let mut m = Mention {
            active: true,
            query: String::new(),
            anchor_row: 0,
            anchor_col: 0,
            matches: vec!["a".into(), "b".into()],
            selected: 0,
        };
        assert_eq!(m.selected, 0);
        m.up(); // stays at 0
        assert_eq!(m.selected, 0);
        m.down();
        assert_eq!(m.selected, 1);
        m.down(); // stays at 1 (max index)
        assert_eq!(m.selected, 1);
    }

    #[test]
    fn mention_down_no_matches_no_panic() {
        let mut m = Mention::default();
        m.down();
        assert_eq!(m.selected, 0);
    }

    // ── SettingsRow ────────────────────────────────────────────────────────────

    #[test]
    fn settings_row_all_returns_every_row() {
        assert_eq!(SettingsRow::all().len(), 6);
    }

    // ── SlashCommand ───────────────────────────────────────────────────────────

    #[test]
    fn slash_commands_are_well_formed() {
        for cmd in slash_commands() {
            assert!(!cmd.name.is_empty());
            assert!(!cmd.desc.is_empty());
            assert!(!cmd.run.is_empty());
        }
    }

    // ── sync_auto_approvals ────────────────────────────────────────────────────

    #[test]
    fn sync_approvals_adds_read_tools() {
        let mut mem = PermissionMemory::default();
        sync_auto_approvals(&mut mem, true);
        assert!(mem.always_allow.contains(&ToolKind::Read));
        assert!(mem.always_allow.contains(&ToolKind::List));
    }

    #[test]
    fn sync_approvals_disabled_clears_read_tools() {
        let mut mem = PermissionMemory::default();
        mem.remember_allow(ToolKind::Read);
        sync_auto_approvals(&mut mem, false);
        assert!(!mem.always_allow.contains(&ToolKind::Read));
    }
}
