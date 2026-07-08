use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::api::models::{ContentPart, MessageContent};
use crate::api::ChatMessage;

/// Plain display/serialization text of a stored message.
fn msg_text(m: &ChatMessage) -> String {
    match &m.content {
        MessageContent::Text(t) => t.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.clone()),
                ContentPart::ImageUrl { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// Upper bound on a single streamed message (content or reasoning), in bytes.
/// Guards against a runaway endpoint that never terminates the stream. Far larger
/// than any legitimate response.
const MAX_STREAM_BYTES: usize = 20 * 1024 * 1024;

/// How long to wait for the first visible stream event before restarting the
/// request. This covers accepted-but-silent backend hangs without killing slow
/// healthy streams after they begin producing output.
pub const COLD_STREAM_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(45);

/// Retry cold starts only a few times; after that the user sees the normal wait
/// state and can cancel with Ctrl-C.
pub const MAX_COLD_STREAM_RETRIES: u8 = 2;

/// Autonomous-loop configuration for a session. When present, the agent keeps
/// taking turns toward `goal` on its own — after each completed turn it either
/// continues (injecting a nudge) or stops, and the model ends it by calling the
/// `finish` tool once `stop` is met. `iteration` counts completed turns; `max`
/// caps them as a hard safety stop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopState {
    pub goal: String,
    pub stop: String,
    pub iteration: usize,
    pub max: usize,
}

impl LoopState {
    /// The default iteration cap when the user doesn't specify one.
    pub const DEFAULT_MAX: usize = 25;
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
    /// Wall-clock start of the currently streamed assistant response.
    pub pending_started_at: Option<std::time::Instant>,
    /// Wall-clock moment the first token/reasoning byte of the current response
    /// arrived, so we can report time-to-first-result separately from total time.
    pub pending_first_at: Option<std::time::Instant>,
    /// Whether the current streamed assistant response comes from the mock backend.
    pub pending_mock: bool,
    /// Agent-declared task breakdown for this session, shown in the sticky panel
    /// above the input while the session is active.
    pub todos: Vec<crate::app::state::TodoItem>,
    /// Agent mode: when true the session uses tool-calling prompts.
    pub agent_mode: bool,
    /// The working directory this session belongs to. Resuming a session `cd`s
    /// back here so file tools and `@`-mentions resolve against the same project.
    pub cwd: Option<PathBuf>,
    /// Unsent composer text, stashed when leaving the session and restored on
    /// return. Persisted to disk so a draft survives a restart.
    pub draft: String,
    /// Active autonomous loop, if any. `Some` means the agent keeps working toward
    /// the goal on its own until `finish`/`max`/cancel. Persisted so a loop survives
    /// a restart.
    pub loop_state: Option<LoopState>,
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
            pending_started_at: None,
            pending_first_at: None,
            pending_mock: false,
            todos: Vec::new(),
            agent_mode: false,
            cwd: std::env::current_dir().ok(),
            draft: String::new(),
            loop_state: None,
        }
    }

    /// Start a streaming assistant message.
    pub fn begin_assistant_stream(&mut self) {
        self.pending_assistant_text = Some(String::new());
        self.pending_reasoning = None;
        self.pending_started_at = Some(std::time::Instant::now());
        self.pending_first_at = None;
    }

    /// Append a content token to the in-progress assistant message.
    pub fn append_stream_token(&mut self, token: &str) {
        if let Some(text) = self.pending_assistant_text.as_mut() {
            self.pending_first_at
                .get_or_insert_with(std::time::Instant::now);
            // Cap accumulation so a broken/malicious endpoint that streams forever
            // (never sending [DONE]) can't grow this unbounded and exhaust memory.
            if text.len() < MAX_STREAM_BYTES {
                text.push_str(token);
            }
        }
    }

    /// Append a reasoning token to the in-progress assistant message.
    pub fn append_reasoning(&mut self, token: &str) {
        if self.pending_assistant_text.is_some() {
            self.pending_first_at
                .get_or_insert_with(std::time::Instant::now);
            let buf = self.pending_reasoning.get_or_insert_with(String::new);
            if buf.len() < MAX_STREAM_BYTES {
                buf.push_str(token);
            }
        }
    }

    /// Mark progress for streamed metadata that is not part of the final visible
    /// assistant body, such as native tool-call headers.
    pub fn mark_stream_progress(&mut self) {
        if self.pending_assistant_text.is_some() {
            self.pending_first_at
                .get_or_insert_with(std::time::Instant::now);
        }
    }

    /// Drop a still-empty pending assistant shell before retrying the same turn.
    /// If anything visible arrived, keep it so a retry cannot erase real output.
    pub fn cancel_empty_assistant_stream(&mut self) {
        let empty_text = self
            .pending_assistant_text
            .as_ref()
            .map(|t| t.is_empty())
            .unwrap_or(false);
        let empty_reasoning = self
            .pending_reasoning
            .as_ref()
            .map(|r| r.is_empty())
            .unwrap_or(true);
        if empty_text && empty_reasoning {
            self.pending_assistant_text = None;
            self.pending_reasoning = None;
            self.pending_started_at = None;
            self.pending_first_at = None;
            self.pending_mock = false;
        }
    }

    /// Time-to-first-result so far (ms), fixed once the first token has arrived.
    /// None while still waiting on the very first byte.
    pub fn pending_first_ms(&self) -> Option<u64> {
        let (start, first) = (self.pending_started_at?, self.pending_first_at?);
        Some(first.saturating_duration_since(start).as_millis() as u64)
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
        let started = self.pending_started_at.take();
        let duration_ms = started.map(|t| t.elapsed().as_millis() as u64);
        // Time-to-first-result: first byte relative to the request start.
        let first_ms = self
            .pending_first_at
            .take()
            .zip(started)
            .map(|(f, s)| f.saturating_duration_since(s).as_millis() as u64);
        let is_mock = self.pending_mock;
        self.pending_mock = false;
        if let Some(text) = self.pending_assistant_text.take() {
            let body = Self::compose(reasoning.as_deref(), &text);
            if !body.trim().is_empty() {
                let mut msg = ChatMessage::assistant(body);
                msg.duration_ms = duration_ms;
                msg.first_ms = first_ms;
                msg.mock = is_mock;
                self.messages.push(msg);
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

    /// Index of the most recent `user` message, if any.
    fn last_user_index(&self) -> Option<usize> {
        self.messages.iter().rposition(|m| m.role == "user")
    }

    /// Plain text of the most recent `assistant` message.
    pub fn last_assistant_text(&self) -> Option<String> {
        self.messages
            .iter()
            .rposition(|m| m.role == "assistant")
            .map(|i| msg_text(&self.messages[i]))
    }

    /// Drop everything the assistant produced after the last user turn (the
    /// assistant reply plus any tool messages), keeping the user message itself.
    /// Returns false when there's no user message to retry. Used by `:retry`.
    pub fn trim_after_last_user(&mut self) -> bool {
        match self.last_user_index() {
            Some(i) => {
                self.messages.truncate(i + 1);
                true
            }
            None => false,
        }
    }

    /// Remove the last user turn entirely (the user message and everything after
    /// it) and return that user message's text, so it can be re-edited in the
    /// composer. Used by `:edit-last`.
    pub fn take_last_user_turn(&mut self) -> Option<String> {
        let i = self.last_user_index()?;
        let text = msg_text(&self.messages[i]);
        self.messages.truncate(i);
        Some(text)
    }

    /// All messages suitable for sending to the API.
    ///
    /// - In agent mode the tool-calling system prompt is prepended so the model
    ///   knows which tools exist and how to call them.
    /// - A user-defined system prompt (if any) is added after it.
    /// - When `native` is on, a stored assistant turn's fenced ```` ```tool ````
    ///   calls become structured `tool_calls`, and the following "tool" results
    ///   become native `role:"tool"` messages with matching `tool_call_id`.
    /// - Otherwise (fenced fallback) "tool" messages are re-mapped to "user" so
    ///   plain OpenAI-compatible endpoints accept them as context.
    #[cfg(test)]
    pub fn api_messages(&self, native: bool) -> Vec<ChatMessage> {
        self.api_messages_windowed(native, None)
    }

    /// Like [`api_messages`], but drops the oldest turns so the tail fits within
    /// `char_budget` characters (a proactive sliding window that keeps the request
    /// under the model's context limit). `None` sends the whole history.
    pub fn api_messages_windowed(
        &self,
        native: bool,
        char_budget: Option<usize>,
    ) -> Vec<ChatMessage> {
        let mut out = Vec::with_capacity(self.messages.len() + 2);
        if self.agent_mode {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            out.push(ChatMessage::system(crate::agent::agent_system_prompt(&cwd)));
        }
        if let Some(ref prompt) = self.system_prompt {
            out.push(ChatMessage::system(prompt.clone()));
        }

        let msgs = &self.messages;
        let mut i = char_budget.map(|b| self.window_start(b)).unwrap_or(0);
        while i < msgs.len() {
            let m = &msgs[i];
            // Mock assistant turns are only for the visible offline transcript; never
            // feed them back to a live API context.
            if m.mock {
                i += 1;
                continue;
            }
            // Native: an assistant turn with fenced tool calls, immediately followed
            // by exactly that many "tool" results, is sent as structured tool_calls.
            if native && m.role == "assistant" {
                let text = msg_text(m);
                let calls = crate::agent::parser::extract_tool_calls(&text);
                if !calls.is_empty() {
                    // Gather the run of tool results answering this turn.
                    let mut n = 0;
                    while i + 1 + n < msgs.len()
                        && msgs[i + 1 + n].role == "tool"
                        && n < calls.len()
                    {
                        n += 1;
                    }
                    if n == calls.len() {
                        let prose = crate::agent::parser::strip_tool_blocks(&text)
                            .trim()
                            .to_string();
                        let api_calls: Vec<crate::api::models::ApiToolCall> = calls
                            .iter()
                            .enumerate()
                            .map(|(k, c)| {
                                let id = c.id.clone().unwrap_or_else(|| format!("call_{}", k));
                                crate::api::models::ApiToolCall::function(
                                    id,
                                    &c.name,
                                    c.args.to_string(),
                                )
                            })
                            .collect();
                        out.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: crate::api::models::MessageContent::Text(prose),
                            mock: false,
                            duration_ms: None,
                            first_ms: None,
                            tool_calls: Some(api_calls.clone()),
                            tool_call_id: None,
                        });
                        for (k, api_call) in api_calls.iter().enumerate() {
                            let result = &msgs[i + 1 + k];
                            out.push(ChatMessage {
                                role: "tool".to_string(),
                                content: result.content.clone(),
                                mock: false,
                                duration_ms: None,
                                first_ms: None,
                                tool_calls: None,
                                tool_call_id: Some(api_call.id.clone()),
                            });
                        }
                        i += 1 + n;
                        continue;
                    }
                    // else: orphaned call (e.g. cancelled round) — fall through and
                    // send it the fenced way so the API never sees an unanswered call.
                }
            }

            if m.role == "tool" {
                out.push(ChatMessage {
                    role: "user".to_string(),
                    content: m.content.clone(),
                    mock: false,
                    duration_ms: None,
                    first_ms: None,
                    tool_calls: None,
                    tool_call_id: None,
                });
            } else {
                out.push(m.clone());
            }
            i += 1;
        }
        out
    }

    /// Earliest index into `self.messages` whose tail fits within `char_budget`
    /// characters, snapped forward to a `user`-message boundary so a kept window
    /// never begins on an orphaned tool result or a mid-turn assistant reply.
    /// Always keeps at least the final user turn (even if that lone turn is over
    /// budget — the reactive `compact_history` path handles a turn too big to
    /// ever fit).
    fn window_start(&self, char_budget: usize) -> usize {
        let msgs = &self.messages;
        if msgs.is_empty() {
            return 0;
        }
        // Walk backwards summing char cost; stop before the message that would
        // push the tail over budget. The first (newest) message is always kept.
        let mut total = 0usize;
        let mut start = msgs.len();
        for i in (0..msgs.len()).rev() {
            let cost = msg_text(&msgs[i]).len();
            if start != msgs.len() && total + cost > char_budget {
                break;
            }
            total += cost;
            start = i;
        }
        // Snap forward to the first user message at/after `start` so tool groups
        // and assistant turns aren't left dangling without their user prompt.
        match msgs[start..].iter().position(|m| m.role == "user") {
            Some(off) => start + off,
            None => self.last_user_index().unwrap_or(start),
        }
    }

    /// Safety-net compaction when the endpoint reports the context is full even
    /// after the proactive window: permanently drop the oldest ~half of the
    /// conversation (whole turns, cutting on a user-message boundary, never the
    /// final user turn). Returns false when only the current turn remains — a
    /// single turn too large to ever fit, which the caller must surface as a hard
    /// error rather than retry forever.
    pub fn compact_history(&mut self) -> bool {
        let users: Vec<usize> = self
            .messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == "user")
            .map(|(i, _)| i)
            .collect();
        if users.len() < 2 {
            return false;
        }
        let cut = users[users.len() / 2];
        self.messages.drain(..cut);
        true
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
                text.lines().nth(1).unwrap_or(text)
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
    todos: Vec<crate::app::state::TodoItem>,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    draft: String,
    #[serde(default)]
    loop_state: Option<LoopState>,
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
            todos: s.todos.clone(),
            cwd: s.cwd.clone(),
            draft: s.draft.clone(),
            loop_state: s.loop_state.clone(),
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
            pending_started_at: None,
            pending_first_at: None,
            pending_mock: false,
            todos: s.todos,
            agent_mode: false,
            cwd: s.cwd,
            draft: s.draft,
            loop_state: s.loop_state,
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
            Err(_) => {
                // Don't silently discard a corrupt save: preserve it as `.bak` so the
                // user (or a future recovery) can inspect it, then start fresh.
                let _ = fs::rename(&path, path.with_extension("json.bak"));
                return Self::new();
            }
        };
        if saved.sessions.is_empty() {
            return Self::new();
        }
        let active_idx = saved.active_idx.min(saved.sessions.len() - 1);
        let next_id = saved.next_id;
        let sessions = saved.sessions.into_iter().map(Session::from).collect();
        Self {
            sessions,
            active_idx,
            next_id,
        }
    }

    /// Persist all sessions to disk (silently ignores errors). Serialization and
    /// the blocking `fs::write` are moved off the UI thread via `spawn_blocking`
    /// when a tokio runtime is available (the app), so finishing a turn doesn't
    /// hitch the render loop; falls back to a synchronous write otherwise (tests).
    pub fn save(&self) {
        let path = sessions_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        // Build the owned snapshot on the caller's thread (cheap clones), then hand
        // the expensive serialize + write to a blocking task.
        let state = SavedState {
            sessions: self.sessions.iter().map(SavedSession::from).collect(),
            active_idx: self.active_idx,
            next_id: self.next_id,
        };
        let write = move || {
            if let Ok(json) = serde_json::to_string_pretty(&state) {
                let _ = atomic_write(&path, json.as_bytes());
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(_) => {
                tokio::task::spawn_blocking(write);
            }
            Err(_) => write(),
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
        // A fork starts idle — never inherit the source's in-flight stream state,
        // nor its autonomous loop (a fork is a manual branch to explore by hand).
        copy.pending_assistant_text = None;
        copy.pending_reasoning = None;
        copy.loop_state = None;
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
        self.remove_at(self.active_idx);
    }

    /// Remove the session at `idx`. The last session is never removed — it is reset
    /// to an empty "Session 1" instead. Keeps `active_idx` pointing at a valid
    /// session (shifts it left when a session at/before it is removed).
    pub fn remove_at(&mut self, idx: usize) {
        if idx >= self.sessions.len() {
            return;
        }
        if self.sessions.len() <= 1 {
            self.sessions[0].messages.clear();
            self.sessions[0].name = "Session 1".to_string();
            self.sessions[0].system_prompt = None;
            self.active_idx = 0;
            return;
        }
        self.sessions.remove(idx);
        if self.active_idx > idx || self.active_idx >= self.sessions.len() {
            self.active_idx = self
                .active_idx
                .saturating_sub(1)
                .min(self.sessions.len() - 1);
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

/// Write `bytes` to `path` atomically: write to a temp file in the same directory,
/// then rename over the target. Rename is atomic on a POSIX filesystem, so a reader
/// (or a crash) never sees a half-written file — the old contents survive intact
/// until the new file is fully in place. Prevents session loss on a kill mid-save.
fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    // Per-call counter so two overlapping saves (session writes run on background
    // blocking tasks) never pick the same temp path and clobber each other.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let tmp = dir.join(format!(
        ".{}.tmp.{}.{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("sessions"),
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?; // flush to disk before the rename so the data is durable
    }
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp); // don't leave temp litter on failure
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Session ────────────────────────────────────────────────────────────────

    #[test]
    fn atomic_write_creates_and_overwrites_without_leaving_temp() {
        let dir = std::env::temp_dir().join(format!("aitui_atomic_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("data.json");
        atomic_write(&path, b"first").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "first");
        // Overwriting replaces the contents wholesale.
        atomic_write(&path, b"second-longer").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second-longer");
        // No leftover temp files in the directory.
        let temps = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .count();
        assert_eq!(temps, 0, "atomic_write left a temp file behind");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_session_has_default_name() {
        let s = Session::new(1);
        assert_eq!(s.name, "Session 1");
        assert!(!s.agent_mode);
        assert!(s.messages.is_empty());
    }

    #[test]
    fn window_drops_oldest_turns_over_budget() {
        let mut s = Session::new(1);
        // Three user/assistant turns, ~20 chars of text each.
        for n in 0..3 {
            s.push_message(ChatMessage::user(format!("question number {}", n)));
            s.push_message(ChatMessage::assistant(format!("answer number  {}", n)));
        }
        // Budget large enough for only the last turn's ~34 chars.
        let msgs = s.api_messages_windowed(false, Some(40));
        // Only the final turn survives, and the window starts on a user message.
        assert_eq!(msgs.first().map(|m| m.role.as_str()), Some("user"));
        assert!(
            msgs.len() < 6,
            "expected oldest turns dropped, got {:?}",
            msgs.len()
        );
        assert_eq!(msg_text(msgs.last().unwrap()), "answer number  2");
    }

    #[test]
    fn window_keeps_last_user_turn_even_when_over_budget() {
        let mut s = Session::new(1);
        s.push_message(ChatMessage::user(
            "a very long final question that exceeds the budget",
        ));
        // Budget smaller than the single turn — must still be sent, not dropped.
        let msgs = s.api_messages_windowed(false, Some(1));
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
    }

    #[test]
    fn compact_drops_oldest_half_but_keeps_current_turn() {
        let mut s = Session::new(1);
        for n in 0..4 {
            s.push_message(ChatMessage::user(format!("q{}", n)));
            s.push_message(ChatMessage::assistant(format!("a{}", n)));
        }
        let before = s.messages.len();
        assert!(s.compact_history());
        assert!(s.messages.len() < before);
        // First surviving message is a user turn (clean boundary), last is intact.
        assert_eq!(s.messages.first().unwrap().role, "user");
        assert_eq!(msg_text(s.messages.last().unwrap()), "a3");
    }

    #[test]
    fn compact_refuses_when_only_one_turn_left() {
        let mut s = Session::new(1);
        s.push_message(ChatMessage::user("only turn"));
        // Nothing older to drop → false, so the caller surfaces a hard error.
        assert!(!s.compact_history());
        assert_eq!(s.messages.len(), 1);
    }

    #[test]
    fn retry_and_edit_helpers_manipulate_the_last_turn() {
        let mut s = Session::new(1);
        s.push_message(ChatMessage::user("hello"));
        s.push_message(ChatMessage::assistant("hi there"));
        s.push_message(ChatMessage::tool("[tool-result:read] read(x) (ok)"));

        assert_eq!(s.last_assistant_text().as_deref(), Some("hi there"));

        // `:retry` keeps the user message but drops the reply + tool result.
        assert!(s.trim_after_last_user());
        assert_eq!(s.messages.len(), 1);
        assert_eq!(s.messages[0].role, "user");

        // `:edit-last` removes the user turn entirely and hands back its text.
        assert_eq!(s.take_last_user_turn().as_deref(), Some("hello"));
        assert!(s.messages.is_empty());

        // Nothing left → both report "nothing to do".
        assert!(!s.trim_after_last_user());
        assert!(s.take_last_user_turn().is_none());
        assert!(s.last_assistant_text().is_none());
    }

    #[test]
    fn tracks_time_to_first_result() {
        let mut s = Session::new(1);
        s.begin_assistant_stream();
        // Nothing yet → time-to-first-result is unknown (header shows a live wait).
        assert!(s.pending_first_ms().is_none());
        s.append_stream_token("hi");
        // First byte arrived → TTFT is now fixed.
        assert!(s.pending_first_ms().is_some());
        s.finalize_assistant_stream();
        let msg = s.messages.last().unwrap();
        assert!(msg.first_ms.is_some());
        // Time-to-first-result can't exceed the full duration.
        assert!(msg.first_ms.unwrap() <= msg.duration_ms.unwrap());
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
        let msgs = s.api_messages(false);
        assert!(msgs
            .iter()
            .any(|m| m.role == "system" && content_text(&m.content).contains("Be helpful")));
    }

    #[test]
    fn api_messages_agent_mode_adds_tool_prompt() {
        let mut s = Session::new(1);
        s.agent_mode = true;
        let msgs = s.api_messages(false);
        let sys_msgs: Vec<_> = msgs.iter().filter(|m| m.role == "system").collect();
        assert_eq!(sys_msgs.len(), 1);
        assert!(content_text(&sys_msgs[0].content).contains("tool"));
    }

    #[test]
    fn api_messages_remaps_tool_role_to_user() {
        let mut s = Session::new(1);
        s.push_message(ChatMessage::tool("tool output"));
        let msgs = s.api_messages(false);
        let tool_msg = msgs
            .iter()
            .find(|m| content_text(&m.content) == "tool output")
            .unwrap();
        assert_eq!(tool_msg.role, "user");
    }

    #[test]
    fn api_messages_native_converts_fenced_call_to_tool_calls() {
        let mut s = Session::new(1);
        s.agent_mode = true;
        s.push_message(ChatMessage::user("read it"));
        s.push_message(ChatMessage::assistant(
            "sure\n```tool\n{\"name\":\"read_file\",\"args\":{\"path\":\"a.rs\"},\"id\":\"call_1\"}\n```",
        ));
        s.push_message(ChatMessage::tool("[tool-result] Read a.rs (ok)\ncontents"));

        let msgs = s.api_messages(true);
        let a = msgs
            .iter()
            .find(|m| m.role == "assistant" && m.tool_calls.is_some())
            .expect("native assistant");
        let calls = a.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(calls[0].id, "call_1");
        // The fenced text was stripped from the assistant content.
        assert!(!content_text(&a.content).contains("```tool"));
        // The following result is a native tool message referencing the call id.
        let t = msgs
            .iter()
            .find(|m| m.role == "tool")
            .expect("native tool message");
        assert_eq!(t.tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn api_messages_native_orphan_call_stays_fenced() {
        let mut s = Session::new(1);
        s.agent_mode = true;
        // A tool call with no following result (e.g. a cancelled round) must NOT
        // become a structured tool_calls (the API would 400 on the unanswered call).
        s.push_message(ChatMessage::assistant(
            "```tool\n{\"name\":\"read_file\",\"args\":{\"path\":\"a.rs\"}}\n```",
        ));
        let msgs = s.api_messages(true);
        let a = msgs.iter().find(|m| m.role == "assistant").unwrap();
        assert!(a.tool_calls.is_none(), "orphan call must stay fenced");
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
