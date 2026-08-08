// This module defines the full tool catalogue (names, descriptions, schemas,
// risk levels). Some accessors are part of the complete API but not yet wired
// into the UI, so allow dead code here.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::OnceLock;

/// Represents a tool the agent can call. Lean, single-purpose catalogue (Unix
/// philosophy): each variant does exactly one thing. Legacy names map onto these
/// via `from_name` so older sessions and habitual model calls still resolve.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ToolKind {
    Read,
    Write,
    Edit,
    List,
    Search,
    MakeDir,
    Shell,
    Move,
    Copy,
    Delete,
    WebSearch,
    WebImages,
    ReverseImage,
    WebFetch,
    Download,
    /// Generate a validated animated PowerPoint deck from a structured slide spec.
    PowerPoint,
    Todo,
    Ask,
    Plan,
    ProposeStep,
    /// Launch a focused parallel child agent for independent work.
    Task,
    /// Autonomous-loop control: the model calls this to signal the loop's stop
    /// criteria are met (or that it's blocked), ending the loop.
    Finish,
}

impl ToolKind {
    pub fn name(&self) -> &'static str {
        match self {
            ToolKind::Read => "read",
            ToolKind::Write => "write",
            ToolKind::Edit => "edit",
            ToolKind::List => "list",
            ToolKind::Search => "search",
            ToolKind::MakeDir => "mkdir",
            ToolKind::Shell => "shell",
            ToolKind::Move => "move",
            ToolKind::Copy => "copy",
            ToolKind::Delete => "delete",
            ToolKind::WebSearch => "web_search",
            ToolKind::WebImages => "web_images",
            ToolKind::ReverseImage => "reverse_image",
            ToolKind::WebFetch => "web_fetch",
            ToolKind::Download => "download",
            ToolKind::PowerPoint => "powerpoint",
            ToolKind::Todo => "todo",
            ToolKind::Ask => "ask",
            ToolKind::Plan => "plan",
            ToolKind::ProposeStep => "propose_step",
            ToolKind::Task => "agent",
            ToolKind::Finish => "finish",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            ToolKind::Read => "Read a file's contents (optionally a line window)",
            ToolKind::Write => "Create or overwrite a whole file",
            ToolKind::Edit => "Replace an exact, unique snippet in a file",
            ToolKind::List => "List a directory (optionally as a tree)",
            ToolKind::Search => "Search file contents for a regex pattern",
            ToolKind::MakeDir => "Create a directory, including missing parents",
            ToolKind::Shell => "Run a shell command (build/test/run)",
            ToolKind::Move => "Move or rename a file or directory",
            ToolKind::Copy => "Copy a file or directory (recursive)",
            ToolKind::Delete => "Delete a file or directory tree permanently",
            ToolKind::WebSearch => "Search the web; returns titled results with links",
            ToolKind::WebImages => "Search Wikimedia Commons for reusable images and metadata",
            ToolKind::ReverseImage => {
                "Find visually similar images and source pages from an image URL or local file"
            }
            ToolKind::WebFetch => "Fetch the readable text of a web page",
            ToolKind::Download => "Download a URL to a local file",
            ToolKind::PowerPoint => "Generate a validated animated PowerPoint deck",
            ToolKind::Todo => "Set the task breakdown shown in the sticky todo panel",
            ToolKind::Ask => "Ask the user for missing information or a decision",
            ToolKind::Plan => "Write a plan file for user review and approval",
            ToolKind::ProposeStep => "Present one workflow step with genuine alternative paths",
            ToolKind::Task => "Launch a focused parallel child agent for independent work",
            ToolKind::Finish => "End the autonomous loop: stop criteria met (or blocked)",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            ToolKind::Read => "◈",
            ToolKind::Write => "⤒",
            ToolKind::Edit => "✎",
            ToolKind::List => "☰",
            ToolKind::Search => "⌕",
            ToolKind::MakeDir => "▣",
            ToolKind::Shell => "$",
            ToolKind::Move => "⇄",
            ToolKind::Copy => "⧉",
            ToolKind::Delete => "✗",
            ToolKind::WebSearch => "⇗",
            ToolKind::WebImages => "▦",
            ToolKind::ReverseImage => "↺",
            ToolKind::WebFetch => "⤓",
            ToolKind::Download => "⇩",
            ToolKind::PowerPoint => "▤",
            ToolKind::Todo => "☑",
            ToolKind::Ask => "?",
            ToolKind::Plan => "◫",
            ToolKind::ProposeStep => "⇉",
            ToolKind::Task => "⚙",
            ToolKind::Finish => "✓",
        }
    }

    /// Risk level: low = auto-approve possible; high = always ask
    pub fn risk(&self) -> ToolRisk {
        match self {
            ToolKind::Read => ToolRisk::Low,
            ToolKind::List => ToolRisk::Low,
            ToolKind::Search => ToolRisk::Low,
            ToolKind::WebSearch => ToolRisk::Low,
            ToolKind::WebImages => ToolRisk::Low,
            ToolKind::ReverseImage => ToolRisk::Low,
            ToolKind::WebFetch => ToolRisk::Low,
            ToolKind::MakeDir => ToolRisk::Medium,
            ToolKind::Write => ToolRisk::Medium,
            ToolKind::Edit => ToolRisk::Medium,
            ToolKind::Move => ToolRisk::Medium,
            ToolKind::Copy => ToolRisk::Medium,
            ToolKind::Download => ToolRisk::Medium,
            ToolKind::PowerPoint => ToolRisk::Medium,
            ToolKind::Todo => ToolRisk::Low,
            ToolKind::Ask => ToolRisk::Low,
            ToolKind::Plan => ToolRisk::Low,
            ToolKind::ProposeStep => ToolRisk::Low,
            ToolKind::Task => ToolRisk::Low,
            ToolKind::Finish => ToolRisk::Low,
            ToolKind::Delete => ToolRisk::High,
            ToolKind::Shell => ToolRisk::High,
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        // TODO(audit): retire legacy aliases once persisted sessions are migrated;
        // aliases currently make schema/prompt/executor behavior harder to reason about.
        match name {
            "read" | "read_file" => Some(ToolKind::Read),
            "write" | "write_file" => Some(ToolKind::Write),
            "edit" | "edit_file" => Some(ToolKind::Edit),
            "list" | "list_dir" => Some(ToolKind::List),
            "search" | "search_files" => Some(ToolKind::Search),
            "mkdir" | "make_dir" => Some(ToolKind::MakeDir),
            "shell" | "run_shell" => Some(ToolKind::Shell),
            "move" | "move_path" => Some(ToolKind::Move),
            "copy" | "copy_path" => Some(ToolKind::Copy),
            "delete" | "delete_file" | "delete_dir" => Some(ToolKind::Delete),
            "web_search" => Some(ToolKind::WebSearch),
            "web_images" | "image_search" => Some(ToolKind::WebImages),
            "reverse_image" | "reverse_image_search" => Some(ToolKind::ReverseImage),
            "web_fetch" => Some(ToolKind::WebFetch),
            "download" | "download_file" => Some(ToolKind::Download),
            "powerpoint" | "pptx" | "presentation" => Some(ToolKind::PowerPoint),
            "todo" | "todos" | "todo_write" => Some(ToolKind::Todo),
            "ask" | "decide" => Some(ToolKind::Ask),
            "plan" => Some(ToolKind::Plan),
            "propose_step" => Some(ToolKind::ProposeStep),
            "agent" | "task" | "subagent" | "sub_agent" => Some(ToolKind::Task),
            "finish" | "done" | "complete" | "stop_loop" => Some(ToolKind::Finish),
            _ => None,
        }
    }

    /// All tools, in display order.
    pub fn all() -> Vec<ToolKind> {
        vec![
            ToolKind::Read,
            ToolKind::List,
            ToolKind::Search,
            ToolKind::MakeDir,
            ToolKind::Edit,
            ToolKind::Write,
            ToolKind::Move,
            ToolKind::Copy,
            ToolKind::Shell,
            ToolKind::WebSearch,
            ToolKind::WebImages,
            ToolKind::ReverseImage,
            ToolKind::WebFetch,
            ToolKind::Download,
            ToolKind::PowerPoint,
            ToolKind::Delete,
        ]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolRisk {
    Low,
    Medium,
    High,
}

impl ToolRisk {
    pub fn label(&self) -> &'static str {
        match self {
            ToolRisk::Low => "LOW",
            ToolRisk::Medium => "MEDIUM",
            ToolRisk::High => "HIGH",
        }
    }
}

/// A parsed tool call from the model's response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    /// Name of the tool
    pub name: String,
    /// Arguments as a flat map
    pub args: serde_json::Value,
    /// Optional call ID from the model
    pub id: Option<String>,
}

