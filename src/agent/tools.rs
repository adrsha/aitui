// This module defines the full tool catalogue (names, descriptions, schemas,
// risk levels). Some accessors are part of the complete API but not yet wired
// into the UI, so allow dead code here.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    Shell,
    Move,
    Copy,
    Delete,
    WebSearch,
    WebFetch,
    Download,
    Todo,
    Ask,
    Plan,
}

impl ToolKind {
    pub fn name(&self) -> &'static str {
        match self {
            ToolKind::Read => "read",
            ToolKind::Write => "write",
            ToolKind::Edit => "edit",
            ToolKind::List => "list",
            ToolKind::Search => "search",
            ToolKind::Shell => "shell",
            ToolKind::Move => "move",
            ToolKind::Copy => "copy",
            ToolKind::Delete => "delete",
            ToolKind::WebSearch => "web_search",
            ToolKind::WebFetch => "web_fetch",
            ToolKind::Download => "download",
            ToolKind::Todo => "todo",
            ToolKind::Ask => "ask",
            ToolKind::Plan => "plan",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            ToolKind::Read => "Read a file's contents (optionally a line window)",
            ToolKind::Write => "Create or overwrite a whole file",
            ToolKind::Edit => "Replace an exact, unique snippet in a file",
            ToolKind::List => "List a directory (optionally as a tree)",
            ToolKind::Search => "Search file contents for a regex pattern",
            ToolKind::Shell => "Run a shell command (build/test/run)",
            ToolKind::Move => "Move or rename a file or directory",
            ToolKind::Copy => "Copy a file or directory (recursive)",
            ToolKind::Delete => "Delete a file or directory tree permanently",
            ToolKind::WebSearch => "Search the web; returns titled results with links",
            ToolKind::WebFetch => "Fetch the readable text of a web page",
            ToolKind::Download => "Download a URL to a local file",
            ToolKind::Todo => "Set the task breakdown shown in the sticky todo panel",
            ToolKind::Ask => "Ask the user to choose from options",
            ToolKind::Plan => "Write a plan file for user review and approval",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            ToolKind::Read => "📖",
            ToolKind::Write => "✎",
            ToolKind::Edit => "✎",
            ToolKind::List => "📁",
            ToolKind::Search => "🔍",
            ToolKind::Shell => "⚡",
            ToolKind::Move => "🚚",
            ToolKind::Copy => "⧉",
            ToolKind::Delete => "🗑",
            ToolKind::WebSearch => "🌐",
            ToolKind::WebFetch => "🔗",
            ToolKind::Download => "⬇",
            ToolKind::Todo => "☑",
            ToolKind::Ask => "?",
            ToolKind::Plan => "📝",
        }
    }

    /// Risk level: low = auto-approve possible; high = always ask
    pub fn risk(&self) -> ToolRisk {
        match self {
            ToolKind::Read => ToolRisk::Low,
            ToolKind::List => ToolRisk::Low,
            ToolKind::Search => ToolRisk::Low,
            ToolKind::WebSearch => ToolRisk::Low,
            ToolKind::WebFetch => ToolRisk::Low,
            ToolKind::Write => ToolRisk::Medium,
            ToolKind::Edit => ToolRisk::Medium,
            ToolKind::Move => ToolRisk::Medium,
            ToolKind::Copy => ToolRisk::Medium,
            ToolKind::Download => ToolRisk::Medium,
            ToolKind::Todo => ToolRisk::Low,
            ToolKind::Ask => ToolRisk::Low,
            ToolKind::Plan => ToolRisk::Low,
            ToolKind::Delete => ToolRisk::High,
            ToolKind::Shell => ToolRisk::High,
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "read" | "read_file" => Some(ToolKind::Read),
            "write" | "write_file" | "append_file" => Some(ToolKind::Write),
            "edit" | "edit_file" => Some(ToolKind::Edit),
            "list" | "list_dir" => Some(ToolKind::List),
            "search" | "search_files" => Some(ToolKind::Search),
            "shell" | "run_shell" => Some(ToolKind::Shell),
            "move" | "move_path" => Some(ToolKind::Move),
            "copy" | "copy_path" => Some(ToolKind::Copy),
            "delete" | "delete_file" | "delete_dir" => Some(ToolKind::Delete),
            "web_search" => Some(ToolKind::WebSearch),
            "web_fetch" => Some(ToolKind::WebFetch),
            "download" | "download_file" => Some(ToolKind::Download),
            "todo" | "todos" | "todo_write" => Some(ToolKind::Todo),
            "ask" | "decide" => Some(ToolKind::Ask),
            "plan" => Some(ToolKind::Plan),
            _ => None,
        }
    }

    /// All tools, in display order.
    pub fn all() -> Vec<ToolKind> {
        vec![
            ToolKind::Read,
            ToolKind::List,
            ToolKind::Search,
            ToolKind::Edit,
            ToolKind::Write,
            ToolKind::Move,
            ToolKind::Copy,
            ToolKind::Shell,
            ToolKind::WebSearch,
            ToolKind::WebFetch,
            ToolKind::Download,
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
        ToolKind::from_name(&self.name)
    }

    /// Directory a call primarily operates in, for directory-scoped permission rules.
    pub fn permission_directory(&self, cwd: &Path) -> Option<PathBuf> {
        let raw = match self.kind() {
            Some(ToolKind::Shell) => Some("."),
            Some(ToolKind::Move) | Some(ToolKind::Copy) => self
                .args
                .get("from")
                .or_else(|| self.args.get("source"))
                .and_then(|v| v.as_str()),
            Some(ToolKind::WebSearch) | Some(ToolKind::WebFetch) | Some(ToolKind::Download) => None,
            _ => self.args.get("path").and_then(|v| v.as_str()),
        }?;
        let p = PathBuf::from(raw);
        let p = if p.is_absolute() { p } else { cwd.join(p) };
        let dir = if p.is_dir() {
            p
        } else {
            p.parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| cwd.to_path_buf())
        };
        Some(std::fs::canonicalize(&dir).unwrap_or(dir))
    }

    /// Whether this call's `path` argument resolves *outside* the project tree
    /// (`cwd`). Used to keep the blanket read auto-approval confined to the project:
    /// a read of `~/.ssh/id_rsa` or `../../etc/passwd` escapes and still prompts.
    ///
    /// Resolution is lexical (`..`/`.` collapsed without touching the filesystem),
    /// so it flags an escape even for a path that doesn't exist yet, and can't be
    /// fooled into a slow/undefined canonicalize on a bogus path.
    pub fn reads_outside_cwd(&self, cwd: &std::path::Path) -> bool {
        let Some(raw) = self.args.get("path").and_then(|v| v.as_str()) else {
            return false;
        };
        let raw_path = std::path::Path::new(raw);
        let joined = if raw_path.is_absolute() {
            raw_path.to_path_buf()
        } else {
            cwd.join(raw_path)
        };
        let target = normalize_lexical(&joined);
        let base = normalize_lexical(cwd);
        !target.starts_with(&base)
    }

    /// Human-readable summary of what this call will do, rendered function-call
    /// style: `name(primary args)`. Reused as the transcript header for the call.
    pub fn summary(&self) -> String {
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
            Some(ToolKind::WebFetch) => format!("web_fetch({})", s("url").unwrap_or("?")),
            Some(ToolKind::Download) => {
                format!("download({} → {})", s("url").unwrap_or("?"), path())
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
            Some(ToolKind::Read) | Some(ToolKind::List) | Some(ToolKind::Delete) => &["path"],
            Some(ToolKind::Write) => &["path", "content"],
            Some(ToolKind::Edit) => &["path", "old", "new"],
            Some(ToolKind::Search) => &["pattern"],
            Some(ToolKind::Move) | Some(ToolKind::Copy) => &["from", "to"],
            Some(ToolKind::WebSearch) => &["query"],
            Some(ToolKind::WebFetch) => &["url"],
            Some(ToolKind::Download) => &["url", "path"],
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

/// Collapse `.` and `..` components lexically (no filesystem access), so
/// `/proj/../etc` becomes `/etc`. Symlinks are not resolved — this is a
/// conservative containment check, and treating a symlink target as "inside" only
/// happens if its lexical path is inside, which is the safe direction.
fn normalize_lexical(p: &std::path::Path) -> PathBuf {
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

/// Permission choice made from the tool approval prompt.
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny,
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
    /// Unix timestamp in seconds. `None` means it lasts until the app exits.
    pub expires_at: Option<u64>,
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
}

impl PermissionMemory {
    pub const TIMED_SECS: u64 = 10 * 60;

    pub fn check(&mut self, call: &ToolCall, cwd: &PathBuf) -> Option<PermissionDecision> {
        self.prune_expired();
        let kind = call.kind()?;
        let dir = call.permission_directory(cwd);

        // Deny rules win over allow rules.
        if self.always_deny.contains(&kind)
            || self.rules.iter().any(|r| {
                r.decision == PermissionDecision::Deny && rule_matches(r, &kind, dir.as_ref())
            })
        {
            return Some(PermissionDecision::Deny);
        }
        // The blanket auto-approve for read-family tools (`always_allow`) is confined
        // to the project tree: a read whose target escapes cwd is NOT covered by it,
        // so it still prompts. Explicit scoped grants (a Directory/Timed rule the user
        // chose) below still apply, and Deny above still wins.
        let kind_auto = self.always_allow.contains(&kind)
            && !(is_read_family(kind) && call.reads_outside_cwd(cwd));
        if kind_auto
            || self.rules.iter().any(|r| {
                r.decision == PermissionDecision::Allow && rule_matches(r, &kind, dir.as_ref())
            })
        {
            return Some(PermissionDecision::Allow);
        }
        None
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
            expires_at,
        });
    }

    fn prune_expired(&mut self) {
        let now = now_secs();
        self.rules.retain(|r| r.expires_at.is_none_or(|t| t > now));
    }
}

