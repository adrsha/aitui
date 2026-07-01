//! Side effects: composing the chat document, starting model requests, and the
//! agent tool-execution loop. These methods may return a follow-up `Action`
//! (e.g. attach a freshly spawned stream) for the reducer to process.

use std::path::PathBuf;
use tokio::sync::mpsc;

use crate::agent::{self, Permission, ToolCall, ToolResult};
use crate::api::models::MessageContent;
use crate::api::{ChatMessage, ChatRequest};
use crate::app::action::Action;
use crate::app::overlay::{Overlay, PermissionRequest};
use crate::app::state::{expand_mentions, App, MAX_AGENT_ITERATIONS};
use crate::domain::blocks::{parse_blocks, parse_tool_result};
use crate::render::document::{build, DocMessage, RenderedLine};
use crate::render::theme::Theme;

/// Plain display text for a stored message.
fn message_text(m: &ChatMessage) -> String {
    match &m.content {
        MessageContent::Text(t) => t.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .map(|p| match p {
                crate::api::models::ContentPart::Text { text } => text.clone(),
                crate::api::models::ContentPart::ImageUrl { .. } => "🖼 [image attached]".to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

impl App {
    /// Render the active conversation as a plain-markdown document for `$EDITOR`.
    /// This is what Ctrl-O opens, so you can read/search history with real vim.
    pub fn conversation_markdown(&self) -> String {
        let session = self.sessions.active();
        let mut out = format!("# {}\n", session.name);
        if let Some(prompt) = &session.system_prompt {
            out.push_str(&format!("\n## system\n\n{}\n", prompt));
        }
        for m in &session.messages {
            let role = match m.role.as_str() {
                "user" => "You",
                "assistant" => "Assistant",
                "tool" => "Tool",
                "system" => "System",
                other => other,
            };
            out.push_str(&format!("\n## {}\n\n{}\n", role, message_text(m)));
        }
        if let Some(partial) = session.streaming_display() {
            out.push_str(&format!("\n## Assistant\n\n{}\n", partial));
        }
        out
    }

    /// Rebuild the chat document if the cache is stale, then keep the cursor valid.
    pub fn sync_chat_doc(&mut self, width: usize, viewport_h: usize) {
        if self.chat.needs_rebuild(self.content_rev, width) {
            let doc = build_chat_doc(self, width, &self.theme(), self.content_rev);
            self.chat.set_doc(doc, self.content_rev, width, viewport_h);
        }
    }

    // ── Submission ──────────────────────────────────────────────────────────

    pub fn submit(&mut self) -> Option<Action> {
        // No parallel turns yet: block a new send while the assistant is working,
        // but keep the composed text in the input so it's ready to fire once idle.
        if self.is_busy() {
            self.overlay = Overlay::Notice {
                title: " Busy ".into(),
                body: "The assistant is still working.\n\nYour message is kept in the input — \
                       press Enter again once the reply finishes.\n\n(Ctrl-C cancels the current turn.)"
                    .into(),
            };
            self.set_status("Can't send yet — assistant is working (Ctrl-C to cancel)");
            return None;
        }

        self.agent_iterations = 0;
        self.mention.reset();
        let text = self.input.take();
        let attachment = self.attachment.take();

        if text.trim().is_empty() && attachment.is_none() {
            self.set_status("Nothing to send. Type a message first.");
            return None;
        }

        // Save to input history (shell-style up/down recall).
        let trimmed = text.trim().to_string();
        if !trimmed.is_empty() && self.input_history.last().map(|s| s.as_str()) != Some(&trimmed) {
            self.input_history.push(trimmed);
            if self.input_history.len() > 100 {
                self.input_history.remove(0);
            }
        }
        self.input_history_idx = None;
        self.input_draft.clear();

        let mention_ctx = expand_mentions(&text);
        let text = if mention_ctx.is_empty() { text } else { format!("{}\n\n{}", mention_ctx, text) };

        let msg = build_user_message(&text, attachment.as_ref(), self);
        self.sessions.active_mut().push_message(msg);
        self.auto_name_session();
        self.touch();

        let sid = self.sessions.active_id();
        self.begin_stream_for(sid)
    }

    fn auto_name_session(&mut self) {
        let is_default = {
            let s = self.sessions.active();
            s.name.starts_with("Session ") && s.messages.len() == 1
        };
        if is_default {
            if let Some(preview) = self.sessions.active().first_message_preview(30) {
                if !preview.is_empty() {
                    self.sessions.active_mut().name = preview;
                }
            }
        }
    }

    fn begin_stream_for(&mut self, sid: usize) -> Option<Action> {
        let Some(session) = self.sessions.by_id_mut(sid) else { return None };
        session.begin_assistant_stream();
        // The animated status-bar spinner ("working") is the generating indicator
        // now — don't set a free-text "Generating…" that later messages clobber.
        self.status = None;
        if sid == self.sessions.active_id() {
            self.chat.stick_bottom = true;
        }
        self.touch();

        // Prepend active skills as system messages (personas / house styles).
        let mut messages = self.sessions.by_id(sid).map(|s| s.api_messages()).unwrap_or_default();
        for skill in self.skills.iter().rev().filter(|s| s.active) {
            messages.insert(0, ChatMessage::system(skill.body.clone()));
        }
        // The global system prompt from config.toml sits at the very front.
        let sys = self.config.api.system_prompt.trim();
        if !sys.is_empty() {
            messages.insert(0, ChatMessage::system(sys.to_string()));
        }
        let request = ChatRequest::new(self.current_model(), messages)
            .with_reasoning_effort(self.reasoning_effort.clone());

        // Offline mock backend: scripted, tool-driving reply, no network.
        if self.mock {
            return Some(Action::AttachStream(sid, crate::api::mock::stream(&request)));
        }

        match self.api.as_ref() {
            Some(client) => match client.stream(request) {
                Ok(rx) => Some(Action::AttachStream(sid, rx)),
                Err(e) => {
                    if let Some(s) = self.sessions.by_id_mut(sid) { s.finalize_assistant_stream(); }
                    self.set_status(format!("Request failed: {}", e));
                    None
                }
            },
            None => {
                if let Some(s) = self.sessions.by_id_mut(sid) { s.finalize_assistant_stream(); }
                self.set_status("No API client");
                None
            }
        }
    }

    // ── Agent tool loop ─────────────────────────────────────────────────────

    /// A stream for `sid` finished. If that session is in agent mode and emitted
    /// tool calls, start (or queue) its tool round. Rounds are serialized: only one
    /// session runs tools at a time, so parallel sessions share one permission UI.
    pub fn maybe_start_agent_round(&mut self, sid: usize) -> Option<Action> {
        let has_tools = !self.tool_calls_in(sid).is_empty();
        if !has_tools {
            // Nothing to run for this session; let a queued session take over.
            return self.start_next_queued_round();
        }
        let agent = self.sessions.by_id(sid).map(|s| s.agent_mode).unwrap_or(false);
        if !agent {
            let n = self.tool_calls_in(sid).len();
            self.set_status(format!("⚠ {} tool call(s) not run — agent mode OFF (Ctrl-A)", n));
            return None;
        }
        if self.agent_session.is_some() && self.agent_session != Some(sid) {
            // Another session is mid-round; wait our turn.
            if !self.agent_queue.contains(&sid) {
                self.agent_queue.push_back(sid);
            }
            return None;
        }
        self.start_agent_round_for(sid)
    }

    fn start_agent_round_for(&mut self, sid: usize) -> Option<Action> {
        let calls = self.tool_calls_in(sid);
        if calls.is_empty() {
            self.agent_session = None;
            return self.start_next_queued_round();
        }
        self.agent_iterations += 1;
        if self.agent_iterations > MAX_AGENT_ITERATIONS {
            self.agent_iterations = 0;
            self.agent_session = None;
            self.set_status(format!("⚠ Agent stopped after {} rounds (loop guard).", MAX_AGENT_ITERATIONS));
            return self.start_next_queued_round();
        }
        self.agent_session = Some(sid);
        self.pending_tools = calls.into();
        self.process_next_tool()
    }

    fn start_next_queued_round(&mut self) -> Option<Action> {
        while let Some(sid) = self.agent_queue.pop_front() {
            if self.sessions.has_id(sid) {
                self.agent_iterations = 0;
                return self.start_agent_round_for(sid);
            }
        }
        None
    }

    /// The session the current tool round belongs to (falls back to active).
    fn agent_sid(&self) -> usize {
        self.agent_session.unwrap_or_else(|| self.sessions.active_id())
    }

    pub(super) fn tool_calls_in(&self, sid: usize) -> Vec<ToolCall> {
        let Some(session) = self.sessions.by_id(sid) else { return Vec::new() };
        let last = session.messages.iter().rev().find(|m| m.role == "assistant");
        match last {
            Some(m) => parse_blocks(&message_text(m))
                .into_iter()
                .filter_map(|b| match b {
                    crate::domain::blocks::Block::ToolCall(c) => Some(c),
                    _ => None,
                })
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn process_next_tool(&mut self) -> Option<Action> {
        while let Some(call) = self.pending_tools.pop_front() {
            let kind = match call.kind() {
                Some(k) => k,
                None => {
                    let res = ToolResult::failure(call.clone(), format!("Unknown tool: {}", call.name), 0);
                    self.record_tool_result(res);
                    continue;
                }
            };
            match self.permissions.check(&kind) {
                Some(Permission::Deny) | Some(Permission::DenyAll) => {
                    let res = ToolResult::failure(call.clone(), "Skipped: denied by session policy".into(), 0);
                    self.record_tool_result(res);
                }
                Some(Permission::Allow) | Some(Permission::AllowAll) => {
                    return self.execute_tool(call);
                }
                None => {
                    self.overlay = Overlay::Permission(PermissionRequest { call, selected: 0 });
                    self.set_status("Permission required — a/A allow · d/D deny");
                    return None;
                }
            }
        }
        self.continue_after_tools()
    }

    pub fn resolve_permission(&mut self, perm: Permission) -> Option<Action> {
        let call = match &self.overlay {
            Overlay::Permission(req) => req.call.clone(),
            _ => return None,
        };
        self.overlay = Overlay::None;
        let kind = call.kind();
        match perm {
            Permission::Allow => self.execute_tool(call),
            Permission::AllowAll => {
                if let Some(k) = kind {
                    self.permissions.remember_allow(k);
                }
                self.execute_tool(call)
            }
            Permission::Deny => {
                let res = ToolResult::failure(call, "Denied by user".into(), 0);
                self.record_tool_result(res);
                self.process_next_tool()
            }
            Permission::DenyAll => {
                if let Some(k) = kind {
                    self.permissions.remember_deny(k);
                }
                let res = ToolResult::failure(call, "Denied by user (all)".into(), 0);
                self.record_tool_result(res);
                self.process_next_tool()
            }
        }
    }

    fn execute_tool(&mut self, call: ToolCall) -> Option<Action> {
        self.set_status(format!("⚙ Running: {}", call.summary()));
        // Run in the owning session's working directory (the process cwd tracks the
        // active session, which may differ when a background session runs tools).
        let sid = self.agent_sid();
        let cwd = self
            .sessions
            .by_id(sid)
            .and_then(|s| s.cwd.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let (tx, rx) = mpsc::channel(1);
        self.agent_tool_rx = Some(rx);
        tokio::task::spawn_blocking(move || {
            let _ = tx.blocking_send(agent::execute(call, &cwd));
        });
        None
    }

    pub fn record_tool_result(&mut self, result: ToolResult) {
        if result.is_ok() {
            self.track_edited_file(&result.call);
        }
        let icon = result.call.kind().map(|k| k.icon()).unwrap_or("⚙");
        let status = if result.is_ok() { "ok" } else { "error" };
        let body = format!("[tool-result] {} {} ({})\n{}", icon, result.call.summary(), status, result.text());
        let sid = self.agent_sid();
        if let Some(s) = self.sessions.by_id_mut(sid) {
            s.push_message(ChatMessage::tool(body));
        }
        if sid == self.sessions.active_id() {
            self.chat.stick_bottom = true;
        }
        self.touch();
    }

    /// Maintain the recently-edited-files list (most recent first) from a
    /// successful mutating tool call, so the user can jump back into them.
    fn track_edited_file(&mut self, call: &ToolCall) {
        let mutates = matches!(call.name.as_str(), "write_file" | "edit_file" | "append_file" | "delete_file");
        if !mutates {
            return;
        }
        let Some(path) = call.args.get("path").and_then(|v| v.as_str()) else { return };
        let path = path.trim_start_matches("./").to_string();
        self.edited_files.retain(|p| p != &path);
        if call.name == "delete_file" {
            return; // removed from the list, nothing to add back
        }
        self.edited_files.insert(0, path);
        self.edited_files.truncate(50);
    }

    /// All queued tools for the current round ran; hand back to the model with a
    /// fresh streaming turn for the same session. The round is over, so clear the
    /// agent slot (the new stream will re-enter via `StreamDone`).
    fn continue_after_tools(&mut self) -> Option<Action> {
        let sid = self.agent_sid();
        self.agent_session = None;
        self.begin_stream_for(sid)
    }
}

fn build_user_message(text: &str, attachment: Option<&PathBuf>, app: &mut App) -> ChatMessage {
    let Some(path) = attachment else {
        return ChatMessage::user(text.to_string());
    };
    if crate::files::is_image(path) {
        match crate::files::load_image_base64(path) {
            Ok((b64, mime)) => return ChatMessage::user_with_image(text, &b64, &mime),
            Err(e) => {
                app.set_status(format!("Image load error: {}", e));
                return ChatMessage::user(text.to_string());
            }
        }
    }
    match crate::files::read_text(path) {
        Ok(content) => {
            let combined = if text.trim().is_empty() {
                format!("```\n{}\n```", content)
            } else {
                format!("```\n{}\n```\n\n{}", content, text)
            };
            ChatMessage::user(combined)
        }
        Err(e) => {
            app.set_status(format!("File read error: {}", e));
            ChatMessage::user(text.to_string())
        }
    }
}

/// Build the full chat document (blocks → rows) from the current session.
fn build_chat_doc(app: &App, width: usize, theme: &Theme, _rev: u64) -> Vec<RenderedLine> {
    let session = app.sessions.active();
    let mut docs: Vec<DocMessage> = Vec::with_capacity(session.messages.len() + 1);

    for m in &session.messages {
        let blocks = if m.role == "tool" {
            vec![parse_tool_result(&message_text(m))]
        } else {
            parse_blocks(&message_text(m))
        };
        docs.push(DocMessage { role: m.role.clone(), blocks });
    }

    if let Some(partial) = session.streaming_display() {
        docs.push(DocMessage { role: "assistant".into(), blocks: parse_blocks(&partial) });
    }

    if docs.is_empty() {
        return welcome_doc(theme);
    }
    let streaming = session.is_streaming();
    build(&docs, width, theme, &app.chat.toggled, app.show_output, streaming)
}

/// The empty-state splash, shown when there are no messages.
fn welcome_doc(theme: &Theme) -> Vec<RenderedLine> {
    let intro = vec![
        DocMessage {
            role: "assistant".into(),
            blocks: vec![crate::domain::blocks::Block::Markdown(
                "# AiTUI\nYour terminal coding agent.\n\n- **@path** — mention a file into context\n- **/** — open the command palette\n- **i … :w** — type a message, then send\n- **Ctrl-A** — toggle agent mode (read/edit/run with approval)\n- **?** — full keybinding help"
                    .into(),
            )],
        },
    ];
    // Build with an empty toggle set; reuse the normal builder for consistent styling.
    let mut rows = build(&intro, 70, theme, &std::collections::HashSet::new(), false, false);
    // Drop the role header for a cleaner splash.
    if !rows.is_empty() {
        rows.remove(0);
    }
    rows
}
