use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::api::ChatMessage;
use crate::agent::{ToolCall, ToolResult};

/// One tool call + result entry tracked for the session timeline.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ToolEvent {
    pub call: ToolCall,
    /// None while waiting for permission / executing.
    pub result: Option<ToolResult>,
    /// Display state
    pub state: ToolEventState,
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum ToolEventState {
    Pending,   // awaiting permission
    Running,   // executing
    Done,      // finished (success or fail)
    Denied,    // user denied
}

/// A single named conversation session.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: usize,
    pub name: String,
    pub messages: Vec<ChatMessage>,
    pub system_prompt: Option<String>,
    /// Accumulated text of the message currently being streamed. None when idle.
    pub pending_assistant_text: Option<String>,
    /// Accumulated reasoning ("thinking") for the in-progress message, if the
    /// endpoint streams it in a separate field.
    pub pending_reasoning: Option<String>,
    /// Agent mode: when true the session uses tool-calling prompts.
    pub agent_mode: bool,
    /// Timeline of tool calls for this session (displayed inline in chat).
    pub tool_events: Vec<ToolEvent>,
}

impl Session {
    pub fn new(id: usize) -> Self {
        Self {
            id,
            name: format!("Session {}", id),
            messages: Vec::new(),
            system_prompt: None,
            pending_assistant_text: None,
            pending_reasoning: None,
            agent_mode: false,
            tool_events: Vec::new(),
        }
    }

    /// Start a streaming assistant message.
    pub fn begin_assistant_stream(&mut self) {
        self.pending_assistant_text = Some(String::new());
        self.pending_reasoning = None;
    }

    /// Append a content token to the in-progress assistant message.
    pub fn append_stream_token(&mut self, token: &str) {
        if let Some(text) = self.pending_assistant_text.as_mut() {
            text.push_str(token);
        }
    }

    /// Append a reasoning token to the in-progress assistant message.
    pub fn append_reasoning(&mut self, token: &str) {
        if self.pending_assistant_text.is_some() {
            self.pending_reasoning.get_or_insert_with(String::new).push_str(token);
        }
    }

    /// Compose the on-disk/display body from reasoning + content. Reasoning is
    /// wrapped in `<think>` so the block parser renders it as a collapsible
    /// thinking section uniformly with inline-tagged reasoning.
    fn compose(reasoning: Option<&str>, text: &str) -> String {
        match reasoning {
            Some(r) if !r.trim().is_empty() => format!("<think>\n{}\n</think>\n{}", r.trim(), text),
            _ => text.to_string(),
        }
    }

    /// Finalize the streamed message and push it to history.
    pub fn finalize_assistant_stream(&mut self) {
        let reasoning = self.pending_reasoning.take();
        if let Some(text) = self.pending_assistant_text.take() {
            let body = Self::compose(reasoning.as_deref(), &text);
            if !body.trim().is_empty() {
                self.messages.push(ChatMessage::assistant(body));
            }
        }
    }

    /// Display body of the streaming message (reasoning + partial), or None.
    pub fn streaming_display(&self) -> Option<String> {
        self.pending_assistant_text
            .as_ref()
            .map(|text| Self::compose(self.pending_reasoning.as_deref(), text))
    }

    /// Push a pre-built message into history.
    pub fn push_message(&mut self, msg: ChatMessage) {
        self.messages.push(msg);
    }

    /// All messages suitable for sending to the API.
    ///
    /// - In agent mode the tool-calling system prompt is prepended so the model
    ///   knows which tools exist and how to call them.
    /// - A user-defined system prompt (if any) is added after it.
    /// - Internal "tool" role messages are re-mapped to "user" so plain
    ///   OpenAI-compatible endpoints accept them as context.
    pub fn api_messages(&self) -> Vec<ChatMessage> {
        let mut out = Vec::with_capacity(self.messages.len() + 2);
        if self.agent_mode {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            out.push(ChatMessage::system(crate::agent::agent_system_prompt(&cwd)));
        }
        if let Some(ref prompt) = self.system_prompt {
            out.push(ChatMessage::system(prompt.clone()));
        }
        for m in &self.messages {
            if m.role == "tool" {
                out.push(ChatMessage {
                    role: "user".to_string(),
                    content: m.content.clone(),
                });
            } else {
                out.push(m.clone());
            }
        }
        out
    }

    /// The display text of the streaming message, or None when idle.
    pub fn streaming_text(&self) -> Option<&str> {
        self.pending_assistant_text.as_deref()
    }

    pub fn is_streaming(&self) -> bool {
        self.pending_assistant_text.is_some()
    }