fn rule_matches(rule: &PermissionRule, kind: &ToolKind, dir: Option<&PathBuf>) -> bool {
    match &rule.scope {
        PermissionScope::Kind(k) => k == kind,
        PermissionScope::Directory(d) => dir.is_some_and(|actual| actual == d),
        PermissionScope::Timed => true,
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Build the system prompt for agent mode.
pub fn agent_system_prompt(cwd: &Path) -> String {
    format!(
        r#"You are an agentic coding assistant running INSIDE a terminal app that
executes your tool calls directly on this machine. You have REAL, working access
to the local filesystem and shell through the tools below. This is not a sandbox
and not a chat-only session.

Current working directory: {}

## How to use a tool
Call a tool the way you call a function — one name, a few arguments. If your
runtime supports native function-calling, just emit the call and the app runs it.
Otherwise, emit a fenced ```tool block with the call as JSON — EXACTLY this shape:
```tool
{{"name": "list", "args": {{"path": "."}}}}
```
The app runs it and feeds the result back as a new message; then you continue. You
may emit several calls in ONE turn (they run as a batch, one round-trip — see "Batch
tool calls" below) and keep calling across turns until the task is done. The user sees a clean
rendering of each call (a diff, a removed-file line, search hits, cited links) —
never the raw arguments — so let the tools do the work and don't paste their guts
into prose. Because that rendering is compact (and sometimes hidden), always follow
tool work with a short plain-text summary for the user: WHAT you did, WHY, and HOW
(e.g. "Sorted ~/Downloads by extension — moved 12 files into Documents/Pictures/
Music using `move`, skipping folders."). One tight paragraph, no raw output dumps.
- You ARE in this terminal with working tools. When the user asks you to inspect,
  organize, move, edit, or delete files, DO IT with the tools — never hand back a
  shell script or PowerShell for the user to run, and never ask them to "enable" or
  "send" tools. The tools below are already active.
- Do NOT say you "don't have access" to files, the shell, or the internet. You do.
- Do NOT ask the user to paste file contents, directory listings, or command
  output — call read / list / shell yourself and wait for the result.
- Do NOT invent tools that aren't listed, or extra arguments. Use only these,
  exactly.

## Tools — signatures and how to use each
Filesystem:
- read(path[, offset, limit]) — Read a file. offset (1-based line) + limit (count)
  read only a window of a big file. Read before you edit.
- write(path, content) — Create or OVERWRITE a whole file. Prefer edit for changes
  to an existing file; write is for new files or a full rewrite.
- edit(path, old, new) — Replace `old` with `new`. `old` must be verbatim and
  UNIQUE in the file — include enough surrounding context to pin it down. The user
  sees the change as a diff.
- list(path[, depth]) — List a directory. "." = cwd; depth>1 descends as a tree.
- search(pattern[, path, glob]) — Regex search across files. Results come back as
  `file:line: match`; when you cite a hit to the user, use that file:line form.
- move(from, to) / copy(from, to) — Move/rename or copy a path (copy is recursive).
- delete(path) — Delete a file OR a directory tree. Irreversible: only for paths
  you created this session or the user explicitly asked to remove.

Shell:
- shell(command) — Run a command for BUILD / TEST / RUN only (e.g. "cargo test",
  "git status"). NEVER use it to read or edit files — use read/edit/write, which
  are safer and render as previews.

Web (always cite what you use):
- web_search(query) — Search the web; returns titled results with URLs. Use it for
  anything that may have changed since training. When a result informs your answer,
  cite it to the user as a markdown link `[title](url)` — never state a web fact
  without its source link.
- web_fetch(url) — Fetch a page's readable text. Cite the page as a link when used.
- download(url, path) — Save a URL (e.g. an image) to a local file.

User requests:
- ask(question, options[], multi) — Ask the user to choose option label(s). Use
  this only when the user must decide; tool result is the chosen label(s).
- plan(path, body) — Write a plan markdown file and ask the user to edit/approve
  it. Tool result is `APPROVED\n<contents>` or `DENIED`.

## Communicating with the user
Your text output is what the user reads between tool calls; they can't see your
thinking or the raw tool results. Before your first tool call, say in one sentence
what you're about to do. While working, give brief updates only when you find
something load-bearing or change direction — brief is good, silent is not, and one
sentence per update is almost always enough. Don't narrate internal deliberation.

Lead with the outcome. Your first sentence after finishing should answer "what
happened" — the thing the user would ask for if they said "just give me the TLDR."
Supporting detail comes after. Readable beats terse: write complete sentences with
technical terms spelled out, not fragments or arrow chains. Match the response to
the task — a simple question gets a direct answer, not headers and sections. The
end-of-turn summary is one or two sentences: what changed and what's next.

When referencing specific functions or code, use the pattern file_path:line_number
so the user can navigate straight to the source.

## Doing tasks
The user primarily requests software-engineering tasks: fixing bugs, adding
functionality, refactoring, explaining code. Interpret an unclear instruction in
that context and in the current working directory — if asked to rename a method,
find it in the code and change the code, don't just print the new name.
- Don't add features, refactor, or introduce abstractions beyond what the task
  requires. A bug fix doesn't need surrounding cleanup; three similar lines beat a
  premature abstraction. No half-finished implementations.
- Don't add error handling, fallbacks, or validation for scenarios that can't
  happen. Trust internal code and framework guarantees; validate only at system
  boundaries (user input, external APIs).
- Delete unused code completely rather than leaving compatibility shims, `// removed`
  comments, or renamed `_unused` vars.
- Write code that reads like the surrounding code — match its comment density,
  naming, and idiom. Default to no comments; only note a constraint the code can't
  show, never what the next line does or why the change is correct.

## Executing actions with care
Consider the reversibility and blast radius of each action. Local, reversible
actions (editing files, running tests) you can take freely. For actions that are
hard to reverse, affect shared state, or could be destructive — deleting files or
branches, `rm -rf`, force-pushing, `git reset --hard`, dropping tables, pushing
code, sending messages, posting to external services — confirm with the user
first, unless they have durably authorized it or told you to operate autonomously.
Approval of an action once does not extend it to every later context. Don't reach
for a destructive shortcut to clear an obstacle: find the root cause instead of
bypassing safety checks (no `--no-verify`). In a git repo, run `git status` before
any command that could discard uncommitted work, and stash or commit first. If you
find unexpected files or state, investigate before deleting or overwriting — it may
be the user's in-progress work.

## Tool usage policy
Act, don't ask for things a tool can get: to CHANGE a file, read first, then edit
(surgical) or write (full rewrite) — never shell out to sed/echo/cat. To learn the
project, use list + read and search to locate things. After each tool result,
reflect briefly, then take the next action or finish. Report outcomes faithfully:
if a build or test fails, say so with the output; state verified work plainly.

## Plan first, then gather in one batch
Before a non-trivial task, break it into concrete steps. Front-load everything you
need so you're efficient: gather the questions you must ask the user, the data you
must read (files, search, shell), and the access/permissions you'll need — then get
them together rather than stopping the user once per step. If several genuine
decisions are the user's to make, ask them together, up front, not one at a time.
Distinguish what a tool can answer (just call it) from what only the user can decide
(ask). Don't begin editing until the shape of the work is clear.

## Batch tool calls — one turn, many calls
Every tool call pauses you: the app runs it and feeds the result back before you can
continue. So MINIMIZE round-trips. In a single turn, emit ALL the calls whose inputs
you already know — the app runs the whole batch and returns every result together in
the next turn, instead of one stop-and-go per call. Predict what you'll need and
request it at once.
- Independent reads/searches → one batch. To understand a module, `read` all the
  relevant files and `search` for the symbols in the SAME turn, not one, wait, next.
- Only go sequential when a call's arguments genuinely depend on an earlier call's
  result — e.g. `search` to find a file's path, THEN `read` that path; `read` a file,
  THEN `edit` it with verbatim text you just saw. That dependency is the ONLY reason
  to split a turn. When in doubt whether two calls are independent, assume they are
  and batch them.
- Don't batch destructive or hard-to-reverse calls speculatively (delete, move,
  write over an existing file, shell that mutates) — those still follow the care
  rules above. Batching is for cheap, reversible information-gathering and edits.

## Task tracking — the todo panel
- todo(items) — Set the task breakdown shown in a sticky panel above the user's
  input. `items` is the FULL ordered list; each item is {{text, status}} with status
  one of "pending" | "in_progress" | "done".
For any multi-part or long request, call `todo` FIRST with one item per section, so
the user can see the plan at a glance. As you work, call `todo` again with the same
list and updated statuses — mark exactly one item "in_progress" at a time and flip
each to "done" the moment it's finished. Always send the whole list (it replaces the
old one). Skip the panel for trivial one-step tasks. The todo call itself is silent
in the transcript — only the panel updates.

Use an ASCII diagram when a picture communicates better than prose — data flow, a
tree, state transitions, box-and-arrow architecture. Put it in a fenced code block
so it renders monospaced.
"#,
        cwd.display()
    )
}