impl ToolCall {
    pub fn kind(&self) -> Option<ToolKind> {
        match self.name.as_str() {
            "file_management" => self.category_action().and_then(|action| match action {
                "read" => Some(ToolKind::Read),
                "write" => Some(ToolKind::Write),
                "edit" => Some(ToolKind::Edit),
                "list" => Some(ToolKind::List),
                "search" => Some(ToolKind::Search),
                "mkdir" => Some(ToolKind::MakeDir),
                "move" => Some(ToolKind::Move),
                "copy" => Some(ToolKind::Copy),
                "delete" => Some(ToolKind::Delete),
                _ => None,
            }),
            "web" => self.category_action().and_then(|action| match action {
                "search" => Some(ToolKind::WebSearch),
                "images" | "image_search" => Some(ToolKind::WebImages),
                "reverse_image" | "reverse" => Some(ToolKind::ReverseImage),
                "fetch" => Some(ToolKind::WebFetch),
                "download" => Some(ToolKind::Download),
                _ => None,
            }),
            "specialized" => self.category_action().and_then(|action| match action {
                "powerpoint" | "pptx" | "presentation" => Some(ToolKind::PowerPoint),
                _ => None,
            }),
            "interaction" => self.category_action().and_then(|action| match action {
                "ask" => Some(ToolKind::Ask),
                "propose" => Some(ToolKind::ProposeStep),
                "plan" => Some(ToolKind::Plan),
                _ => None,
            }),
            "workflow" => self.category_action().and_then(|action| match action {
                "todo" => Some(ToolKind::Todo),
                "agent" | "task" => Some(ToolKind::Task),
                "propose" => Some(ToolKind::ProposeStep),
                "finish" => Some(ToolKind::Finish),
                _ => None,
            }),
            _ => ToolKind::from_name(&self.name),
        }
    }

    fn category_action(&self) -> Option<&str> {
        self.args.get("action").and_then(|v| v.as_str())
    }

    /// Directories a call operates in for permission matching. Multi-path and
    /// batch operations must keep every endpoint inside a scoped rule.
    pub fn permission_directories(&self, cwd: &Path) -> Vec<PathBuf> {
        let mut raw_paths: Vec<String> = Vec::new();
        let mut collect = |args: &serde_json::Value| match self.kind() {
            Some(ToolKind::Shell) => raw_paths.push(".".into()),
            Some(ToolKind::Move) | Some(ToolKind::Copy) => {
                raw_paths.extend(
                    ["from", "to"]
                        .into_iter()
                        .filter_map(|key| args.get(key).and_then(|value| value.as_str()))
                        .map(str::to_string),
                );
            }
            Some(ToolKind::Download) => raw_paths.extend(
                args.get("path")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
            ),
            Some(ToolKind::PowerPoint) => raw_paths.extend(
                args.get("output_path")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
            ),
            Some(ToolKind::WebSearch)
            | Some(ToolKind::WebImages)
            | Some(ToolKind::ReverseImage)
            | Some(ToolKind::WebFetch) => {}
            _ => {
                raw_paths.extend(
                    args.get("path")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                );
                raw_paths.extend(
                    args.get("paths")
                        .and_then(|value| value.as_array())
                        .into_iter()
                        .flatten()
                        .filter_map(|value| value.as_str())
                        .map(str::to_string),
                );
            }
        };
        if let Some(batch) = self.args.get("batch").and_then(|value| value.as_array()) {
            for args in batch {
                collect(args);
            }
        } else {
            collect(&self.args);
        }
        raw_paths
            .into_iter()
            .map(|raw| {
                let path = PathBuf::from(raw);
                let path = if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                };
                let directory = if path.is_dir() {
                    path
                } else {
                    path.parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| cwd.to_path_buf())
                };
                normalize_lexical(&directory)
            })
            .collect()
    }

    /// Primary directory shown in the access editor.
    pub fn permission_directory(&self, cwd: &Path) -> Option<PathBuf> {
        self.permission_directories(cwd).into_iter().next()
    }

    /// Whether this call's `path` argument resolves *outside* the project tree
    /// (`cwd`). Used to keep the blanket read auto-approval confined to the project:
    /// a read of `~/.ssh/id_rsa` or `../../etc/passwd` escapes and still prompts.
    ///
    /// Resolution is lexical (`..`/`.` collapsed without touching the filesystem),
    /// so it flags an escape even for a path that doesn't exist yet, and can't be
    /// fooled into a slow/undefined canonicalize on a bogus path.
    pub fn reads_outside_cwd(&self, cwd: &std::path::Path) -> bool {
        let base = normalize_lexical(cwd);
        self.permission_directories(cwd)
            .into_iter()
            .any(|directory| !directory.starts_with(&base))
    }

    /// Expand a batched parent request into the concrete calls it represents.
    /// Returns `None` for an ordinary single call. Keeping this normalization on
    /// `ToolCall` lets execution, permission previews, and transcript rendering
    /// agree on the actual arguments instead of displaying empty top-level fields.
    pub fn expanded_calls(&self) -> Result<Option<Vec<ToolCall>>, String> {
        let Some(parent) = self.args.as_object() else {
            return Ok(None);
        };

        let make_call = |args: serde_json::Map<String, serde_json::Value>| ToolCall {
            name: self.name.clone(),
            args: serde_json::Value::Object(args),
            id: self.id.clone(),
        };

        if let Some(batch) = parent.get("batch") {
            let batch = batch
                .as_array()
                .ok_or("'batch' must be an array of argument objects")?;
            let mut base = parent.clone();
            base.remove("batch");
            base.remove("paths");
            base.remove("commands");
            let mut calls = Vec::with_capacity(batch.len());
            for item in batch {
                let item = item
                    .as_object()
                    .ok_or("Every 'batch' item must be an argument object")?;
                let mut args = base.clone();
                args.extend(item.clone());
                calls.push(make_call(args));
            }
            return Ok(Some(calls));
        }

        if let Some(paths) = parent.get("paths") {
            let paths = paths
                .as_array()
                .ok_or("'paths' must be an array of file or directory paths")?;
            let mut base = parent.clone();
            base.remove("paths");
            let mut calls = Vec::with_capacity(paths.len());
            for path in paths {
                let path = path.as_str().ok_or("Every 'paths' item must be a string")?;
                let mut args = base.clone();
                args.insert("path".into(), serde_json::Value::String(path.into()));
                calls.push(make_call(args));
            }
            return Ok(Some(calls));
        }

        if let Some(commands) = parent.get("commands") {
            let commands = commands
                .as_array()
                .ok_or("'commands' must be an array of shell command strings")?;
            let mut base = parent.clone();
            base.remove("commands");
            let mut calls = Vec::with_capacity(commands.len());
            for command in commands {
                let command = command
                    .as_str()
                    .ok_or("Every 'commands' item must be a string")?;
                let mut args = base.clone();
                args.insert("command".into(), serde_json::Value::String(command.into()));
                calls.push(make_call(args));
            }
            return Ok(Some(calls));
        }

        Ok(None)
    }

    /// Human-readable summary of what this call will do, rendered function-call
    /// style: `name(primary args)`. Reused as the transcript header for the call.
    pub fn summary(&self) -> String {
        let batch_len = self
            .args
            .get("batch")
            .or_else(|| self.args.get("paths"))
            .or_else(|| self.args.get("commands"))
            .and_then(|value| value.as_array())
            .map(Vec::len);
        if let Some(count) = batch_len {
            let name = self.kind().map(|kind| kind.name()).unwrap_or(&self.name);
            return format!("{}({} operations)", name, count);
        }
        let s = |k: &str| self.args.get(k).and_then(|v| v.as_str());
        let path = || s("path").unwrap_or("?");
        match self.kind() {
            Some(ToolKind::Read) => format!("read({})", path()),
            Some(ToolKind::Write) => {
                let lines = s("content").map(|c| c.lines().count()).unwrap_or(0);
                format!("write({} · {} lines)", path(), lines)
            }
            Some(ToolKind::Edit) => format!("edit({})", path()),
            Some(ToolKind::List) => format!("list({})", s("path").unwrap_or(".")),
            Some(ToolKind::Shell) => format!("shell({})", s("command").unwrap_or("?")),
            Some(ToolKind::Search) => {
                let pat = s("pattern").or_else(|| s("query")).unwrap_or("?");
                format!("search(\"{}\")", pat)
            }
            Some(ToolKind::MakeDir) => format!("mkdir({})", path()),
            Some(ToolKind::Delete) => format!("delete({})", path()),
            Some(ToolKind::Move) => {
                format!(
                    "move({} → {})",
                    s("from").unwrap_or("?"),
                    s("to").unwrap_or("?")
                )
            }
            Some(ToolKind::Copy) => {
                format!(
                    "copy({} → {})",
                    s("from").unwrap_or("?"),
                    s("to").unwrap_or("?")
                )
            }
            Some(ToolKind::WebSearch) => {
                let q = s("query").or_else(|| s("q")).unwrap_or("?");
                format!("web_search(\"{}\")", q)
            }
            Some(ToolKind::WebImages) => {
                let q = s("query").or_else(|| s("q")).unwrap_or("?");
                format!("web_images(\"{}\")", q)
            }
            Some(ToolKind::ReverseImage) => format!(
                "reverse_image({})",
                s("url").or_else(|| s("path")).unwrap_or("?")
            ),
            Some(ToolKind::WebFetch) => format!("web_fetch({})", s("url").unwrap_or("?")),
            Some(ToolKind::Download) => {
                format!("download({} → {})", s("url").unwrap_or("?"), path())
            }
            Some(ToolKind::PowerPoint) => {
                let slides = self
                    .args
                    .get("slides")
                    .and_then(|value| value.as_array())
                    .map(Vec::len)
                    .unwrap_or(0);
                format!(
                    "powerpoint({} · {} slide{})",
                    s("output_path").unwrap_or("?"),
                    slides,
                    if slides == 1 { "" } else { "s" }
                )
            }
            Some(ToolKind::Todo) => {
                let n = self
                    .args
                    .get("items")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                format!("todo({} items)", n)
            }
            Some(ToolKind::Ask) => {
                let q = s("question").unwrap_or("?");
                let n = self
                    .args
                    .get("options")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                format!("ask(\"{}\" · {} options)", q, n)
            }
            Some(ToolKind::Plan) => {
                let lines = s("body").map(|c| c.lines().count()).unwrap_or(0);
                format!("plan({} · {} lines)", path(), lines)
            }
            Some(ToolKind::ProposeStep) => {
                let title = s("title").unwrap_or("?");
                let n = self
                    .args
                    .get("alternatives")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                format!("propose_step(\"{}\" · {} paths)", title, n)
            }
            Some(ToolKind::Task) => {
                let desc = s("description").or_else(|| s("prompt")).unwrap_or("?");
                let index = self
                    .args
                    .get("agent_index")
                    .and_then(|value| value.as_u64());
                let task_index = self.args.get("task_index").and_then(|value| value.as_u64());
                match (index, task_index) {
                    (Some(index), Some(task_index)) => {
                        format!("agent {} → task {} (\"{}\")", index, task_index, desc)
                    }
                    (Some(index), None) => format!("agent {} (\"{}\")", index, desc),
                    (None, Some(task_index)) => {
                        format!("agent → task {} (\"{}\")", task_index, desc)
                    }
                    (None, None) => format!("agent(\"{}\")", desc),
                }
            }
            Some(ToolKind::Finish) => {
                let why = s("summary").or_else(|| s("reason")).unwrap_or("done");
                format!("finish(\"{}\")", why)
            }
            None => format!("{}({})", self.name, self.args),
        }
    }

    /// The argument keys a permission prompt lets the user review and edit, in
    /// display order. Covers every field that defines what the call *does* — the
    /// shell command, a file path, an edit's old→new snippets, a move's from→to —
    /// so the whole action (including the diff) is editable before it runs. Empty
    /// for tools that never reach the permission prompt (todo/ask/plan).
    pub fn editable_arg_keys(&self) -> &'static [&'static str] {
        match self.kind() {
            Some(ToolKind::Shell) => &["command"],
            Some(ToolKind::Read)
            | Some(ToolKind::List)
            | Some(ToolKind::MakeDir)
            | Some(ToolKind::Delete) => &["path"],
            Some(ToolKind::Write) => &["path", "content"],
            Some(ToolKind::Edit) => &["path", "old", "new"],
            Some(ToolKind::Search) => &["pattern"],
            Some(ToolKind::Move) | Some(ToolKind::Copy) => &["from", "to"],
            Some(ToolKind::WebSearch) | Some(ToolKind::WebImages) => &["query"],
            Some(ToolKind::ReverseImage) => &["url", "path"],
            Some(ToolKind::WebFetch) => &["url"],
            Some(ToolKind::Download) => &["url", "path"],
            Some(ToolKind::PowerPoint) => &["output_path"],
            _ => &[],
        }
    }

    /// String value of an argument, if present.
    pub fn get_arg(&self, key: &str) -> Option<&str> {
        self.args.get(key).and_then(|v| v.as_str())
    }

    /// Set (or replace) a string argument, used to apply the user's inline edits.
    pub fn set_arg(&mut self, key: &str, val: String) {
        if let Some(obj) = self.args.as_object_mut() {
            obj.insert(key.to_string(), serde_json::Value::String(val));
        }
    }
}

