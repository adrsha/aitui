//! Overlay (modal) state: fuzzy pickers, the slash-command palette, the settings
//! panel, the agent permission prompt, and the inline `@file` mention popup.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::agent::{Permission, PermissionMemory, ToolCall, ToolKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    Model,
    Session,
    Skill,
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

/// A discoverable slash command.
#[derive(Debug, Clone, Copy)]
pub struct SlashCommand {
    pub name: &'static str,
    pub icon: &'static str,
    pub desc: &'static str,
    pub run: &'static str,
}

pub fn slash_commands() -> &'static [SlashCommand] {
    &[
        SlashCommand {
            name: "send",
            icon: "▸",
            desc: "Send the message",
            run: "w",
        },
        SlashCommand {
            name: "agent",
            icon: "◇",
            desc: "Toggle agent (tool-using) mode",
            run: "agent",
        },
        SlashCommand {
            name: "mock",
            icon: "⚗",
            desc: "Toggle offline mock/test mode",
            run: "mock",
        },
        SlashCommand {
            name: "model",
            icon: "◆",
            desc: "Pick the model",
            run: "models",
        },
        SlashCommand {
            name: "attach",
            icon: "▤",
            desc: "Attach a file",
            run: "files",
        },
        SlashCommand {
            name: "new",
            icon: "+",
            desc: "Start a new session",
            run: "new",
        },
        SlashCommand {
            name: "fork",
            icon: "⑂",
            desc: "Fork this session into a parallel branch",
            run: "fork",
        },
        SlashCommand {
            name: "retry",
            icon: "↻",
            desc: "Regenerate the last reply",
            run: "retry",
        },
        SlashCommand {
            name: "edit-last",
            icon: "✎",
            desc: "Edit your last message and resend",
            run: "edit-last",
        },
        SlashCommand {
            name: "copy",
            icon: "⧉",
            desc: "Copy the last reply to the clipboard",
            run: "copy",
        },
        SlashCommand {
            name: "copy-code",
            icon: "⧉",
            desc: "Copy the last code block to the clipboard",
            run: "copy-code",
        },
        SlashCommand {
            name: "effort",
            icon: "🧠",
            desc: "Cycle reasoning effort (low/medium/high/off)",
            run: "effort",
        },
        SlashCommand {
            name: "sessions",
            icon: "≡",
            desc: "Switch session",
            run: "sessions",
        },
        SlashCommand {
            name: "skills",
            icon: "✦",
            desc: "Toggle skills (personas / instructions)",
            run: "skills",
        },
        SlashCommand {
            name: "editor",
            icon: "⌨",
            desc: "Open conversation in $EDITOR",
            run: "editor",
        },
        SlashCommand {
            name: "edit",
            icon: "✎",
            desc: "Open a file in $EDITOR (edited files first)",
            run: "edit",
        },
        SlashCommand {
            name: "shell",
            icon: "▮",
            desc: "Drop into a shell, then return",
            run: "shell",
        },
        SlashCommand {
            name: "rename",
            icon: "✎",
            desc: "Rename the current session",
            run: "rename ",
        },
        SlashCommand {
            name: "clear",
            icon: "⌫",
            desc: "Clear the conversation",
            run: "clear",
        },
        SlashCommand {
            name: "setup",
            icon: "🔑",
            desc: "Set API endpoint URL + key",
            run: "setup",
        },
        SlashCommand {
            name: "settings",
            icon: "⚙",
            desc: "Open settings",
            run: "settings",
        },
        SlashCommand {
            name: "system",
            icon: "✦",
            desc: "Edit the system prompt",
            run: "settings",
        },
        SlashCommand {
            name: "help",
            icon: "?",
            desc: "Keybinding help",
            run: "help",
        },
        SlashCommand {
            name: "quit",
            icon: "⏻",
            desc: "Quit",
            run: "quit",
        },
    ]
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
            .map(|&i| &slash_commands()[i])
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsRow {
    AgentDefault,
    AutoApprove,
    InputHeight,
    SystemPrompt,
}

impl SettingsRow {
    pub fn all() -> [SettingsRow; 4] {
        [
            SettingsRow::AgentDefault,
            SettingsRow::AutoApprove,
            SettingsRow::InputHeight,
            SettingsRow::SystemPrompt,
        ]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub selected: usize,
    pub editing_prompt: bool,
    pub prompt_buf: String,
}

/// Pending tool call(s) awaiting the user's permission decision.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequest {
    pub calls: Vec<ToolCall>,
    pub selected: usize,
    /// First display-line of the (possibly long) command list that's visible; the
    /// renderer windows the list from here so a big batch stays scrollable.
    pub scroll: usize,
}