    /// First user message preview for the sidebar (up to `max_chars` chars).
    pub fn first_message_preview(&self, max_chars: usize) -> Option<String> {
        self.messages.iter().find(|m| m.role == "user").map(|m| {
            let text = match &m.content {
                crate::api::models::MessageContent::Text(t) => t.as_str(),
                crate::api::models::MessageContent::Parts(_) => "[image]",
            };
            // Strip leading ``` blocks (file attachments), trim whitespace
            let cleaned = if text.starts_with("```\n") {
                text.lines().skip(1).next().unwrap_or(text)
            } else {
                text.lines().next().unwrap_or(text)
            };
            let cleaned = cleaned.trim();
            if cleaned.len() > max_chars {
                format!("{}…", &cleaned[..max_chars])
            } else {
                cleaned.to_string()
            }
        })
    }
}

// ── Serializable snapshot for disk persistence ────────────────────────────────

#[derive(Serialize, Deserialize)]
struct SavedSession {
    id: usize,
    name: String,
    messages: Vec<ChatMessage>,
    system_prompt: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct SavedState {
    sessions: Vec<SavedSession>,
    active_idx: usize,
    next_id: usize,
}

impl From<&Session> for SavedSession {
    fn from(s: &Session) -> Self {
        SavedSession {
            id: s.id,
            name: s.name.clone(),
            messages: s.messages.clone(),
            system_prompt: s.system_prompt.clone(),
        }
    }
}

impl From<SavedSession> for Session {
    fn from(s: SavedSession) -> Self {
        Session {
            id: s.id,
            name: s.name,
            messages: s.messages,
            system_prompt: s.system_prompt,
            pending_assistant_text: None,
            pending_reasoning: None,
            agent_mode: false,
            tool_events: Vec::new(),
        }
    }
}

// ── SessionManager ────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct SessionManager {
    sessions: Vec<Session>,
    active_idx: usize,
    next_id: usize,
}

impl SessionManager {
    pub fn new() -> Self {
        let first = Session::new(1);
        Self {
            sessions: vec![first],
            active_idx: 0,
            next_id: 2,
        }
    }

    /// Try to load from disk; returns a fresh manager if no save file exists.
    pub fn load() -> Self {
        let path = sessions_path();
        let raw = match fs::read_to_string(&path) {
            Ok(r) => r,
            Err(_) => return Self::new(),
        };
        let saved: SavedState = match serde_json::from_str(&raw) {
            Ok(s) => s,
            Err(_) => return Self::new(),
        };
        if saved.sessions.is_empty() {
            return Self::new();
        }
        let active_idx = saved.active_idx.min(saved.sessions.len() - 1);
        let next_id = saved.next_id;
        let sessions = saved.sessions.into_iter().map(Session::from).collect();
        Self { sessions, active_idx, next_id }
    }

    /// Persist all sessions to disk (silently ignores errors).
    pub fn save(&self) {
        let path = sessions_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let state = SavedState {
            sessions: self.sessions.iter().map(SavedSession::from).collect(),
            active_idx: self.active_idx,
            next_id: self.next_id,
        };
        if let Ok(json) = serde_json::to_string_pretty(&state) {
            let _ = fs::write(&path, json);
        }
    }

    pub fn active(&self) -> &Session {
        &self.sessions[self.active_idx]
    }

    pub fn active_mut(&mut self) -> &mut Session {
        &mut self.sessions[self.active_idx]
    }

    pub fn all(&self) -> &[Session] {
        &self.sessions
    }

    pub fn active_idx(&self) -> usize {
        self.active_idx
    }

    pub fn new_session(&mut self) {
        let session = Session::new(self.next_id);
        self.next_id += 1;
        self.sessions.push(session);
        self.active_idx = self.sessions.len() - 1;
    }

    pub fn select(&mut self, idx: usize) {
        if idx < self.sessions.len() {
            self.active_idx = idx;
        }
    }

    pub fn select_next(&mut self) {
        if self.active_idx + 1 < self.sessions.len() {
            self.active_idx += 1;
        }
    }

    pub fn select_prev(&mut self) {
        if self.active_idx > 0 {
            self.active_idx -= 1;
        }
    }

    pub fn remove_active(&mut self) {
        if self.sessions.len() <= 1 {
            self.sessions[0].messages.clear();
            self.sessions[0].name = "Session 1".to_string();
            self.sessions[0].system_prompt = None;
            return;
        }
        self.sessions.remove(self.active_idx);
        if self.active_idx >= self.sessions.len() {
            self.active_idx = self.sessions.len() - 1;
        }
    }
}

fn sessions_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    base.join("aichat-tui").join("sessions.json")
}
