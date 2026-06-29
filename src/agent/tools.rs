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
        }
    }

    /// Risk level: low = auto-approve possible; high = always ask
    pub fn risk(&self) -> ToolRisk {
        match self {
            ToolKind::ReadFile    => ToolRisk::Low,
            ToolKind::ListDir     => ToolRisk::Low,
            ToolKind::SearchFiles => ToolRisk::Low,
            ToolKind::WriteFile   => ToolRisk::Medium,
            ToolKind::AppendFile  => ToolRisk::Medium,
            ToolKind::EditFile    => ToolRisk::Medium,
            ToolKind::DeleteFile  => ToolRisk::High,
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
            ToolKind::RunShell,
            ToolKind::DeleteFile,
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
        r#"You are an agentic coding assistant with access to the local filesystem.
Current working directory: {}

You have the following tools available. To call a tool, output a JSON block:
```tool
{{"name": "<tool_name>", "args": {{<arguments>}}, "id": "<unique_id>"}}
```

Available tools:
- read_file(path): Read contents of a file. Args: {{"path": "relative/or/absolute/path"}}
- write_file(path, content): Write content to a file (creates or overwrites). Args: {{"path": "...", "content": "..."}}
- append_file(path, content): Append content to a file. Args: {{"path": "...", "content": "..."}}
- edit_file(path, old_string, new_string): Replace old_string with new_string in a file (structural edit). Args: {{"path": "...", "old_string": "...", "new_string": "..."}}
- list_dir(path): List directory contents. Args: {{"path": "."}}
- run_shell(command): Run a shell command and return output. Args: {{"command": "ls -la"}}
- search_files(pattern, path): Search for text pattern in files. Args: {{"pattern": "fn main", "path": "."}}
- delete_file(path): Permanently delete a file. Args: {{"path": "..."}}

Rules:
- Always plan before acting. Briefly state what you will do and why.
- When writing code, prefer writing complete, working files.
- After every tool result you receive, reflect on it before taking the next action.
- When you have finished a task, summarize what you did.
- If a tool fails, diagnose the error and try a corrective approach.
- Only call tools when necessary. Prefer reading before writing.
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
        }
    ])
}
