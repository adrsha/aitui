use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::api::ChatMessage;

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
    /// The working directory this session belongs to. Resuming a session `cd`s
    /// back here so file tools and `@`-mentions resolve against the same project.
    pub cwd: Option<PathBuf>,
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
            cwd: std::env::current_dir().ok(),
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
    #[serde(default)]
    cwd: Option<PathBuf>,
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
            cwd: s.cwd.clone(),
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
            cwd: s.cwd,
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

    /// Set agent mode on every session (used to apply `agent_default` at startup,
    /// since agent mode isn't persisted and loaded sessions default to off).
    pub fn set_agent_mode_all(&mut self, on: bool) {
        for s in &mut self.sessions {
            s.agent_mode = on;
        }
    }

    pub fn new_session(&mut self) {
        let session = Session::new(self.next_id);
        self.next_id += 1;
        self.sessions.push(session);
        self.active_idx = self.sessions.len() - 1;
    }

    /// Duplicate the active session (messages, prompt, agent mode, cwd) into a new
    /// session and select it, so the conversation can branch in parallel.
    pub fn fork_active(&mut self) {
        let mut copy = self.sessions[self.active_idx].clone();
        copy.id = self.next_id;
        self.next_id += 1;
        copy.name = format!("{} (fork)", copy.name);
        // A fork starts idle — never inherit the source's in-flight stream state.
        copy.pending_assistant_text = None;
        copy.pending_reasoning = None;
        self.sessions.push(copy);
        self.active_idx = self.sessions.len() - 1;
    }

    /// The active session's stable id.
    pub fn active_id(&self) -> usize {
        self.sessions[self.active_idx].id
    }

    pub fn by_id(&self, id: usize) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn by_id_mut(&mut self, id: usize) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    /// Whether the session with `id` still exists (streams outlive session switches
    /// but not deletions).
    pub fn has_id(&self, id: usize) -> bool {
        self.sessions.iter().any(|s| s.id == id)
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
    base.join("aitui").join("sessions.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Session ────────────────────────────────────────────────────────────────

    #[test]
    fn new_session_has_default_name() {
        let s = Session::new(1);
        assert_eq!(s.name, "Session 1");
        assert!(!s.agent_mode);
        assert!(s.messages.is_empty());
    }

    #[test]
    fn begin_and_append_stream_accumulates_text() {
        let mut s = Session::new(1);
        s.begin_assistant_stream();
        assert!(s.is_streaming());
        s.append_stream_token("Hello ");
        s.append_stream_token("world");
        assert_eq!(s.streaming_display().unwrap(), "Hello world");
    }

    #[test]
    fn append_reasoning_works_without_stream() {
        let mut s = Session::new(1);
        s.append_reasoning("reasoning"); // no-op if no stream active
        assert!(s.pending_reasoning.is_none());
    }

    #[test]
    fn append_reasoning_with_stream() {
        let mut s = Session::new(1);
        s.begin_assistant_stream();
        s.append_reasoning("step 1 ");
        s.append_reasoning("step 2");
        s.append_stream_token("result");
        let display = s.streaming_display().unwrap();
        assert!(display.contains("step 1 step 2"));
        assert!(display.contains("result"));
    }

    #[test]
    fn finalize_stream_pushes_assistant_message() {
        let mut s = Session::new(1);
        s.begin_assistant_stream();
        s.append_stream_token("final text");
        s.finalize_assistant_stream();
        assert!(!s.is_streaming());
        assert_eq!(s.messages.len(), 1);
        assert_eq!(s.messages[0].role, "assistant");
    }

    #[test]
    fn finalize_stream_with_thinking_wraps_reasoning() {
        let mut s = Session::new(1);
        s.begin_assistant_stream();
        s.append_reasoning("thinks deeply");
        s.append_stream_token("answer");
        s.finalize_assistant_stream();
        let text = content_text(&s.messages[0].content);
        assert!(text.contains("<think>"));
        assert!(text.contains("thinks deeply"));
        assert!(text.contains("answer"));
    }

    #[test]
    fn finalize_empty_stream_does_not_push() {
        let mut s = Session::new(1);
        s.begin_assistant_stream();
        s.finalize_assistant_stream();
        assert!(s.messages.is_empty());
    }

    #[test]
    fn push_message_appends() {
        let mut s = Session::new(1);
        s.push_message(ChatMessage::user("hello"));
        assert_eq!(s.messages.len(), 1);
    }

    #[test]
    fn first_message_preview_finds_first_user() {
        let mut s = Session::new(1);
        s.push_message(ChatMessage::assistant("hi"));
        s.push_message(ChatMessage::user("hello world"));
        assert_eq!(s.first_message_preview(100).unwrap(), "hello world");
    }

    #[test]
    fn first_message_preview_none_if_no_user() {
        let s = Session::new(1);
        assert!(s.first_message_preview(10).is_none());
    }

    #[test]
    fn first_message_preview_truncates() {
        let mut s = Session::new(1);
        s.push_message(ChatMessage::user("hello world foo bar"));
        let preview = s.first_message_preview(5).unwrap();
        assert!(preview.chars().count() <= 6);
        assert!(preview.contains("…"));
    }

    #[test]
    fn is_streaming_false_by_default() {
        let s = Session::new(1);
        assert!(!s.is_streaming());
    }

    fn content_text(c: &crate::api::models::MessageContent) -> &str {
        match c {
            crate::api::models::MessageContent::Text(t) => t.as_str(),
            _ => "",
        }
    }

    #[test]
    fn api_messages_includes_system_prompt() {
        let mut s = Session::new(1);
        s.system_prompt = Some("Be helpful".into());
        let msgs = s.api_messages();
        assert!(msgs.iter().any(|m| m.role == "system" && content_text(&m.content).contains("Be helpful")));
    }

    #[test]
    fn api_messages_agent_mode_adds_tool_prompt() {
        let mut s = Session::new(1);
        s.agent_mode = true;
        let msgs = s.api_messages();
        let sys_msgs: Vec<_> = msgs.iter().filter(|m| m.role == "system").collect();
        assert_eq!(sys_msgs.len(), 1);
        assert!(content_text(&sys_msgs[0].content).contains("tool"));
    }

    #[test]
    fn api_messages_remaps_tool_role_to_user() {
        let mut s = Session::new(1);
        s.push_message(ChatMessage::tool("tool output"));
        let msgs = s.api_messages();
        let tool_msg = msgs.iter().find(|m| content_text(&m.content) == "tool output").unwrap();
        assert_eq!(tool_msg.role, "user");
    }

    // ── SessionManager ─────────────────────────────────────────────────────────

    #[test]
    fn manager_new_creates_one_session() {
        let m = SessionManager::new();
        assert_eq!(m.all().len(), 1);
        assert_eq!(m.active_idx(), 0);
    }

    #[test]
    fn manager_new_session_adds_and_selects() {
        let mut m = SessionManager::new();
        m.new_session();
        assert_eq!(m.all().len(), 2);
        assert_eq!(m.active_idx(), 1);
    }

    #[test]
    fn manager_select_next_cycles_within_bounds() {
        let mut m = SessionManager::new();
        m.new_session();
        m.new_session();
        // idx = 2 (last of 3), select_next stays at 2
        m.select_next();
        assert_eq!(m.active_idx(), 2);
        // go back to 0 then forward
        m.select_prev();
        m.select_prev();
        assert_eq!(m.active_idx(), 0);
        m.select_next();
        assert_eq!(m.active_idx(), 1);
        m.select_next();
        assert_eq!(m.active_idx(), 2);
    }

    #[test]
    fn manager_select_prev_cycles_within_bounds() {
        let mut m = SessionManager::new();
        assert_eq!(m.active_idx(), 0);
        m.select_prev(); // stays at 0
        assert_eq!(m.active_idx(), 0);
    }

    #[test]
    fn manager_remove_active_resets_when_last() {
        let mut m = SessionManager::new();
        m.remove_active();
        assert_eq!(m.all().len(), 1);
        assert_eq!(m.active_idx(), 0);
    }

    #[test]
    fn manager_remove_active_switches_previous() {
        let mut m = SessionManager::new();
        m.new_session();
        m.new_session();
        m.select_prev();
        assert_eq!(m.active_idx(), 1);
        m.remove_active();
        assert_eq!(m.all().len(), 2);
    }

    #[test]
    fn manager_select_updates_active() {
        let mut m = SessionManager::new();
        m.new_session();
        m.select(0);
        assert_eq!(m.active_idx(), 0);
    }

    #[test]
    fn manager_select_out_of_bounds_noop() {
        let mut m = SessionManager::new();
        m.select(100);
        assert_eq!(m.active_idx(), 0);
    }

    #[test]
    fn manager_active_mut_modifies_active() {
        let mut m = SessionManager::new();
        m.active_mut().name = "Custom".into();
        assert_eq!(m.active().name, "Custom");
    }
}