/// True for the read-only, auto-approvable tool family.
fn is_read_family(kind: ToolKind) -> bool {
    matches!(kind, ToolKind::Read | ToolKind::List | ToolKind::Search)
}

/// Whether `raw` (relative to `cwd` when not absolute) lexically resolves outside
/// the project tree. Shared with the access-policy safety floor so a mutation that
/// escapes `cwd` can be forced to prompt. Lexical, like `reads_outside_cwd`.
pub(crate) fn path_escapes_cwd(raw: &str, cwd: &Path) -> bool {
    let rp = Path::new(raw);
    let joined = if rp.is_absolute() {
        rp.to_path_buf()
    } else {
        cwd.join(rp)
    };
    !normalize_lexical(&joined).starts_with(normalize_lexical(cwd))
}

/// Collapse `.` and `..` components lexically (no filesystem access), so
/// `/proj/../etc` becomes `/etc`. Symlinks are not resolved — this is a
/// conservative containment check, and treating a symlink target as "inside" only
/// happens if its lexical path is inside, which is the safe direction.
pub fn normalize_lexical(p: &std::path::Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Result of executing a tool.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub call: ToolCall,
    pub output: Result<String, String>,
    pub duration_ms: u64,
}

impl ToolResult {
    pub fn success(call: ToolCall, output: String, duration_ms: u64) -> Self {
        Self {
            call,
            output: Ok(output),
            duration_ms,
        }
    }
    pub fn failure(call: ToolCall, err: String, duration_ms: u64) -> Self {
        Self {
            call,
            output: Err(err),
            duration_ms,
        }
    }
    pub fn is_ok(&self) -> bool {
        self.output.is_ok()
    }
    pub fn text(&self) -> &str {
        match &self.output {
            Ok(s) | Err(s) => s.as_str(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Permission {
    Allow,
    AllowKind,
    AllowDirectory,
    AllowTimed,
    Deny,
    DenyKind,
    DenyDirectory,
    DenyTimed,
    Custom(PermissionRuleDraft),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionLifetime {
    Once,
    Session,
    Indefinite,
    Minutes(u64),
    MatchingRequests(u32),
    GeneralRequests(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRuleDraft {
    pub decision: PermissionDecision,
    pub kind: Option<ToolKind>,
    pub directory: Option<PathBuf>,
    pub include_children: bool,
    pub lifetime: PermissionLifetime,
}

impl PermissionRuleDraft {
    pub fn matches(&self, call: &ToolCall, cwd: &Path) -> bool {
        let kind_matches = self
            .kind
            .is_none_or(|expected| call.kind() == Some(expected));
        let directory_matches = self.directory.as_ref().is_none_or(|expected| {
            let actual = call.permission_directories(cwd);
            !actual.is_empty()
                && actual.iter().all(|directory| {
                    directory == expected
                        || (self.include_children && directory.starts_with(expected))
                })
        });
        kind_matches && directory_matches
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionScope {
    Kind(ToolKind),
    Directory(PathBuf),
    Timed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRule {
    pub decision: PermissionDecision,
    pub scope: PermissionScope,
    /// Optional extra filters used by fully customized rules.
    pub kind: Option<ToolKind>,
    pub directory: Option<PathBuf>,
    pub include_children: bool,
    /// Unix timestamp in seconds. `None` means it lasts until the app exits.
    pub expires_at: Option<u64>,
    pub remaining_matching: Option<u32>,
    pub remaining_general: Option<u32>,
}

/// Per-session permission memory. Rules are in-memory only and last for this app
/// session; timed rules expire automatically during checks.
#[derive(Debug, Clone, Default)]
pub struct PermissionMemory {
    /// Kept for settings/tests and treated as kind-scoped allow rules.
    pub always_allow: Vec<ToolKind>,
    /// Kept for settings/tests and treated as kind-scoped deny rules.
    pub always_deny: Vec<ToolKind>,
    pub rules: Vec<PermissionRule>,
    /// A natural-language description of what access the user is willing to
    /// auto-grant this session. When set, an uncovered call is judged against it by
    /// a fast model (see `agent::access`) before ever prompting. `None` = judge off.
    pub policy: Option<String>,
}

impl PermissionMemory {
    pub const TIMED_SECS: u64 = 10 * 60;

    pub fn check(&mut self, call: &ToolCall, cwd: &Path) -> Option<PermissionDecision> {
        self.prune_expired();
        let kind = call.kind()?;
        let directories = call.permission_directories(cwd);

        let deny = self.rules.iter().position(|rule| {
            rule.decision == PermissionDecision::Deny && rule_matches(rule, &kind, &directories)
        });
        let allow = self.rules.iter().position(|rule| {
            rule.decision == PermissionDecision::Allow && rule_matches(rule, &kind, &directories)
        });
        let rule_decision = deny.or(allow).map(|index| self.rules[index].decision);

        if self.always_deny.contains(&kind) || rule_decision == Some(PermissionDecision::Deny) {
            return Some(PermissionDecision::Deny);
        }
        let kind_auto = self.always_allow.contains(&kind)
            && !(is_read_family(kind) && call.reads_outside_cwd(cwd));
        if kind_auto || rule_decision == Some(PermissionDecision::Allow) {
            return Some(PermissionDecision::Allow);
        }
        None
    }

    pub fn consume(&mut self, call: &ToolCall, cwd: &Path) -> Option<PermissionDecision> {
        let decision = self.check(call, cwd);
        let Some(kind) = call.kind() else {
            return decision;
        };
        let directories = call.permission_directories(cwd);
        let matched = self
            .rules
            .iter()
            .position(|rule| {
                rule.decision == PermissionDecision::Deny && rule_matches(rule, &kind, &directories)
            })
            .or_else(|| {
                self.rules.iter().position(|rule| {
                    rule.decision == PermissionDecision::Allow
                        && rule_matches(rule, &kind, &directories)
                })
            });
        for (index, rule) in self.rules.iter_mut().enumerate() {
            if let Some(remaining) = rule.remaining_general.as_mut() {
                *remaining = remaining.saturating_sub(1);
            }
            if Some(index) == matched {
                if let Some(remaining) = rule.remaining_matching.as_mut() {
                    *remaining = remaining.saturating_sub(1);
                }
            }
        }
        self.prune_expired();
        decision
    }

    /// Set (or clear, when blank) the natural-language session access policy.
    pub fn set_policy(&mut self, text: &str) {
        let t = text.trim();
        self.policy = if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        };
    }

    pub fn remember_allow(&mut self, kind: ToolKind) {
        if !self.always_allow.contains(&kind) {
            self.always_allow.push(kind);
        }
    }

    pub fn remember_deny(&mut self, kind: ToolKind) {
        self.always_allow.retain(|k| k != &kind);
        self.always_deny.retain(|k| k != &kind);
        self.always_deny.push(kind);
    }

    pub fn remember_rule(
        &mut self,
        decision: PermissionDecision,
        scope: PermissionScope,
        timed: bool,
    ) {
        let expires_at = timed.then(|| now_secs() + Self::TIMED_SECS);
        self.rules
            .retain(|r| !(r.decision == decision && r.scope == scope));
        self.rules.push(PermissionRule {
            decision,
            scope,
            kind: None,
            directory: None,
            include_children: false,
            expires_at,
            remaining_matching: None,
            remaining_general: None,
        });
    }

    pub fn remember_custom_rule(&mut self, draft: PermissionRuleDraft) {
        let PermissionRuleDraft {
            decision,
            kind,
            directory,
            include_children,
            lifetime,
        } = draft;
        if lifetime == PermissionLifetime::Once {
            return;
        }
        let scope = match (&kind, &directory) {
            (Some(kind), _) => PermissionScope::Kind(*kind),
            (None, Some(directory)) => PermissionScope::Directory(directory.clone()),
            (None, None) => PermissionScope::Timed,
        };
        let expires_at = match lifetime {
            PermissionLifetime::Minutes(minutes) => Some(now_secs() + minutes.saturating_mul(60)),
            _ => None,
        };
        let remaining_matching = match lifetime {
            PermissionLifetime::MatchingRequests(count) => Some(count),
            _ => None,
        };
        let remaining_general = match lifetime {
            PermissionLifetime::GeneralRequests(count) => Some(count),
            _ => None,
        };
        self.rules.push(PermissionRule {
            decision,
            scope,
            kind,
            directory,
            include_children,
            expires_at,
            remaining_matching,
            remaining_general,
        });
    }

    fn prune_expired(&mut self) {
        let now = now_secs();
        self.rules.retain(|r| {
            r.expires_at.is_none_or(|t| t > now)
                && r.remaining_matching.is_none_or(|n| n > 0)
                && r.remaining_general.is_none_or(|n| n > 0)
        });
    }
}

fn rule_matches(rule: &PermissionRule, kind: &ToolKind, directories: &[PathBuf]) -> bool {
    let directory_matches = |expected: &PathBuf| {
        let expected_norm = normalize_lexical(expected);
        !directories.is_empty()
            && directories.iter().all(|actual| {
                let actual_norm = normalize_lexical(actual);
                actual_norm == expected_norm
                    || (rule.include_children && actual_norm.starts_with(&expected_norm))
            })
    };
    let legacy_scope = match &rule.scope {
        PermissionScope::Kind(k) => k == kind,
        PermissionScope::Directory(d) => directory_matches(d),
        PermissionScope::Timed => true,
    };
    legacy_scope
        && rule.kind.is_none_or(|expected| expected == *kind)
        && rule.directory.as_ref().is_none_or(directory_matches)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Build the system prompt for agent mode.
/// Tools are defined in attached JSON schemas — this prompt provides identity,
/// safety rules, and workflow guidance only (no per-tool descriptions).
/// Cached: regenerated only when cwd or the named-agent registry changes.
pub fn agent_system_prompt(
    cwd: &Path,
    agents: &BTreeMap<String, crate::config::types::AgentDef>,
) -> String {
    static CACHE: Mutex<Option<(PathBuf, String, String)>> = Mutex::new(None);
    let agents_block = if agents.is_empty() {
        String::new()
    } else {
        let mut block =
            String::from("\nConfigured child agents (use `workflow(agent)` with these names):\n");
        for (name, def) in agents {
            let desc = if def.description.is_empty() {
                "no description".to_string()
            } else {
                def.description.clone()
            };
            let mut line = format!("- {name}: {desc}");
            if let Some(model) = def.model.as_deref() {
                line.push_str(&format!(" (model: {model})"));
            }
            block.push_str(&line);
            block.push('\n');
        }
        block
    };
    let mut cache = CACHE.lock().unwrap();
    if let Some((cached_cwd, cached_agents, cached_prompt)) = &*cache {
        if cached_cwd == cwd && cached_agents == &agents_block {
            return cached_prompt.clone();
        }
    }
    let prompt = format!(
        r#"You are an agentic coding assistant with filesystem + shell + web access.
CWD: {}

Use the attached tool schemas (6 categories: file_management, shell, web, specialized, interaction, workflow).
Call them natively; results return directly.

Safety: destructive actions (delete, force-push, rm -rf, reset --hard) require user approval.
Run `git status` before discarding uncommitted work.

Workflow: a separate task tracker maintains the visible checklist — its current state is
shown in a read-only system message; never edit it yourself, just work one step at a time.
Delegate only independent parallel work via `workflow(agent)` — never sequential. Info needed
AFTER your current step? Schedule a child now; it gathers it in parallel. Hand off to the user
only after all children complete. `workflow(finish)` ends the autonomous loop when stop
criteria are met.

Plan first, then batch: before your first tool call, state a one-line plan naming every file you
expect to read or write this step. If the plan names multiple reads (for example A, B, and C),
read all of them in ONE tool call using `paths` or `batch`; do not emit separate sequential read
calls. The same rule applies to writes, edits, lists, searches, commands, downloads, and every
other operation that supports independent batching. Prefer one batch call over many calls: results
are returned together in item order and rendered like the equivalent individual operations. Only
split calls when a later operation genuinely depends on an earlier result. Files already read this
session are served from cache; do not re-read them unless a write or edit changed them.

Report: lead with outcome; cite path:line_number. Keep updates brief.{}
"#,
        cwd.display(),
        agents_block
    );
    *cache = Some((cwd.to_path_buf(), agents_block, prompt.clone()));
    prompt
}

/// The JSON schema descriptions for tool calls (OpenAI function-calling format).
/// Lean, single-purpose set (12 tools). Descriptions carry the output-structure
/// expectations so the model formats results consistently (mirrored in the prompt).
fn operation_schemas() -> serde_json::Value {
    // TODO(audit): generate schemas from a typed argument model shared with
    // `editable_arg_keys` and executor parsing to prevent argument drift.
    // One entry: name, description, and (property, is-required, prop-description) rows.
    fn f(name: &str, desc: &str, props: &[(&str, bool, &str)]) -> serde_json::Value {
        let mut properties = serde_json::Map::new();
        let mut required: Vec<serde_json::Value> = Vec::new();
        for (key, req, pdesc) in props {
            properties.insert(
                key.to_string(),
                serde_json::json!({ "type": "string", "description": pdesc }),
            );
            if *req {
                required.push(serde_json::Value::String(key.to_string()));
            }
        }
        serde_json::json!({
            "type": "function",
            "function": {
                "name": name,
                "description": desc,
                "parameters": { "type": "object", "properties": properties, "required": required }
            }
        })
    }

    let mut schemas = serde_json::json!([
        f("read", "Read one or many files. When multiple files are planned, use paths or batch in this ONE call rather than separate read calls. Omit offset/limit for whole files; large files return paging guidance.", &[
            ("path", true, "File path, relative to cwd or absolute"),
            ("offset", false, "1-based first line to read (optional)"),
            ("limit", false, "Number of lines from offset (optional)"),
        ]),
        f("write", "Create or OVERWRITE a whole file (parent dirs auto-created). Prefer `edit` for changing an existing file.", &[
            ("path", true, "File path"),
            ("content", true, "Full file contents to write"),
        ]),
        f("edit", "Replace an exact, unique snippet in a file. REQUIRED: always pass path, old, and new; never call edit with an empty args object or without path. `old` must match verbatim and be unique. Include surrounding lines/indentation so it identifies one occurrence; avoid tiny repeated snippets. If edit reports multiple matches, read the file and retry with a larger unique block. Read the file first.", &[
            ("path", true, "File path"),
            ("old", true, "Exact existing text to replace; must be verbatim and unique in the file, including enough surrounding context to disambiguate"),
            ("new", true, "Replacement text"),
        ]),
        f("list", "List a directory. depth>1 descends as an indented tree (skips .hidden, target, node_modules).", &[
            ("path", true, "Directory path (\".\" for cwd)"),
            ("depth", false, "Levels to descend; 1 (default) = just this dir"),
        ]),
        f("search", "Search file contents for a regex (ripgrep, .gitignore-aware; literal-substring fallback). Returns file:line: match. Optional glob narrows files; offset/limit page through broad result sets.", &[
            ("pattern", true, "Regex (ripgrep) or literal substring (fallback)"),
            ("path", false, "Directory to search (default \".\")"),
            ("glob", false, "File glob, e.g. \"*.rs\" (ripgrep only)"),
            ("offset", false, "1-based first result to show (optional)"),
            ("limit", false, "Number of results to show from offset (optional, default 200, max 1000)"),
        ]),
        f("mkdir", "Create a directory and any missing parent directories.", &[
            ("path", true, "Directory path to create"),
        ]),
        f("shell", "Run one or many BUILD/TEST/RUN commands. Use commands or batch for independent commands in one call. Never use shell to read or edit files — use file operations.", &[
            ("command", false, "One shell command; omit when using commands or batch"),
        ]),
        f("move", "Move or rename a file or directory.", &[
            ("from", true, "Source path"),
            ("to", true, "Destination path"),
        ]),
        f("copy", "Copy a file or directory (recursive).", &[
            ("from", true, "Source path"),
            ("to", true, "Destination path"),
        ]),
        f("delete", "Permanently delete a file or a directory tree. Irreversible — only for paths you created or the user asked to remove.", &[
            ("path", true, "File or directory path to remove"),
        ]),
        f("web_search", "Search the web; returns titled results with URLs. When you use a result, cite it to the user as a markdown link [title](url).", &[
            ("query", true, "Search query in plain words"),
        ]),
        f("web_images", "Search Wikimedia Commons for reusable images. Returns preview and original image URLs plus source-page, description, creator, and license metadata. Review the source and license before downloading or reusing an image.", &[
            ("query", true, "Visual subject, style, feature, building, or location to find"),
        ]),
        f("reverse_image", "Reverse-search an image URL or local file through Google Lens. Returns visually similar images and matching source-page links. Pass exactly one of url or path.", &[
            ("url", false, "Public http(s) image URL"),
            ("path", false, "Local image file path, relative to cwd or absolute"),
        ]),
        f("web_fetch", "Fetch the readable text of a page. Cite the page as a markdown link when you use its content.", &[
            ("url", true, "https URL to fetch"),
        ]),
        f("download", "Download a URL to a local file (images, assets).", &[
            ("url", true, "URL to download"),
            ("path", true, "Local destination path"),
        ]),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "powerpoint",
                "description": "Generate a validated animated .pptx deck from structured slides. Supports positioned text, images, and basic shapes; fixed entrance/exit animations; and fade/push/wipe transitions.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "output_path": {
                            "type": "string",
                            "description": "Destination .pptx path, relative to cwd or absolute"
                        },
                        "slides": {
                            "type": "array",
                            "description": "Ordered slide objects. Each slide accepts elements, animations, and transition. Elements require id/type/x/y/width/height and may include text, image_path, shape_type, fill_color, text_color, font_size. Animation objects require type/target/order and may include duration_ms, delay_ms, trigger. Supported transitions: fade, push_left, wipe_left.",
                            "items": { "type": "object", "additionalProperties": true }
                        }
                    },
                    "required": ["output_path", "slides"]
                }
            }
        }),
        // `todo` takes an array param, so it's built directly rather than via `f`.
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "todo",
                "description": "Set/replace the task breakdown shown in the sticky panel above the input. For a multi-part or long task, call this first with every section as an item, then call again at every task boundary. Mark each individual item done immediately when it finishes, before unrelated work and before starting/finishing another item. Always send the FULL list each time (it replaces the old one).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "items": {
                            "type": "array",
                            "description": "The full ordered task list.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "text": { "type": "string", "description": "Short task description" },
                                    "status": { "type": "string", "enum": ["pending", "in_progress", "done"] }
                                },
                                "required": ["text"]
                            }
                        }
                    },
                    "required": ["items"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "ask",
                "description": "Ask the user for required information, clarification, or a decision. If options are omitted or empty, the user types a free-form answer. If options are provided, the user chooses one or more labels; set multi true to allow multiple choices. Use when requirements are ambiguous, missing data blocks progress, or the user must decide.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "question": { "type": "string", "description": "Question shown to the user" },
                        "options": {
                            "type": "array",
                            "description": "Optional labels to choose from; omit or leave empty for free-form text input",
                            "items": { "type": "string" }
                        },
                        "multi": { "type": "boolean", "description": "Whether multiple options may be selected when options are provided" }
                    },
                    "required": ["question"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "plan",
                "description": "Write a markdown plan to a file and ask the user to edit/approve it before continuing.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Plan file path, relative to cwd or absolute" },
                        "body": { "type": "string", "description": "Markdown plan contents" }
                    },
                    "required": ["path", "body"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "propose_step",
                "description": "Present one workflow step only when it has multiple viable paths requiring user preference. Skip this tool when one path is obvious. Each option should include enough explanation for an informed choice; user may edit it or enter a custom response.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string", "description": "Short step title" },
                        "description": { "type": "string", "description": "What this step achieves and why it is next" },
                        "alternatives": {
                            "type": "array",
                            "description": "Two or more paths for this step",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "label": { "type": "string", "description": "Short option label" },
                                    "description": { "type": "string", "description": "Full path details" },
                                    "feasibility": { "type": "string", "enum": ["possible", "limited", "impossible"] },
                                    "actions": {
                                        "type": "array",
                                        "description": "Optional concise action summaries planned for this path",
                                        "items": { "type": "string" }
                                    },
                                    "tool_kinds": {
                                        "type": "array",
                                        "description": "Operational tool names this path may require",
                                        "items": { "type": "string" }
                                    }
                                },
                                "required": ["label", "description", "feasibility"]
                            }
                        }
                    },
                    "required": ["title", "description", "alternatives"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "agent",
                "description": "Delegate an independent branch to a parallel child agent. Consecutive agent calls launch concurrently; the app waits for the whole batch. Never delegate sequential work — do that yourself. Info needed AFTER your current task? Schedule it now to gather in parallel. Give complete scope, constraints, evidence expectations, and final report shape. Children may launch their own bounded agents.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "description": { "type": "string", "description": "Short user-visible label for this child agent" },
                        "agent": { "type": "string", "description": "Optional name of a configured agent (from the [agents] config): use it when the configured description fits the task, so the child gets its role, model, and tool policy. Otherwise omit and describe the role inline in the prompt." },
                        "prompt": { "type": "string", "description": "Detailed instructions for the child agent, including scope, constraints, and expected final report" },
                        "task_index": { "type": "integer", "minimum": 1, "description": "Optional one-based index of the main checklist subtask this child agent owns" },
                        "checks": {
                            "type": "array",
                            "description": "Important explicit success criteria. When provided, AiTUI runs isolated replicas and reconciles evidence before returning the report.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string", "description": "Stable short identifier used for voting" },
                                    "question": { "type": "string", "description": "Concrete fact or success criterion to verify" }
                                },
                                "required": ["id", "question"]
                            }
                        },
                        "verification": {
                            "type": "string",
                            "enum": ["none", "replicate"],
                            "description": "Verification policy. replicate runs two isolated agents and escalates disagreement to a third replica and independent verifier only when needed."
                        },
                        "cwd": { "type": "string", "description": "Optional working directory for this child agent, relative to current cwd or absolute" }
                    },
                    "required": ["description", "prompt"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "finish",
                "description": "Autonomous loop only: END the loop when stop criteria are verifiably met, or when blocked. Never call prematurely or in normal chat.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "summary": { "type": "string", "description": "Brief summary of what was accomplished, or why you're stopping" }
                    },
                    "required": ["summary"]
                }
            }
        }),
    ]);

    if let Some(operations) = schemas.as_array_mut() {
        for operation in operations {
            let Some(name) = operation["function"]["name"].as_str().map(str::to_string) else {
                continue;
            };
            if !matches!(
                name.as_str(),
                "read"
                    | "write"
                    | "edit"
                    | "list"
                    | "search"
                    | "mkdir"
                    | "shell"
                    | "move"
                    | "copy"
                    | "delete"
                    | "web_search"
                    | "web_images"
                    | "reverse_image"
                    | "web_fetch"
                    | "download"
            ) {
                continue;
            }
            let Some(properties) =
                operation["function"]["parameters"]["properties"].as_object_mut()
            else {
                continue;
            };
            properties.insert(
                "batch".into(),
                serde_json::json!({
                    "type": "array",
                    "description": "Multiple independent invocations of this operation, executed in order in this single tool call. Each item is an object containing that invocation's normal arguments. Prefer this whenever the plan names multiple operations.",
                    "items": { "type": "object", "additionalProperties": true }
                }),
            );
            if matches!(name.as_str(), "read" | "list" | "mkdir" | "delete") {
                properties.insert(
                    "paths".into(),
                    serde_json::json!({
                        "type": "array",
                        "description": "Convenience batch of paths using the shared top-level options. For multiple reads, pass every planned file here in one call.",
                        "items": { "type": "string" }
                    }),
                );
            }
            if name == "shell" {
                properties.insert(
                    "commands".into(),
                    serde_json::json!({
                        "type": "array",
                        "description": "Independent BUILD/TEST/RUN commands to execute in order in this one tool call.",
                        "items": { "type": "string" }
                    }),
                );
            }
        }
    }
    schemas
}

