// This module defines the full tool catalogue (names, descriptions, schemas,
// risk levels). Some accessors are part of the complete API but not yet wired
// into the UI, so allow dead code here.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Represents a tool the agent can call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ToolKind {
    ReadFile,
    WriteFile,
    ListDir,
    RunShell,
    SearchFiles,
    AppendFile,
    DeleteFile,
    EditFile,
    MakeDir,
    MovePath,
    CopyPath,
    DeleteDir,
    WebSearch,
    WebFetch,
    DownloadFile,
}

impl ToolKind {
    pub fn name(&self) -> &'static str {
        match self {
            ToolKind::ReadFile    => "read_file",
            ToolKind::WriteFile   => "write_file",
            ToolKind::ListDir     => "list_dir",
            ToolKind::RunShell    => "run_shell",
            ToolKind::SearchFiles => "search_files",
            ToolKind::AppendFile  => "append_file",
            ToolKind::DeleteFile  => "delete_file",
            ToolKind::EditFile    => "edit_file",
            ToolKind::MakeDir     => "make_dir",
            ToolKind::MovePath    => "move_path",
            ToolKind::CopyPath    => "copy_path",
            ToolKind::DeleteDir   => "delete_dir",
            ToolKind::WebSearch   => "web_search",
            ToolKind::WebFetch    => "web_fetch",
            ToolKind::DownloadFile => "download_file",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            ToolKind::ReadFile    => "Read the contents of a file",
            ToolKind::WriteFile   => "Write or overwrite content to a file",
            ToolKind::ListDir     => "List files and directories in a path",
            ToolKind::RunShell    => "Execute a shell command",
            ToolKind::SearchFiles => "Search for text pattern across files",
            ToolKind::AppendFile  => "Append content to an existing file",
            ToolKind::DeleteFile  => "Delete a file permanently",
            ToolKind::EditFile    => "Edit a file by replacing old_string with new_string (structural edit)",
            ToolKind::MakeDir     => "Create a directory (and any missing parents)",
            ToolKind::MovePath    => "Move or rename a file or directory",
            ToolKind::CopyPath    => "Copy a file or directory (recursive)",
            ToolKind::DeleteDir   => "Delete a directory and all its contents",
            ToolKind::WebSearch   => "Search the web and return top results",
            ToolKind::WebFetch    => "Fetch the text content of a URL",
            ToolKind::DownloadFile => "Download a URL to a local file (images, assets, …)",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            ToolKind::ReadFile    => "📖",
            ToolKind::WriteFile   => "✏️",
            ToolKind::ListDir     => "📁",
            ToolKind::RunShell    => "⚡",
            ToolKind::SearchFiles => "🔍",
            ToolKind::AppendFile  => "➕",
            ToolKind::DeleteFile  => "🗑️",
            ToolKind::EditFile    => "🔧",
            ToolKind::MakeDir     => "📂",
            ToolKind::MovePath    => "🚚",
            ToolKind::CopyPath    => "⧉",
            ToolKind::DeleteDir   => "🗑️",
            ToolKind::WebSearch   => "🌐",
            ToolKind::WebFetch    => "🔗",
            ToolKind::DownloadFile => "⬇️",
        }
    }

    /// Risk level: low = auto-approve possible; high = always ask
    pub fn risk(&self) -> ToolRisk {
        match self {
            ToolKind::ReadFile    => ToolRisk::Low,
            ToolKind::ListDir     => ToolRisk::Low,
            ToolKind::SearchFiles => ToolRisk::Low,
            ToolKind::WebSearch   => ToolRisk::Low,
            ToolKind::WebFetch    => ToolRisk::Low,
            ToolKind::WriteFile   => ToolRisk::Medium,
            ToolKind::AppendFile  => ToolRisk::Medium,
            ToolKind::EditFile    => ToolRisk::Medium,
            ToolKind::MakeDir     => ToolRisk::Medium,
            ToolKind::MovePath    => ToolRisk::Medium,
            ToolKind::CopyPath    => ToolRisk::Medium,
            ToolKind::DownloadFile => ToolRisk::Medium,
            ToolKind::DeleteFile  => ToolRisk::High,
            ToolKind::DeleteDir   => ToolRisk::High,
            ToolKind::RunShell    => ToolRisk::High,
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "read_file"    => Some(ToolKind::ReadFile),
            "write_file"   => Some(ToolKind::WriteFile),
            "list_dir"     => Some(ToolKind::ListDir),
            "run_shell"    => Some(ToolKind::RunShell),
            "search_files" => Some(ToolKind::SearchFiles),
            "append_file"  => Some(ToolKind::AppendFile),
            "delete_file"  => Some(ToolKind::DeleteFile),
            "edit_file"    => Some(ToolKind::EditFile),
            "make_dir"     => Some(ToolKind::MakeDir),
            "move_path"    => Some(ToolKind::MovePath),
            "copy_path"    => Some(ToolKind::CopyPath),
            "delete_dir"   => Some(ToolKind::DeleteDir),
            "web_search"   => Some(ToolKind::WebSearch),
            "web_fetch"    => Some(ToolKind::WebFetch),
            "download_file" => Some(ToolKind::DownloadFile),
            _ => None,
        }
    }

    /// All tools, in display order.
    pub fn all() -> Vec<ToolKind> {
        vec![
            ToolKind::ReadFile,
            ToolKind::ListDir,
            ToolKind::SearchFiles,
            ToolKind::EditFile,
            ToolKind::WriteFile,
            ToolKind::AppendFile,
            ToolKind::MakeDir,
            ToolKind::MovePath,
            ToolKind::CopyPath,
            ToolKind::RunShell,
            ToolKind::WebSearch,
            ToolKind::WebFetch,
            ToolKind::DownloadFile,
            ToolKind::DeleteFile,
            ToolKind::DeleteDir,
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
            ToolRisk::Low    => "LOW",
            ToolRisk::Medium => "MEDIUM",
            ToolRisk::High   => "HIGH",
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

    /// Human-readable summary of what this call will do.
    pub fn summary(&self) -> String {
        match self.name.as_str() {
            "read_file" => {
                let path = self.args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                format!("Read  {}", path)
            }
            "write_file" => {
                let path = self.args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                let lines = self.args.get("content").and_then(|v| v.as_str())
                    .map(|s| s.lines().count()).unwrap_or(0);
                format!("Write {} ({} lines)", path, lines)
            }
            "append_file" => {
                let path = self.args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                format!("Append {}", path)
            }
            "edit_file" => {
                let path = self.args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                format!("Edit  {}", path)
            }
            "list_dir" => {
                let path = self.args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                format!("List  {}", path)
            }
            "run_shell" => {
                let cmd = self.args.get("command").and_then(|v| v.as_str()).unwrap_or("?");
                format!("Shell {}", cmd)
            }
            "search_files" => {
                let pat = self.args.get("pattern").and_then(|v| v.as_str()).unwrap_or("?");
                let path = self.args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                format!("Search '{}' in {}", pat, path)
            }
            "delete_file" => {
                let path = self.args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                format!("DELETE {}", path)
            }
            "make_dir" => {
                let path = self.args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                format!("Mkdir {}", path)
            }
            "move_path" => {
                let from = self.args.get("from").and_then(|v| v.as_str()).unwrap_or("?");
                let to = self.args.get("to").and_then(|v| v.as_str()).unwrap_or("?");
                format!("Move  {} → {}", from, to)
            }
            "copy_path" => {
                let from = self.args.get("from").and_then(|v| v.as_str()).unwrap_or("?");
                let to = self.args.get("to").and_then(|v| v.as_str()).unwrap_or("?");
                format!("Copy  {} → {}", from, to)
            }
            "delete_dir" => {
                let path = self.args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                format!("DELETE DIR {}", path)
            }
            "web_search" => {
                let q = self.args.get("query").or_else(|| self.args.get("q"))
                    .and_then(|v| v.as_str()).unwrap_or("?");
                format!("Search web '{}'", q)
            }
            "web_fetch" => {
                let url = self.args.get("url").and_then(|v| v.as_str()).unwrap_or("?");
                format!("Fetch {}", url)
            }
            "download_file" => {
                let url = self.args.get("url").and_then(|v| v.as_str()).unwrap_or("?");
                let path = self.args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                format!("Download {} → {}", url, path)
            }
            _ => format!("{} {:?}", self.name, self.args),
        }
    }
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
        Self { call, output: Ok(output), duration_ms }
    }
    pub fn failure(call: ToolCall, err: String, duration_ms: u64) -> Self {
        Self { call, output: Err(err), duration_ms }
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

/// Permission granted for a tool call.
#[derive(Debug, Clone, PartialEq)]
pub enum Permission {
    Allow,
    AllowAll,   // approve this and all future calls of same kind
    Deny,
    DenyAll,    // block this kind for the rest of the session
}

/// Per-session permission memory: which tool kinds are auto-approved or denied.
#[derive(Debug, Clone, Default)]
pub struct PermissionMemory {
    pub always_allow: Vec<ToolKind>,
    pub always_deny:  Vec<ToolKind>,
}

impl PermissionMemory {
    pub fn check(&self, kind: &ToolKind) -> Option<Permission> {
        if self.always_deny.contains(kind) {
            return Some(Permission::Deny);
        }
        if self.always_allow.contains(kind) {
            return Some(Permission::Allow);
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
}

/// Build the system prompt for agent mode.
pub fn agent_system_prompt(cwd: &PathBuf) -> String {
    format!(
        r#"You are an agentic coding assistant running INSIDE a terminal app that
executes your tool calls directly on this machine. You have REAL, working access
to the local filesystem and shell through the tools below. This is not a sandbox
and not a chat-only session.

Current working directory: {}

## How to use a tool
Emit a fenced JSON block EXACTLY like this (the app parses it and runs it for you):
```tool
{{"name": "list_dir", "args": {{"path": "."}}}}
```
The app runs the tool and feeds the result back to you as a new message. Then you
continue. You may call multiple tools across turns until the task is done.

CRITICAL — do NOT do any of these:
- Do NOT say you "don't have access" to files, the shell, or the internet. You do.
- Do NOT ask the user to paste file contents, directory listings, or command
  output. Call read_file / list_dir / run_shell yourself and wait for the result.
- Do NOT invent tools that aren't listed (there is no "image tool"). Use only the
  tools below, with the exact names and argument keys shown.
- Do NOT print a tool call as an example and then stop — if you want it run, emit
  it as a real ```tool block and nothing after it.

## Tools (exact names + argument keys)
- read_file(path) — Read a file's contents.
- write_file(path, content) — Create or OVERWRITE a whole file (parent dirs auto-created).
- edit_file(path, old_string, new_string) — Replace an exact snippet in a file. PREFERRED for changing existing files.
- append_file(path, content) — Append to a file.
- list_dir(path) — List a directory. Use "." for the current dir.
- search_files(pattern, path) — Search text across files (grep-like).
- make_dir(path) — Create a directory (and parents).
- move_path(from, to) — Move/rename a file or directory.
- copy_path(from, to) — Copy a file or directory (recursive).
- delete_file(path) — Delete one file.  delete_dir(path) — Delete a directory tree.
- run_shell(command) — Run a shell command. Use for BUILDING/TESTING/RUNNING only
  (e.g. "cargo test", "npm run build", "git status"). Do NOT use it to read or
  edit files — use read_file/edit_file/write_file, which are safer and previewable.
- web_search(query) — Search the web. Args: {{"query": "your question in plain words"}}.
  Returns a summary + top links. Use it for anything you're unsure about or that
  may have changed. Follow up with web_fetch(url) to read a specific page.
- web_fetch(url) — Fetch the readable text of a page. Args: {{"url": "https://..."}}.
- download_file(url, path) — Save a URL (e.g. an image) to a local file.

## Rules
1. Act, don't ask. If you need information that a tool can get, call the tool.
2. To CHANGE a file: read_file first, then edit_file (surgical) or write_file (full
   rewrite). Never shell out to sed/echo/cat to edit files — use the file tools.
3. To learn the project: list_dir and read_file; search_files to locate things.
4. After each tool result, briefly reflect, then take the next action or finish.
5. When done, summarize what you changed and why. Keep tool calls purposeful.
"#,
        cwd.display()
    )
}

/// The JSON schema descriptions for tool calls (OpenAI function-calling format).
pub fn tool_schemas() -> serde_json::Value {
    serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read the full contents of a file",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File path (relative to cwd or absolute)"}
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Write or overwrite a file with given content",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path":    {"type": "string"},
                        "content": {"type": "string"}
                    },
                    "required": ["path", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "edit_file",
                "description": "Replace old_string with new_string in a file (structural edit, preserves surrounding code)",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File path"},
                        "old_string": {"type": "string", "description": "The exact existing text to replace"},
                        "new_string": {"type": "string", "description": "The new text to insert in its place"}
                    },
                    "required": ["path", "old_string", "new_string"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "append_file",
                "description": "Append content to an existing file",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path":    {"type": "string"},
                        "content": {"type": "string"}
                    },
                    "required": ["path", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_dir",
                "description": "List directory contents",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"}
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "run_shell",
                "description": "Execute a shell command",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {"type": "string"}
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "search_files",
                "description": "Search for a text pattern in files under a directory",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string"},
                        "path":    {"type": "string"}
                    },
                    "required": ["pattern", "path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "delete_file",
                "description": "Permanently delete a file",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"}
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "make_dir",
                "description": "Create a directory and any missing parents",
                "parameters": {
                    "type": "object",
                    "properties": { "path": {"type": "string"} },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "move_path",
                "description": "Move or rename a file or directory",
                "parameters": {
                    "type": "object",
                    "properties": { "from": {"type": "string"}, "to": {"type": "string"} },
                    "required": ["from", "to"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "copy_path",
                "description": "Copy a file or directory (recursive)",
                "parameters": {
                    "type": "object",
                    "properties": { "from": {"type": "string"}, "to": {"type": "string"} },
                    "required": ["from", "to"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "delete_dir",
                "description": "Delete a directory and all its contents",
                "parameters": {
                    "type": "object",
                    "properties": { "path": {"type": "string"} },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web and return top results",
                "parameters": {
                    "type": "object",
                    "properties": { "query": {"type": "string"} },
                    "required": ["query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "web_fetch",
                "description": "Fetch the readable text content of a URL",
                "parameters": {
                    "type": "object",
                    "properties": { "url": {"type": "string"} },
                    "required": ["url"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "download_file",
                "description": "Download a URL to a local file (images, assets, …)",
                "parameters": {
                    "type": "object",
                    "properties": { "url": {"type": "string"}, "path": {"type": "string"} },
                    "required": ["url", "path"]
                }
            }
        }
    ])
}