/// Plain-ASCII fence lines wrapping every field value in the editable buffer.
/// A value line has to be *exactly* `>>>` to collide, which effectively never
/// happens in real command / code content.
const FIELD_OPEN: &str = "<<<";
const FIELD_CLOSE: &str = ">>>";

impl PermissionRequest {
    pub fn new(calls: Vec<ToolCall>) -> Self {
        Self {
            calls,
            selected: 0,
            scroll: 0,
        }
    }

    /// Build a single-call request. Test-only (production batches via the queue).
    #[cfg(test)]
    pub fn single(call: ToolCall) -> Self {
        Self::new(vec![call])
    }

    pub fn scroll_up(&mut self) {
        self.scroll = self.scroll.saturating_sub(1);
    }
    pub fn scroll_down(&mut self) {
        self.scroll = self.scroll.saturating_add(1);
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
}

impl DecisionRequest {
    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
    pub fn down(&mut self) {
        if self.selected + 1 < self.options.len() {
            self.selected += 1;
        }
    }
    pub fn toggle(&mut self) {
        if self.multi {
            if !self.chosen.remove(&self.selected) {
                self.chosen.insert(self.selected);
            }
        } else {
            self.chosen.clear();
            self.chosen.insert(self.selected);
        }
    }
    pub fn labels(&self) -> Vec<String> {
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

/// The permission menu, in display order. Four allow options then four deny
/// options, each: once · all of this tool type · all in this directory · timed.
pub const PERMISSION_OPTIONS: usize = 8;

impl PermissionRequest {
    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
    pub fn down(&mut self) {
        if self.selected + 1 < PERMISSION_OPTIONS {
            self.selected += 1;
        }
    }
    pub fn permission(&self) -> Permission {
        match self.selected {
            0 => Permission::Allow,
            1 => Permission::AllowKind,
            2 => Permission::AllowDirectory,
            3 => Permission::AllowTimed,
            4 => Permission::Deny,
            5 => Permission::DenyKind,
            6 => Permission::DenyDirectory,
            _ => Permission::DenyTimed,
        }
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

/// The launch screen: choose to resume a saved session (which `cd`s to that
/// session's folder) or start a fresh one. Shown once at startup when there is
/// at least one non-empty session to resume.
#[derive(Debug, Clone, PartialEq)]
pub struct Startup {
    /// Highlighted row. Index 0 is "new session"; 1..=sessions map to session
    /// index `selected - 1`.
    pub selected: usize,
    /// Number of resumable sessions shown below the "new session" row.
    pub sessions: usize,
}

impl Startup {
    pub fn new(sessions: usize) -> Self {
        Self {
            selected: 0,
            sessions,
        }
    }
    /// Total selectable rows ("new" + each session).
    pub fn options(&self) -> usize {
        self.sessions + 1
    }
    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
    pub fn down(&mut self) {
        if self.selected + 1 < self.options() {
            self.selected += 1;
        }
    }
}

/// Prompt to enter the API endpoint URL + key, shown when a request fails because
/// the endpoint is missing/relative (or via `:setup`). On confirm, the values are
/// saved to config and the API client is rebuilt.
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
    Startup(Startup),
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

    /// Whether any overlay is showing (used to dim the transcript behind it).
    pub fn is_active(&self) -> bool {
        !matches!(self, Overlay::None)
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
        assert!(p.filtered.len() > 0);
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

    // ── PermissionRequest ──────────────────────────────────────────────────────

    #[test]
    fn permission_maps_correctly() {
        let req = PermissionRequest::single(ToolCall {
            name: "read_file".into(),
            args: serde_json::json!({}),
            id: None,
        });
        assert_eq!(req.permission(), Permission::Allow);
    }

    #[test]
    fn permission_selected_1_allow_all() {
        let mut req = PermissionRequest::single(ToolCall {
            name: "read_file".into(),
            args: serde_json::json!({}),
            id: None,
        });
        req.down();
        assert_eq!(req.permission(), Permission::AllowKind);
    }

    #[test]
    fn permission_selected_maps_all_eight() {
        let mut req = PermissionRequest::single(ToolCall {
            name: "read_file".into(),
            args: serde_json::json!({}),
            id: None,
        });
        let expected = [
            Permission::Allow,
            Permission::AllowKind,
            Permission::AllowDirectory,
            Permission::AllowTimed,
            Permission::Deny,
            Permission::DenyKind,
            Permission::DenyDirectory,
            Permission::DenyTimed,
        ];
        for want in expected {
            assert_eq!(req.permission(), want);
            req.down();
        }
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
        let mut req = PermissionRequest::new(vec![
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
        ]);
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
        let mut req = PermissionRequest::new(vec![
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
        ]);
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
    fn settings_row_all_returns_four() {
        assert_eq!(SettingsRow::all().len(), 4);
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