/// Six model-visible category tools. Each category carries an `action` enum and
/// the union of its action arguments; `ToolCall::kind` resolves the selected action
/// back to the operation-level type used by permissions, execution, and rendering.
pub fn tool_schemas() -> serde_json::Value {
    static CACHE: OnceLock<serde_json::Value> = OnceLock::new();
    CACHE.get_or_init(|| build_tool_schemas(false)).clone()
}

/// Model-visible schemas for a main-agent turn. `finish` is only selectable
/// while that session is actively running an autonomous loop.
pub fn tool_schemas_for_loop(loop_active: bool) -> serde_json::Value {
    if !loop_active {
        return tool_schemas();
    }
    build_tool_schemas(true)
}

fn build_tool_schemas(loop_active: bool) -> serde_json::Value {
    fn category(
        operations: &[serde_json::Value],
        name: &str,
        description: &str,
        actions: &[(&str, &str)],
    ) -> serde_json::Value {
        let mut properties = serde_json::Map::new();
        let action_names: Vec<&str> = actions.iter().map(|(action, _)| *action).collect();
        properties.insert(
            "action".into(),
            serde_json::json!({
                "type": "string",
                "enum": action_names,
                "description": "Operation to perform within this category"
            }),
        );
        for (_, operation_name) in actions {
            let Some(schema) = operations
                .iter()
                .find(|schema| schema["function"]["name"].as_str() == Some(operation_name))
            else {
                continue;
            };
            let Some(operation_properties) =
                schema["function"]["parameters"]["properties"].as_object()
            else {
                continue;
            };
            for (key, value) in operation_properties {
                properties
                    .entry(key.clone())
                    .or_insert_with(|| value.clone());
            }
        }
        serde_json::json!({
            "type": "function",
            "function": {
                "name": name,
                "description": description,
                "parameters": {
                    "type": "object",
                    "properties": properties,
                    "required": ["action"]
                }
            }
        })
    }

    let operations = operation_schemas();
    let operations = operations.as_array().cloned().unwrap_or_default();
    let mut shell = operations
        .iter()
        .find(|schema| schema["function"]["name"].as_str() == Some("shell"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    shell["function"]["description"] = serde_json::json!(
        "Run BUILD/TEST/RUN commands only. Batch independent commands in ONE call using commands or batch; results return together in command order. Never use shell to read or edit files."
    );

    let mut schemas = serde_json::json!([
        category(
            &operations,
            "file_management",
            "Local file and directory operations. Choose action first. Batch independent work in ONE call: use paths for multiple reads/lists where applicable, or batch for multiple reads, writes, edits, searches, mkdirs, moves, copies, or deletes. Prefer batching every file named together in the plan; results return together in item order. Use edit for surgical changes and write for new files or intentional whole-file replacement.",
            &[
                ("read", "read"),
                ("write", "write"),
                ("edit", "edit"),
                ("list", "list"),
                ("search", "search"),
                ("mkdir", "mkdir"),
                ("move", "move"),
                ("copy", "copy"),
                ("delete", "delete"),
            ],
        ),
        shell,
        category(
            &operations,
            "web",
            "Web research and downloads. Choose action first: search finds current text sources, images finds reusable Wikimedia Commons assets and metadata, reverse_image finds visually similar images and source links from an image URL or local image, fetch reads one page, and download saves a direct asset URL. Cite sources used in the answer.",
            &[
                ("search", "web_search"),
                ("images", "web_images"),
                ("reverse_image", "reverse_image"),
                ("fetch", "web_fetch"),
                ("download", "download"),
            ],
        ),
        category(
            &operations,
            "specialized",
            "Specialized artifact-generation tools. Choose powerpoint to create a validated animated .pptx deck directly from a structured slide specification.",
            &[("powerpoint", "powerpoint")],
        ),
        category(
            &operations,
            "interaction",
            "User interaction that requires structured UI. Choose ask for missing information, propose only for genuine alternative paths, or plan for a reviewable markdown plan.",
            &[("ask", "ask"), ("propose", "propose_step"), ("plan", "plan")],
        ),
        category(
            &operations,
            "workflow",
            "Task-state controls. The visible task checklist is maintained by a separate tracker and shown to you read-only — never edit it yourself. Choose agent to launch a focused parallel child agent, or finish only to end an autonomous loop whose stop criteria are met or blocked.",
            &[("todo", "todo"), ("agent", "agent"), ("finish", "finish")],
        ),
    ]);
    // The `todo` operation stays in the schema properties (child agents still
    // track their own local progress with it), but the main agent's task
    // checklist is maintained by a separate tracker — the active agent only
    // sees it and must not edit it.
    if let Some(schemas) = schemas.as_array_mut() {
        if let Some(workflow) = schemas
            .iter_mut()
            .find(|schema| schema["function"]["name"].as_str() == Some("workflow"))
        {
            if let Some(properties) =
                workflow["function"]["parameters"]["properties"]["action"].as_object_mut()
            {
                properties.insert(
                    "enum".into(),
                    serde_json::json!(if loop_active {
                        vec!["agent", "finish"]
                    } else {
                        vec!["agent"]
                    }),
                );
            }
        }
    }
    schemas
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            name: name.into(),
            args,
            id: None,
        }
    }

    #[test]
    fn legacy_names_alias_onto_operation_kinds() {
        assert_eq!(ToolKind::from_name("read_file"), Some(ToolKind::Read));
        assert_eq!(ToolKind::from_name("run_shell"), Some(ToolKind::Shell));
        assert_eq!(ToolKind::from_name("delete_file"), Some(ToolKind::Delete));
        assert_eq!(ToolKind::from_name("delete_dir"), Some(ToolKind::Delete));
        assert_eq!(ToolKind::from_name("make_dir"), Some(ToolKind::MakeDir));
        assert_eq!(ToolKind::from_name("append_file"), None);
        assert_eq!(ToolKind::from_name("complete_step"), None);
        assert_eq!(ToolKind::from_name("bogus"), None);
    }

    #[test]
    fn categorized_calls_resolve_to_operation_kinds() {
        assert_eq!(
            call(
                "file_management",
                serde_json::json!({"action": "edit", "path": "a.rs"})
            )
            .kind(),
            Some(ToolKind::Edit)
        );
        assert_eq!(
            call("web", serde_json::json!({"action": "fetch"})).kind(),
            Some(ToolKind::WebFetch)
        );
        assert_eq!(
            call(
                "web",
                serde_json::json!({"action": "images", "query": "Victorian house"})
            )
            .kind(),
            Some(ToolKind::WebImages)
        );
        assert_eq!(
            call(
                "web",
                serde_json::json!({"action": "reverse_image", "url": "https://example.com/a.jpg"})
            )
            .kind(),
            Some(ToolKind::ReverseImage)
        );
        assert_eq!(
            call(
                "specialized",
                serde_json::json!({"action": "powerpoint", "output_path": "deck.pptx", "slides": []})
            )
            .kind(),
            Some(ToolKind::PowerPoint)
        );
        assert_eq!(
            call("interaction", serde_json::json!({"action": "propose"})).kind(),
            Some(ToolKind::ProposeStep)
        );
        assert_eq!(
            call("workflow", serde_json::json!({"action": "propose"})).kind(),
            Some(ToolKind::ProposeStep)
        );
        assert_eq!(
            call("workflow", serde_json::json!({"action": "todo"})).kind(),
            Some(ToolKind::Todo)
        );
        assert_eq!(
            call(
                "workflow",
                serde_json::json!({"action": "agent", "description": "scan", "prompt": "look"})
            )
            .kind(),
            Some(ToolKind::Task)
        );
        assert_eq!(
            call("workflow", serde_json::json!({"action": "task"})).kind(),
            Some(ToolKind::Task)
        );
        assert_eq!(
            call("file_management", serde_json::json!({"action": "append"})).kind(),
            None
        );
    }

    #[test]
    fn summary_is_function_call_style() {
        assert_eq!(
            call("read", serde_json::json!({"path": "a.rs"})).summary(),
            "read(a.rs)"
        );
        assert_eq!(
            call("delete", serde_json::json!({"path": "x"})).summary(),
            "delete(x)"
        );
        assert_eq!(
            call("move", serde_json::json!({"from": "a", "to": "b"})).summary(),
            "move(a → b)"
        );
        // Legacy name still yields the canonical function-style summary.
        assert_eq!(
            call("edit_file", serde_json::json!({"path": "a.rs"})).summary(),
            "edit(a.rs)"
        );
    }

    #[test]
    fn schemas_expose_specialized_powerpoint_category() {
        let schemas = tool_schemas();
        let names: Vec<&str> = schemas
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "file_management",
                "shell",
                "web",
                "specialized",
                "interaction",
                "workflow"
            ]
        );

        let file = &schemas[0]["function"]["parameters"]["properties"]["action"]["enum"];
        assert_eq!(
            file,
            &serde_json::json!([
                "read", "write", "edit", "list", "search", "mkdir", "move", "copy", "delete"
            ])
        );
        let web = &schemas[2]["function"]["parameters"]["properties"]["action"]["enum"];
        assert_eq!(
            web,
            &serde_json::json!(["search", "images", "reverse_image", "fetch", "download"])
        );
        let specialized = &schemas[3]["function"]["parameters"]["properties"]["action"]["enum"];
        assert_eq!(specialized, &serde_json::json!(["powerpoint"]));
        let workflow = &schemas[5]["function"]["parameters"]["properties"]["action"]["enum"];
        assert_eq!(workflow, &serde_json::json!(["agent"]));
        let loop_schemas = tool_schemas_for_loop(true);
        let loop_workflow =
            &loop_schemas[5]["function"]["parameters"]["properties"]["action"]["enum"];
        assert_eq!(loop_workflow, &serde_json::json!(["agent", "finish"]));
        assert!(
            schemas[5].to_string().contains("\"items\""),
            "todo args stay in the schema for child agents"
        );
        assert!(!schemas.to_string().contains("complete_step"));
        assert!(!schemas.to_string().contains("append_file"));
    }

    #[test]
    fn every_advertised_category_action_routes_to_an_executable_kind() {
        for schemas in [tool_schemas(), tool_schemas_for_loop(true)] {
            for schema in schemas.as_array().unwrap() {
                let name = schema["function"]["name"].as_str().unwrap();
                if name == "shell" {
                    assert_eq!(
                        call(name, serde_json::json!({"command": "cargo test"})).kind(),
                        Some(ToolKind::Shell)
                    );
                    continue;
                }
                for action in schema["function"]["parameters"]["properties"]["action"]["enum"]
                    .as_array()
                    .unwrap()
                {
                    let routed = call(name, serde_json::json!({"action": action}));
                    assert!(
                        routed.kind().is_some(),
                        "advertised action has no executor route: {name}/{action}"
                    );
                }
            }
        }
    }

    #[test]
    fn agent_prompt_describes_adaptive_steps_and_editable_choices() {
        let prompt = agent_system_prompt(Path::new("/tmp/project"), &Default::default());
        assert!(prompt.contains("CWD: /tmp/project"));
        assert!(prompt.contains("6 categories"));
        assert!(prompt.contains("Safety"));
        assert!(prompt.contains("Workflow"));
        assert!(prompt.contains("Report"));
        assert!(prompt.contains("destructive"));
        assert!(prompt.contains("parallel"));
        assert!(prompt.contains("sequential"));
        assert!(prompt.contains("AFTER your current step"));
        assert!(prompt.contains("destructive"));
        assert!(!prompt.contains("Configured child agents"));
    }

    #[test]
    fn agent_prompt_lists_configured_agents_with_descriptions_and_models() {
        use std::collections::BTreeMap;
        let mut agents = BTreeMap::new();
        agents.insert(
            "reviewer".to_string(),
            crate::config::types::AgentDef {
                description: "Peer-review code for correctness".to_string(),
                model: Some("fast-model".to_string()),
                role: "a meticulous reviewer".to_string(),
                tools: vec!["read".to_string(), "search".to_string()],
                deny: vec![],
            },
        );
        agents.insert(
            "tester".to_string(),
            crate::config::types::AgentDef {
                description: "Run the test suite".to_string(),
                model: None,
                role: String::new(),
                tools: vec![],
                deny: vec!["write".to_string()],
            },
        );
        let prompt = agent_system_prompt(Path::new("/tmp/project"), &agents);
        assert!(prompt.contains("Configured child agents"));
        assert!(prompt.contains("- reviewer: Peer-review code for correctness (model: fast-model)"));
        assert!(prompt.contains("- tester: Run the test suite"));
        let empty = agent_system_prompt(Path::new("/tmp/project"), &Default::default());
        assert!(!empty.contains("Configured child agents"));
    }

    #[test]
    fn schemas_reinforce_edit_uniqueness_and_todo_boundaries() {
        let schemas = tool_schemas();
        let file = &schemas[0];
        let file_desc = file["function"]["description"].as_str().unwrap();
        assert!(file_desc.contains("surgical changes"));
        let old_desc = file["function"]["parameters"]["properties"]["old"]["description"]
            .as_str()
            .unwrap();
        assert!(old_desc.contains("disambiguate"));

        let interaction = &schemas[4];
        let interaction_desc = interaction["function"]["description"].as_str().unwrap();
        assert!(interaction_desc.contains("genuine alternative paths"));

        let workflow = &schemas[5];
        let workflow_desc = workflow["function"]["description"].as_str().unwrap();
        assert!(workflow_desc.contains("tracker"));
        assert!(workflow_desc.contains("parallel child agent"));
        assert!(workflow_desc.contains("stop criteria"));
    }

    #[test]
    fn custom_rule_tool_and_directory_filters_form_an_and_condition() {
        let cwd = std::env::temp_dir().join("aitui_permission_filter_matrix");
        let src = cwd.join("src");
        let nested = src.join("nested");
        let other = cwd.join("other");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let src = std::fs::canonicalize(src).unwrap();

        for decision in [PermissionDecision::Allow, PermissionDecision::Deny] {
            for include_children in [false, true] {
                let draft = PermissionRuleDraft {
                    decision,
                    kind: Some(ToolKind::Read),
                    directory: Some(src.clone()),
                    include_children,
                    lifetime: PermissionLifetime::Session,
                };
                let direct = call("read", serde_json::json!({"path": "src/a.rs"}));
                let child = call("read", serde_json::json!({"path": "src/nested/a.rs"}));
                let wrong_kind = call(
                    "write",
                    serde_json::json!({"path": "src/a.rs", "content": "x"}),
                );
                let wrong_directory = call("read", serde_json::json!({"path": "other/a.rs"}));

                assert!(draft.matches(&direct, &cwd));
                assert_eq!(draft.matches(&child, &cwd), include_children);
                assert!(!draft.matches(&wrong_kind, &cwd));
                assert!(!draft.matches(&wrong_directory, &cwd));
            }
        }
    }

    #[test]
    fn directory_rules_cover_every_move_copy_endpoint_and_download_target() {
        let cwd = std::env::temp_dir().join("aitui_permission_multi_path");
        let allowed = cwd.join("allowed");
        let nested = allowed.join("nested");
        let outside = cwd.join("outside");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let allowed = std::fs::canonicalize(allowed).unwrap();
        let draft = |kind| PermissionRuleDraft {
            decision: PermissionDecision::Allow,
            kind: Some(kind),
            directory: Some(allowed.clone()),
            include_children: true,
            lifetime: PermissionLifetime::Session,
        };

        for kind in [ToolKind::Move, ToolKind::Copy] {
            let name = kind.name();
            let inside = call(
                name,
                serde_json::json!({"from": "allowed/a.txt", "to": "allowed/nested/b.txt"}),
            );
            let escaping = call(
                name,
                serde_json::json!({"from": "allowed/a.txt", "to": "outside/b.txt"}),
            );
            assert!(draft(kind).matches(&inside, &cwd));
            assert!(!draft(kind).matches(&escaping, &cwd));
        }

        let download = draft(ToolKind::Download);
        assert!(download.matches(
            &call(
                "download",
                serde_json::json!({"url": "https://example.com/a", "path": "allowed/a.bin"}),
            ),
            &cwd,
        ));
        assert!(!download.matches(
            &call(
                "download",
                serde_json::json!({"url": "https://example.com/a", "path": "outside/a.bin"}),
            ),
            &cwd,
        ));
    }

    #[test]
    fn once_rules_are_not_remembered_and_session_rules_do_not_expire() {
        let mut mem = PermissionMemory::default();
        let once = PermissionRuleDraft {
            decision: PermissionDecision::Allow,
            kind: Some(ToolKind::Read),
            directory: None,
            include_children: false,
            lifetime: PermissionLifetime::Once,
        };
        mem.remember_custom_rule(once);
        assert!(mem.rules.is_empty());

        mem.remember_custom_rule(PermissionRuleDraft {
            lifetime: PermissionLifetime::Session,
            ..PermissionRuleDraft {
                decision: PermissionDecision::Allow,
                kind: Some(ToolKind::Read),
                directory: None,
                include_children: false,
                lifetime: PermissionLifetime::Once,
            }
        });
        assert_eq!(mem.rules.len(), 1);
        assert!(mem.rules[0].expires_at.is_none());
        assert!(mem.rules[0].remaining_matching.is_none());
        assert!(mem.rules[0].remaining_general.is_none());
    }

    #[test]
    fn allow_and_deny_precedence_is_stable_across_legacy_and_custom_rules() {
        let cwd = PathBuf::from("/tmp/project");
        let shell = call("shell", serde_json::json!({"command": "cargo test"}));

        let mut mem = PermissionMemory::default();
        mem.remember_allow(ToolKind::Shell);
        mem.remember_custom_rule(PermissionRuleDraft {
            decision: PermissionDecision::Deny,
            kind: Some(ToolKind::Shell),
            directory: None,
            include_children: false,
            lifetime: PermissionLifetime::Session,
        });
        assert_eq!(mem.check(&shell, &cwd), Some(PermissionDecision::Deny));

        let mut mem = PermissionMemory::default();
        mem.remember_deny(ToolKind::Shell);
        mem.remember_custom_rule(PermissionRuleDraft {
            decision: PermissionDecision::Allow,
            kind: Some(ToolKind::Shell),
            directory: None,
            include_children: false,
            lifetime: PermissionLifetime::Session,
        });
        assert_eq!(mem.check(&shell, &cwd), Some(PermissionDecision::Deny));
    }

    #[test]
    fn expanded_calls_normalize_commands_and_edit_batches() {
        let shell = call(
            "shell",
            serde_json::json!({"commands": ["cargo test", "cargo clippy"]}),
        );
        let commands = shell.expanded_calls().unwrap().unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].get_arg("command"), Some("cargo test"));
        assert_eq!(commands[1].get_arg("command"), Some("cargo clippy"));

        let edit = call(
            "file_management",
            serde_json::json!({
                "action": "edit",
                "batch": [
                    {"path": "a.rs", "old": "old a", "new": "new a"},
                    {"path": "b.rs", "old": "old b", "new": "new b"}
                ]
            }),
        );
        let edits = edit.expanded_calls().unwrap().unwrap();
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].get_arg("path"), Some("a.rs"));
        assert_eq!(edits[0].get_arg("old"), Some("old a"));
        assert_eq!(edits[0].get_arg("new"), Some("new a"));
        assert_eq!(edits[1].get_arg("path"), Some("b.rs"));
    }

    #[test]
    fn no_rule_means_ask() {
        let mut mem = PermissionMemory::default();
        let c = call("read_file", serde_json::json!({"path": "a.txt"}));
        assert_eq!(mem.check(&c, &PathBuf::from(".")), None);
    }

    #[test]
    fn reads_outside_cwd_detects_escapes() {
        let cwd = PathBuf::from("/home/u/proj");
        let inside = call("read_file", serde_json::json!({"path": "src/main.rs"}));
        assert!(!inside.reads_outside_cwd(&cwd));
        // `..` traversal that climbs out of the project.
        let climb = call("read_file", serde_json::json!({"path": "../../etc/passwd"}));
        assert!(climb.reads_outside_cwd(&cwd));
        // Absolute path outside the tree.
        let abs = call("read_file", serde_json::json!({"path": "/etc/shadow"}));
        assert!(abs.reads_outside_cwd(&cwd));
        // Absolute path inside the tree is fine.
        let abs_in = call(
            "read_file",
            serde_json::json!({"path": "/home/u/proj/a.rs"}),
        );
        assert!(!abs_in.reads_outside_cwd(&cwd));
        // `..` that stays inside after collapsing is fine.
        let bounce = call("read_file", serde_json::json!({"path": "src/../lib.rs"}));
        assert!(!bounce.reads_outside_cwd(&cwd));
    }

    #[test]
    fn auto_approved_reads_are_confined_to_cwd() {
        let cwd = PathBuf::from("/home/u/proj");
        let mut mem = PermissionMemory::default();
        mem.remember_allow(ToolKind::Read); // the auto-approve default

        // In-project read flows without a prompt.
        let inside = call("read_file", serde_json::json!({"path": "src/main.rs"}));
        assert_eq!(mem.check(&inside, &cwd), Some(PermissionDecision::Allow));

        // A read escaping the project still prompts despite the blanket allow.
        let outside = call("read_file", serde_json::json!({"path": "/etc/shadow"}));
        assert_eq!(mem.check(&outside, &cwd), None);

        // An explicit session-wide (Timed) grant DOES cover the out-of-tree read.
        mem.remember_rule(PermissionDecision::Allow, PermissionScope::Timed, false);
        assert_eq!(mem.check(&outside, &cwd), Some(PermissionDecision::Allow));
    }

    #[test]
    fn kind_rule_applies_to_same_tool_any_args() {
        let mut mem = PermissionMemory::default();
        mem.remember_rule(
            PermissionDecision::Allow,
            PermissionScope::Kind(ToolKind::Read),
            false,
        );
        let a = call("read_file", serde_json::json!({"path": "a.txt"}));
        let b = call("read_file", serde_json::json!({"path": "b.txt"}));
        let w = call(
            "write_file",
            serde_json::json!({"path": "a.txt", "content": ""}),
        );
        assert_eq!(
            mem.check(&a, &PathBuf::from(".")),
            Some(PermissionDecision::Allow)
        );
        assert_eq!(
            mem.check(&b, &PathBuf::from(".")),
            Some(PermissionDecision::Allow)
        );
        // A different tool kind is unaffected.
        assert_eq!(mem.check(&w, &PathBuf::from(".")), None);
    }

    #[test]
    fn deny_rule_wins_over_allow() {
        let mut mem = PermissionMemory::default();
        mem.remember_rule(
            PermissionDecision::Allow,
            PermissionScope::Kind(ToolKind::Shell),
            false,
        );
        mem.remember_rule(
            PermissionDecision::Deny,
            PermissionScope::Kind(ToolKind::Shell),
            false,
        );
        let c = call("run_shell", serde_json::json!({"command": "ls"}));
        assert_eq!(
            mem.check(&c, &PathBuf::from(".")),
            Some(PermissionDecision::Deny)
        );
    }

    #[test]
    fn timed_rule_matches_every_tool_then_expires() {
        let mut mem = PermissionMemory::default();
        mem.remember_rule(PermissionDecision::Allow, PermissionScope::Timed, true);
        let c = call("delete_file", serde_json::json!({"path": "x"}));
        assert_eq!(
            mem.check(&c, &PathBuf::from(".")),
            Some(PermissionDecision::Allow)
        );
        // Force expiry: rewrite the rule's timestamp into the past, then prune.
        for r in mem.rules.iter_mut() {
            r.expires_at = Some(0);
        }
        assert_eq!(mem.check(&c, &PathBuf::from(".")), None);
    }

    #[test]
    fn matching_request_limit_is_consumed_only_by_matching_calls() {
        let cwd = PathBuf::from("/tmp/project");
        let mut mem = PermissionMemory::default();
        mem.remember_custom_rule(PermissionRuleDraft {
            decision: PermissionDecision::Allow,
            kind: Some(ToolKind::Shell),
            directory: None,
            include_children: false,
            lifetime: PermissionLifetime::MatchingRequests(2),
        });
        let read = call("read_file", serde_json::json!({"path": "a.txt"}));
        let shell = call("run_shell", serde_json::json!({"command": "cargo test"}));
        assert_eq!(mem.consume(&read, &cwd), None);
        assert_eq!(mem.consume(&shell, &cwd), Some(PermissionDecision::Allow));
        assert_eq!(mem.consume(&shell, &cwd), Some(PermissionDecision::Allow));
        assert_eq!(mem.consume(&shell, &cwd), None);
    }

    #[test]
    fn general_request_limit_counts_unrelated_access_checks() {
        let cwd = PathBuf::from("/tmp/project");
        let mut mem = PermissionMemory::default();
        mem.remember_custom_rule(PermissionRuleDraft {
            decision: PermissionDecision::Allow,
            kind: Some(ToolKind::Shell),
            directory: None,
            include_children: false,
            lifetime: PermissionLifetime::GeneralRequests(2),
        });
        let read = call("read_file", serde_json::json!({"path": "a.txt"}));
        let shell = call("run_shell", serde_json::json!({"command": "cargo test"}));
        assert_eq!(mem.consume(&read, &cwd), None);
        assert_eq!(mem.consume(&shell, &cwd), Some(PermissionDecision::Allow));
        assert_eq!(mem.consume(&shell, &cwd), None);
    }

    #[test]
    fn directory_rule_scopes_to_that_dir() {
        let base = std::env::temp_dir().join(format!("aitui_perm_{}", std::process::id()));
        let nested = base.join("nested");
        let other = std::env::temp_dir().join(format!("aitui_perm_other_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&nested);
        let _ = std::fs::create_dir_all(&other);
        let mut mem = PermissionMemory::default();
        let dir = std::fs::canonicalize(&base).unwrap();
        mem.remember_rule(
            PermissionDecision::Allow,
            PermissionScope::Directory(dir),
            false,
        );

        let inside = call("read_file", serde_json::json!({"path": "f.txt"}));
        assert_eq!(mem.check(&inside, &base), Some(PermissionDecision::Allow));
        assert_eq!(mem.check(&inside, &nested), None);
        let outside = call("read_file", serde_json::json!({"path": "f.txt"}));
        assert_eq!(mem.check(&outside, &other), None);
    }

    #[test]
    fn custom_directory_rule_can_include_children() {
        let base = std::env::temp_dir().join(format!("aitui_perm_children_{}", std::process::id()));
        let nested = base.join("nested");
        let _ = std::fs::create_dir_all(&nested);
        let mut mem = PermissionMemory::default();
        let dir = std::fs::canonicalize(&base).unwrap();
        mem.remember_custom_rule(PermissionRuleDraft {
            decision: PermissionDecision::Allow,
            kind: Some(ToolKind::Read),
            directory: Some(dir),
            include_children: true,
            lifetime: PermissionLifetime::Session,
        });

        let inside = call("read_file", serde_json::json!({"path": "f.txt"}));
        assert_eq!(mem.check(&inside, &nested), Some(PermissionDecision::Allow));
    }
}