/// The JSON schema descriptions for tool calls (OpenAI function-calling format).
/// Lean, single-purpose set (12 tools). Descriptions carry the output-structure
/// expectations so the model formats results consistently (mirrored in the prompt).
pub fn tool_schemas() -> serde_json::Value {
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

    serde_json::json!([
        f("read", "Read a file's contents. Omit offset/limit for the whole file (capped); pass them to read only a line window of a large file.", &[
            ("path", true, "File path, relative to cwd or absolute"),
            ("offset", false, "1-based first line to read (optional)"),
            ("limit", false, "Number of lines from offset (optional)"),
        ]),
        f("write", "Create or OVERWRITE a whole file (parent dirs auto-created). Prefer `edit` for changing an existing file.", &[
            ("path", true, "File path"),
            ("content", true, "Full file contents to write"),
        ]),
        f("edit", "Replace an exact, unique snippet in a file. `old` must match verbatim and be unique — include enough surrounding context. Read the file first.", &[
            ("path", true, "File path"),
            ("old", true, "Exact existing text to replace (unique in the file)"),
            ("new", true, "Replacement text"),
        ]),
        f("list", "List a directory. depth>1 descends as an indented tree (skips .hidden, target, node_modules).", &[
            ("path", true, "Directory path (\".\" for cwd)"),
            ("depth", false, "Levels to descend; 1 (default) = just this dir"),
        ]),
        f("search", "Search file contents for a regex (ripgrep, .gitignore-aware; literal-substring fallback). Returns file:line: match. Optional glob narrows files.", &[
            ("pattern", true, "Regex (ripgrep) or literal substring (fallback)"),
            ("path", false, "Directory to search (default \".\")"),
            ("glob", false, "File glob, e.g. \"*.rs\" (ripgrep only)"),
        ]),
        f("shell", "Run a shell command for BUILD/TEST/RUN only (e.g. cargo test). Never use it to read or edit files — use read/edit/write.", &[
            ("command", true, "Shell command to execute in cwd"),
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
        f("web_fetch", "Fetch the readable text of a page. Cite the page as a markdown link when you use its content.", &[
            ("url", true, "https URL to fetch"),
        ]),
        f("download", "Download a URL to a local file (images, assets).", &[
            ("url", true, "URL to download"),
            ("path", true, "Local destination path"),
        ]),
        // `todo` takes an array param, so it's built directly rather than via `f`.
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "todo",
                "description": "Set/replace the task breakdown shown in the sticky panel above the input. For a multi-part or long task, call this first with every section as an item, then call again to update statuses as you go. Always send the FULL list each time (it replaces the old one).",
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
                "description": "Ask the user to choose from explicit options. Use only when the user must decide; the tool result contains the chosen label(s).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "question": { "type": "string", "description": "Question shown to the user" },
                        "options": {
                            "type": "array",
                            "description": "Option labels to choose from",
                            "items": { "type": "string" }
                        },
                        "multi": { "type": "boolean", "description": "Whether multiple options may be selected" }
                    },
                    "required": ["question", "options"]
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
    ])
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
    fn legacy_names_alias_onto_canonical_kinds() {
        assert_eq!(ToolKind::from_name("read_file"), Some(ToolKind::Read));
        assert_eq!(ToolKind::from_name("read"), Some(ToolKind::Read));
        assert_eq!(ToolKind::from_name("run_shell"), Some(ToolKind::Shell));
        assert_eq!(ToolKind::from_name("delete_file"), Some(ToolKind::Delete));
        assert_eq!(ToolKind::from_name("delete_dir"), Some(ToolKind::Delete));
        assert_eq!(ToolKind::from_name("append_file"), Some(ToolKind::Write));
        assert_eq!(ToolKind::from_name("bogus"), None);
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
    fn schemas_cover_the_tools() {
        let schemas = tool_schemas();
        let names: Vec<&str> = schemas
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["function"]["name"].as_str().unwrap())
            .collect();
        for expected in [
            "read",
            "write",
            "edit",
            "list",
            "search",
            "shell",
            "move",
            "copy",
            "delete",
            "web_search",
            "web_fetch",
            "download",
            "todo",
            "ask",
            "plan",
        ] {
            assert!(names.contains(&expected), "missing schema for {expected}");
        }
        assert_eq!(names.len(), 15);
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
    fn directory_rule_scopes_to_that_dir() {
        let base = std::env::temp_dir().join(format!("aitui_perm_{}", std::process::id()));
        let other = base.join("other");
        let _ = std::fs::create_dir_all(&base);
        let _ = std::fs::create_dir_all(&other);
        let mut mem = PermissionMemory::default();
        let dir = std::fs::canonicalize(&base).unwrap();
        mem.remember_rule(
            PermissionDecision::Allow,
            PermissionScope::Directory(dir),
            false,
        );

        // A file directly in `base` → allowed.
        let inside = call("read_file", serde_json::json!({"path": "f.txt"}));
        assert_eq!(mem.check(&inside, &base), Some(PermissionDecision::Allow));
        // A file in a different directory → still asks.
        let outside = call("read_file", serde_json::json!({"path": "f.txt"}));
        assert_eq!(mem.check(&outside, &other), None);
    }
}
