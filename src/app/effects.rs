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
    /// Rebuild the chat document if the cache is stale, then keep the cursor valid.
    pub fn sync_chat_doc(&mut self, width: usize, viewport_h: usize) {
        if self.chat.needs_rebuild(self.content_rev, width) {
            let doc = build_chat_doc(self, width, &self.theme(), self.content_rev);
            self.chat.set_doc(doc, self.content_rev, width, viewport_h);
        }
    }

    // ── Submission ──────────────────────────────────────────────────────────

    pub fn submit(&mut self) -> Option<Action> {
        self.agent_iterations = 0;
        self.mention.reset();
        let text = self.input.take();
        let attachment = self.attachment.take();

        if text.trim().is_empty() && attachment.is_none() {
            self.set_status("Nothing to send. Type a message first.");
            return None;
        }

        let mention_ctx = expand_mentions(&text);
        let text = if mention_ctx.is_empty() { text } else { format!("{}\n\n{}", mention_ctx, text) };

        let msg = build_user_message(&text, attachment.as_ref(), self);
        self.sessions.active_mut().push_message(msg);
        self.auto_name_session();
        self.touch();

        self.begin_stream()
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

    fn begin_stream(&mut self) -> Option<Action> {
        self.sessions.active_mut().begin_assistant_stream();
        self.set_status("Generating…");
        self.chat.stick_bottom = true;
        self.touch();

        let messages = self.sessions.active().api_messages();
        let request = ChatRequest::new(self.current_model(), messages);
        match self.api.as_ref() {
            Some(client) => match client.stream(request) {
                Ok(rx) => Some(Action::AttachStream(rx)),
                Err(e) => {
                    self.sessions.active_mut().finalize_assistant_stream();
                    self.set_status(format!("Request failed: {}", e));
                    None
                }
            },
            None => {
                self.sessions.active_mut().finalize_assistant_stream();
                self.set_status("No API client");
                None
            }
        }
    }

    // ── Agent tool loop ─────────────────────────────────────────────────────

    pub fn start_agent_round(&mut self) -> Option<Action> {
        let calls = self.tool_calls_in_last_assistant();
        if calls.is_empty() {
            return None;
        }
        self.agent_iterations += 1;
        if self.agent_iterations > MAX_AGENT_ITERATIONS {
            self.agent_iterations = 0;
            self.set_status(format!("⚠ Agent stopped after {} rounds (loop guard).", MAX_AGENT_ITERATIONS));
            return None;
        }
        self.pending_tools = calls.into();
        self.process_next_tool()
    }

    fn tool_calls_in_last_assistant(&self) -> Vec<ToolCall> {
        let last = self.sessions.active().messages.iter().rev().find(|m| m.role == "assistant");
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
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let (tx, rx) = mpsc::channel(1);
        self.agent_tool_rx = Some(rx);
        tokio::task::spawn_blocking(move || {
            let _ = tx.blocking_send(agent::execute(call, &cwd));
        });
        None
    }

    pub fn record_tool_result(&mut self, result: ToolResult) {
        let icon = result.call.kind().map(|k| k.icon()).unwrap_or("⚙");
        let status = if result.is_ok() { "ok" } else { "error" };
        let body = format!("[tool-result] {} {} ({})\n{}", icon, result.call.summary(), status, result.text());
        self.sessions.active_mut().push_message(ChatMessage::tool(body));
        self.chat.stick_bottom = true;
        self.touch();
    }

    fn continue_after_tools(&mut self) -> Option<Action> {
        self.begin_stream()
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
    build(&docs, width, theme, &app.chat.toggled)
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
    let mut rows = build(&intro, 70, theme, &std::collections::HashSet::new());
    // Drop the role header for a cleaner splash.
    if !rows.is_empty() {
        rows.remove(0);
    }
    rows
}
