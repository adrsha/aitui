//! Side effects: composing the chat document, starting model requests, and the
//! agent tool-execution loop. These methods may return a follow-up `Action`
//! (e.g. attach a freshly spawned stream) for the reducer to process.

use std::collections::BTreeSet;
use std::path::PathBuf;
use tokio::sync::mpsc;

use crate::agent::{
    self, AccessVerdict, Permission, PermissionDecision, PermissionScope, ToolCall, ToolKind,
    ToolResult,
};
use crate::api::models::MessageContent;
use crate::api::{ApiClient, ChatMessage, ChatRequest};
use crate::app::state::JudgeBatch;
use crate::app::action::Action;
use crate::app::overlay::{
    DecisionRequest, Overlay, PermissionRequest, PlanRequest, PERMISSION_OPTIONS,
};
use crate::app::state::{expand_mentions, App, MAX_AGENT_ITERATIONS};
use crate::domain::blocks::{parse_blocks, parse_tool_result};
use crate::domain::session::MAX_COLD_STREAM_RETRIES;
use crate::render::document::{build_message, DocMessage, LoadingKind, RenderedLine};
use crate::render::theme::Theme;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Plain display text for a stored message.
fn message_text(m: &ChatMessage) -> String {
    match &m.content {
        MessageContent::Text(t) => t.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .map(|p| match p {
                crate::api::models::ContentPart::Text { text } => text.clone(),
                crate::api::models::ContentPart::ImageUrl { .. } => {
                    "🖼 [image attached]".to_string()
                }
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
            let doc = self.build_chat_doc(width);
            self.chat.set_doc(doc, self.content_rev, width, viewport_h);
        }
    }

    /// Assemble the chat document, reusing per-message cached rows for every
    /// message whose content signature is unchanged. Only cache misses (in
    /// practice: the streaming message, plus anything just appended or toggled)
    /// pay the parse + highlight + wrap cost. The in-progress streaming partial is
    /// always rebuilt fresh and never cached.
    fn build_chat_doc(&mut self, width: usize) -> Vec<RenderedLine> {
        let theme = self.theme();
        let show_output = self.show_output;

        // Disjoint field borrows: cache (mut), toggled + sessions (shared).
        let cache = &mut self.doc_cache;
        let toggled = &self.chat.toggled;
        let session = self.sessions.active();
        let active_tool = self.active_tool.clone();
        let active_tool_for_session = self.agent_session == Some(session.id);

        cache.reset_if_env_changed(width, show_output);
        cache.truncate(session.messages.len());

        let mut out: Vec<RenderedLine> = Vec::new();
        for (mi, m) in session.messages.iter().enumerate() {
            let text = message_text(m);
            let sig = message_sig(&m.role, &text, m.duration_ms, toggled, mi);
            if let Some(rows) = cache.get(mi, sig) {
                out.extend_from_slice(rows);
            } else {
                let blocks = if m.role == "tool" {
                    vec![parse_tool_result(&text)]
                } else {
                    parse_blocks(&text)
                };
                let doc_msg = DocMessage {
                    role: m.role.clone(),
                    blocks,
                    duration_ms: m.duration_ms,
                    first_ms: m.first_ms,
                    loading: None,
                    started_at: None,
                };
                // Finalized messages don't animate — pass streaming=false so a
                // finished thinking block isn't spinning (and stays cacheable).
                let rows = build_message(&doc_msg, mi, width, &theme, toggled, show_output, false);
                out.extend_from_slice(&rows);
                cache.put(mi, sig, rows);
            }
        }

        if active_tool_for_session {
            if let Some((summary, started_at)) = active_tool {
                let mi = session.messages.len();
                let doc_msg = DocMessage {
                    role: "tool".into(),
                    blocks: vec![crate::domain::blocks::Block::Markdown(format!(
                        "⚙ About to run `{}` — executing the requested tool now.",
                        summary
                    ))],
                    duration_ms: None,
                    first_ms: None,
                    loading: Some(LoadingKind::Tool),
                    started_at: Some(started_at),
                };
                out.extend(build_message(
                    &doc_msg,
                    mi,
                    width,
                    &theme,
                    toggled,
                    show_output,
                    false,
                ));
            }
        }

        // The live streaming partial: rebuilt every frame (its text changes each
        // token), appended after the cached history, with the spinner animating.
        if let Some(partial) = session.streaming_display() {
            let mi = session.messages.len();
            let loading = if partial.is_empty() {
                LoadingKind::Network
            } else {
                LoadingKind::Streaming
            };
            let doc_msg = DocMessage {
                role: "assistant".into(),
                blocks: parse_blocks(&partial),
                duration_ms: None,
                // Time-to-first-result, fixed once the first byte lands (None while
                // still waiting → the header shows a live "waiting" timer).
                first_ms: session.pending_first_ms(),
                loading: Some(loading),
                started_at: session.pending_started_at,
            };
            out.extend(build_message(
                &doc_msg,
                mi,
                width,
                &theme,
                toggled,
                show_output,
                true,
            ));
        }

        if out.is_empty() {
            return welcome_doc(&theme, width);
        }
        out
    }

    // ── Smart paste ─────────────────────────────────────────────────────────

    /// Handle a bracketed paste. Big blobs are written to a file and attached so
    /// they don't flood the composer; medium blobs are stored and shown as a
    /// compact `[PASTED#N-…]` chip (expanded to full text on submit); small pastes
    /// are inserted verbatim.
    pub fn smart_paste(&mut self, text: String) {
        // Thresholds. A 12k-char paste is a chip; only very large pastes → file.
        const FILE_CHARS: usize = 50_000;
        const CHIP_CHARS: usize = 400;
        const CHIP_LINES: usize = 5;

        let lines = text.lines().count().max(1);
        let chars = text.chars().count();

        if chars >= FILE_CHARS {
            match write_paste_file(&text) {
                Ok(path) => {
                    self.attachment = Some(path);
                    self.set_status(format!(
                        "🖇 Large paste attached as file ({} lines, {} chars)",
                        lines, chars
                    ));
                }
                Err(e) => {
                    self.set_status(format!("Paste file error: {} — pasted inline", e));
                    self.input.paste(&text);
                    self.update_mention();
                }
            }
        } else if chars >= CHIP_CHARS || lines >= CHIP_LINES {
            self.pastes.push(text);
            let n = self.pastes.len();
            let token = format!("[PASTED#{}-{}lines-{}chars]", n, lines, chars);
            self.input.paste(&token);
            self.set_status(format!(
                "Pasted {} lines, {} chars — expands on send",
                lines, chars
            ));
        } else {
            self.input.paste(&text);
            self.update_mention();
        }
    }

    /// Replace every `[PASTED#N-…]` chip in `text` with its stored blob, then clear
    /// the paste store (the turn consumes them). Unknown/edited chips are left as-is.
    fn expand_pastes(&mut self, text: String) -> String {
        if self.pastes.is_empty() || !text.contains("[PASTED#") {
            self.pastes.clear();
            return text;
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text.as_str();
        while let Some(start) = rest.find("[PASTED#") {
            out.push_str(&rest[..start]);
            let after = &rest[start..];
            let Some(end) = after.find(']') else {
                out.push_str(after);
                rest = "";
                break;
            };
            let token = &after[..=end]; // "[PASTED#N-…]"
            let n: Option<usize> = token
                .strip_prefix("[PASTED#")
                .and_then(|s| s.split('-').next())
                .and_then(|d| d.parse().ok());
            match n.and_then(|n| self.pastes.get(n.saturating_sub(1))) {
                Some(blob) => out.push_str(blob),
                None => out.push_str(token), // unknown index → leave the chip text
            }
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        self.pastes.clear();
        out
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
        // Restore any `[PASTED#N-…]` chips to their full text before sending.
        let text = self.input.take();
        let text = self.expand_pastes(text);
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
        // The composed text is now a real message; clear the session's stashed
        // draft so a stale copy isn't persisted or restored later.
        self.sessions.active_mut().draft.clear();

        let mention_ctx = expand_mentions(&text);
        let text = if mention_ctx.is_empty() {
            text
        } else {
            format!("{}\n\n{}", mention_ctx, text)
        };

        let msg = build_user_message(&text, attachment.as_ref(), self);
        self.sessions.active_mut().push_message(msg);
        self.auto_name_session();
        self.touch();

        let sid = self.sessions.active_id();
        self.begin_stream_for(sid)
    }

    /// Regenerate the last assistant turn: drop everything after the last user
    /// message and resend. Blocked while a stream is in flight.
    pub fn retry_last(&mut self) -> Option<Action> {
        if self.is_busy() {
            self.set_status("Can't retry — assistant is working (Ctrl-C to cancel)");
            return None;
        }
        let sid = self.sessions.active_id();
        if !self.sessions.active_mut().trim_after_last_user() {
            self.set_status("Nothing to retry — no previous message");
            return None;
        }
        self.agent_iterations = 0;
        self.mention.reset();
        self.chat.stick_bottom = true;
        self.touch();
        self.begin_stream_for(sid)
    }

    /// Pull the last user message back into the composer (removing that turn and
    /// its reply) so it can be tweaked and resent. Blocked while streaming.
    pub fn edit_last(&mut self) {
        if self.is_busy() {
            self.set_status("Can't edit — assistant is working (Ctrl-C to cancel)");
            return;
        }
        match self.sessions.active_mut().take_last_user_turn() {
            Some(text) => {
                self.input.set_text(&text);
                self.vim = crate::input::vim::VimMode::Insert;
                self.mention.reset();
                self.chat.stick_bottom = true;
                self.touch();
                self.set_status("Editing last message — Enter to resend");
            }
            None => self.set_status("Nothing to edit — no previous message"),
        }
    }

    /// Queue the last assistant reply for the system clipboard (OSC 52).
    pub fn copy_last_reply(&mut self) {
        match self.sessions.active().last_assistant_text() {
            Some(t) if !t.trim().is_empty() => {
                let n = t.chars().count();
                self.pending_clipboard = Some(t);
                self.set_status(format!("Copied reply to clipboard ({} chars)", n));
            }
            _ => self.set_status("No assistant reply to copy"),
        }
    }

    /// Queue the last fenced code block from the last reply for the clipboard.
    pub fn copy_last_code(&mut self) {
        let code = self
            .sessions
            .active()
            .last_assistant_text()
            .and_then(|t| crate::domain::blocks::last_code_block(&t));
        match code {
            Some(c) => {
                let lines = c.lines().count().max(1);
                self.pending_clipboard = Some(c);
                self.set_status(format!("Copied code block to clipboard ({} lines)", lines));
            }
            None => self.set_status("No code block in the last reply to copy"),
        }
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

    /// Stash the live composer text into the active session so it persists and is
    /// restored on return. Call before switching away or saving on quit.
    pub fn stash_draft(&mut self) {
        let text = self.input.text();
        self.sessions.active_mut().draft = text;
    }

    /// Load the (now-)active session's stashed draft into the composer. Call right
    /// after switching sessions.
    pub fn load_active_draft(&mut self) {
        let draft = self.sessions.active().draft.clone();
        self.input.set_text(&draft);
        self.input_history_idx = None;
        self.input_draft.clear();
    }

    pub fn begin_stream_for(&mut self, sid: usize) -> Option<Action> {
        // TODO(audit): move request assembly into a pure builder so streaming,
        // image routing, skills, tools, and context-window policy can be tested separately.
        // Fresh turn: bump the epoch (so any speculative result still in flight from
        // the previous turn is dropped, not served stale) and drop its state.
        self.spec_epoch = self.spec_epoch.wrapping_add(1);
        self.spec_dispatched.clear();
        self.spec_results.clear();
        self.start_stream_for(sid, None)
    }

    pub fn retry_cold_stream(&mut self, sid: usize, cold_retries: u8) -> Option<Action> {
        if let Some(s) = self.sessions.by_id_mut(sid) {
            s.cancel_empty_assistant_stream();
        }
        self.streams.retain(|h| h.session_id != sid);
        self.set_status(format!(
            "No response yet — retrying request ({}/{})…",
            cold_retries, MAX_COLD_STREAM_RETRIES
        ));
        self.touch();
        self.start_stream_for(sid, Some(cold_retries))
    }

    fn start_stream_for(&mut self, sid: usize, cold_retries: Option<u8>) -> Option<Action> {
        let is_mock = self.is_mock();
        let session = self.sessions.by_id_mut(sid)?;
        session.begin_assistant_stream();
        session.pending_mock = is_mock;
        // The animated status-bar spinner ("working") is the generating indicator
        // now — don't set a free-text "Generating…" that later messages clobber.
        self.status = None;
        self.touch();

        // Image-generation models use a different endpoint (chat completions 503s
        // them). Route to /v1/images/generations with the last user message as the
        // prompt; the result comes back over the same stream channel.
        let model = self.current_model().to_string();
        if crate::api::is_image_model(&model) && !self.is_mock() {
            let prompt = self
                .sessions
                .by_id(sid)
                .and_then(|s| s.messages.iter().rev().find(|m| m.role == "user"))
                .map(message_text)
                .unwrap_or_default();
            if prompt.trim().is_empty() {
                if let Some(s) = self.sessions.by_id_mut(sid) {
                    s.finalize_assistant_stream();
                }
                self.set_status("Nothing to generate — describe the image first.");
                return None;
            }
            return match self.api.as_ref() {
                Some(client) => match client.generate_image(&model, &prompt) {
                    Ok((rx, img_path)) => {
                        self.pending_image = Some(img_path.into());
                        match cold_retries {
                            Some(n) => Some(Action::AttachRetriedStream(sid, rx, n)),
                            None => Some(Action::AttachStream(sid, rx)),
                        }
                    }
                    Err(e) => {
                        if let Some(s) = self.sessions.by_id_mut(sid) {
                            s.finalize_assistant_stream();
                        }
                        self.set_status(format!("Image request failed: {}", e));
                        None
                    }
                },
                None => {
                    if let Some(s) = self.sessions.by_id_mut(sid) {
                        s.finalize_assistant_stream();
                    }
                    self.set_status("No API client");
                    None
                }
            };
        }

        // Proactive sliding window: keep the sent history to ~75% of the model's
        // context (≈4 chars/token) so there's headroom for the reply plus the
        // system/skill prompts prepended below. Oldest turns fall off silently.
        let char_budget = (self.config.ui.context_window as usize).saturating_mul(3);
        let mut messages = self
            .sessions
            .by_id(sid)
            .map(|s| s.api_messages_windowed(true, Some(char_budget)))
            .unwrap_or_default();
        self.skills = crate::skills::reload_preserving_active(&self.skills);
        // Loop-mode directive: tell the model it's working autonomously and how to end.
        if let Some(l) = self.sessions.by_id(sid).and_then(|s| s.loop_state.as_ref()) {
            let directive = format!(
                "AUTONOMOUS LOOP MODE is active. You are working toward a goal across \
                 multiple turns without waiting for the user between them.\n\
                 GOAL: {}\n\
                 STOP CRITERIA: {}\n\
                 Each turn, make concrete, verifiable progress using tools (read/edit/\
                 write/shell/etc). Do not just describe what you would do — do it. When \
                 (and ONLY when) the STOP CRITERIA are fully and verifiably met, call the \
                 `finish` tool with a short summary to end the loop. If you become truly \
                 blocked and cannot proceed, call `finish` explaining why. You are on \
                 iteration {} of at most {}.",
                l.goal, l.stop, l.iteration + 1, l.max
            );
            prepend_or_merge_system(&mut messages, directive);
        }
        if let Some(skill_prompt) = active_skills_prompt(&self.skills) {
            prepend_or_merge_system(&mut messages, skill_prompt);
        }
        // The global system prompt from config.toml sits at the very front.
        let sys = self.config.api.system_prompt.trim();
        if !sys.is_empty() {
            prepend_or_merge_system(&mut messages, sys.to_string());
        }
        let mut request = ChatRequest::new(self.current_model(), messages)
            .with_reasoning_effort(self.reasoning_effort.clone());
        // Send tool schemas so the model returns structured tool_calls instead of
        // ```tool fences (agent turns only).
        if self
            .sessions
            .by_id(sid)
            .map(|s| s.agent_mode)
            .unwrap_or(false)
        {
            request = request.with_tools(crate::agent::tool_schemas());
        }

        // Offline mock backend: scripted, tool-driving reply, no network.
        if self.is_mock() {
            let rx = crate::api::mock::stream(&request);
            return match cold_retries {
                Some(n) => Some(Action::AttachRetriedStream(sid, rx, n)),
                None => Some(Action::AttachStream(sid, rx)),
            };
        }

        match self.api.as_ref() {
            Some(client) => match client.stream(request) {
                Ok(rx) => match cold_retries {
                    Some(n) => Some(Action::AttachRetriedStream(sid, rx, n)),
                    None => Some(Action::AttachStream(sid, rx)),
                },
                Err(e) => {
                    if let Some(s) = self.sessions.by_id_mut(sid) {
                        s.finalize_assistant_stream();
                    }
                    self.set_status(format!("Request failed: {}", e));
                    None
                }
            },
            None => {
                if let Some(s) = self.sessions.by_id_mut(sid) {
                    s.finalize_assistant_stream();
                }
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
            // The turn ended with a plain reply. If this session is running an
            // autonomous loop, keep it going (or stop it at the cap).
            if let Some(follow) = self.maybe_continue_loop(sid) {
                return Some(follow);
            }
            // Nothing to run for this session; let a queued session take over.
            return self.start_next_queued_round();
        }
        let agent = self
            .sessions
            .by_id(sid)
            .map(|s| s.agent_mode)
            .unwrap_or(false);
        if !agent {
            // Agent mode is off but the model asked for tools: offer to enable agent
            // mode and run them, or decline and let it answer without.
            let count = self.tool_calls_in(sid).len();
            self.overlay = Overlay::ToolRequest(crate::app::overlay::ToolRequest { sid, count });
            self.set_status("Model wants tools — y enable agent & run · n answer without");
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

    /// Accept a tool request from a non-agent session: turn on agent mode and run
    /// the pending tool call(s).
    pub fn enable_agent_and_run(&mut self) -> Option<Action> {
        let sid = match &self.overlay {
            Overlay::ToolRequest(r) => r.sid,
            _ => return None,
        };
        self.overlay = Overlay::None;
        if let Some(s) = self.sessions.by_id_mut(sid) {
            s.agent_mode = true;
        }
        self.sessions.save();
        self.set_status("◇ Agent mode ON — running the requested tool(s)");
        self.start_agent_round_for(sid)
    }

    /// Decline a tool request: leave agent mode off, tell the model the tools were
    /// declined, and let it answer without them.
    pub fn decline_agent_tools(&mut self) -> Option<Action> {
        let sid = match &self.overlay {
            Overlay::ToolRequest(r) => r.sid,
            _ => return None,
        };
        self.overlay = Overlay::None;
        if let Some(s) = self.sessions.by_id_mut(sid) {
            s.push_message(ChatMessage::user(
                "(You requested a tool, but agent mode is off and the user declined. \
                 Answer directly without using any tools.)",
            ));
        }
        self.touch();
        self.set_status("Declined — the model will answer without tools");
        self.begin_stream_for(sid)
    }

    fn start_agent_round_for(&mut self, sid: usize) -> Option<Action> {
        let calls = self.tool_calls_in(sid);
        if calls.is_empty() {
            self.agent_session = None;
            return self.start_next_queued_round();
        }
        if agent_loop_guard_reached(self.agent_iterations) {
            self.agent_iterations = 0;
            self.agent_session = None;
            self.set_status(format!(
                "⚠ Agent stopped after {} rounds (loop guard).",
                MAX_AGENT_ITERATIONS
            ));
            return self.start_next_queued_round();
        }
        self.agent_iterations += 1;
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
        self.agent_session
            .unwrap_or_else(|| self.sessions.active_id())
    }

    pub(super) fn tool_calls_in(&self, sid: usize) -> Vec<ToolCall> {
        let Some(session) = self.sessions.by_id(sid) else {
            return Vec::new();
        };
        let last = session
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant");
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
        let cwd = self.agent_cwd();
        // Calls already cleared this round (judged-allow or batch-allow) run first,
        // straight to execution with no fresh permission check or re-judge.
        if let Some(call) = self.approved.pop_front() {
            return self.execute_tool(call);
        }
        while let Some(call) = self.pending_tools.pop_front() {
            let Some(kind) = call.kind() else {
                let res =
                    ToolResult::failure(call.clone(), format!("Unknown tool: {}", call.name), 0);
                self.record_tool_result(res);
                continue;
            };
            if kind == ToolKind::Todo {
                let res = self.apply_todo(&call);
                self.record_tool_result(res);
                continue;
            }
            if kind == ToolKind::Ask {
                return self.open_decision(call);
            }
            if kind == ToolKind::Plan {
                return self.open_plan(call);
            }
            if kind == ToolKind::Finish {
                let res = self.apply_finish(&call);
                self.record_tool_result(res);
                continue;
            }
            match self.permissions.check(&call, &cwd) {
                Some(PermissionDecision::Deny) => {
                    let res = ToolResult::failure(
                        call.clone(),
                        "Skipped: denied by session policy".into(),
                        0,
                    );
                    self.record_tool_result(res);
                }
                Some(PermissionDecision::Allow) => {
                    return self.execute_tool(call);
                }
                None => {
                    let mut calls = vec![call];
                    while let Some(next) = self.pending_tools.front() {
                        if next.kind().is_none()
                            || matches!(
                                next.kind(),
                                Some(ToolKind::Todo | ToolKind::Ask | ToolKind::Plan)
                            )
                        {
                            break;
                        }
                        if self.permissions.check(next, &cwd).is_some() {
                            break;
                        }
                        calls.push(self.pending_tools.pop_front().unwrap());
                    }
                    // With a session access policy set, let the judge model triage
                    // the batch before bothering the human — unless we can't reach a
                    // model (mock/offline) or every call is on the safety floor.
                    if self.permissions.policy.is_some()
                        && self.can_judge()
                        && self.batch_has_judgeable(&calls, &cwd)
                    {
                        return self.begin_judge(calls);
                    }
                    return self.prompt_permission(calls);
                }
            }
        }
        self.continue_after_tools()
    }

    /// Apply a permission choice from the menu (or a quick allow/deny). Choices
    /// broader than "once" are recorded as a session rule so the same
    /// kind/directory/timed decision auto-applies for the rest of the session.
    pub fn resolve_permission(&mut self, perm: Permission) -> Option<Action> {
        let calls = match &self.overlay {
            Overlay::Permission(req) => req.calls.clone(),
            _ => return None,
        };
        self.overlay = Overlay::None;

        let allow = matches!(
            perm,
            Permission::Allow
                | Permission::AllowKind
                | Permission::AllowDirectory
                | Permission::AllowTimed
        );
        let decision = if allow {
            PermissionDecision::Allow
        } else {
            PermissionDecision::Deny
        };
        let cwd = self.agent_cwd();

        for call in &calls {
            match perm {
                Permission::AllowKind | Permission::DenyKind => {
                    if let Some(k) = call.kind() {
                        self.permissions
                            .remember_rule(decision, PermissionScope::Kind(k), false);
                    }
                }
                Permission::AllowDirectory | Permission::DenyDirectory => {
                    if let Some(dir) = call.permission_directory(&cwd) {
                        self.permissions.remember_rule(
                            decision,
                            PermissionScope::Directory(dir),
                            false,
                        );
                    }
                }
                Permission::AllowTimed | Permission::DenyTimed => {
                    self.permissions
                        .remember_rule(decision, PermissionScope::Timed, false);
                }
                Permission::Allow | Permission::Deny => {}
            }
        }

        if allow {
            let mut calls = calls;
            let first = calls.remove(0);
            for call in calls.into_iter().rev() {
                self.pending_tools.push_front(call);
            }
            self.execute_tool(first)
        } else {
            for call in calls {
                let res = ToolResult::failure(call, "Denied by user".into(), 0);
                self.record_tool_result(res);
            }
            self.process_next_tool()
        }
    }

    /// Show the permission prompt for a batch the judge couldn't clear (or when no
    /// policy is set). Shared by the plain path, the judge's "ask" verdicts, and a
    /// re-judge that still needs the human.
    fn prompt_permission(&mut self, calls: Vec<ToolCall>) -> Option<Action> {
        self.notify_desktop(
            "AiTUI — access needed",
            format!(
                "Allow {} pending tool call{}?",
                calls.len(),
                if calls.len() == 1 { "" } else { "s" }
            ),
        );
        self.overlay = Overlay::Permission(PermissionRequest::new(calls));
        self.set_status(
            "Access — ↑↓ option · a allow · d deny · e edit · p policy · ⏎ run · Esc cancel",
        );
        None
    }

    /// Whether a live model is reachable to run the access judge (never in mock /
    /// offline mode — there's nothing to ask).
    fn can_judge(&self) -> bool {
        !self.is_mock() && !self.config.api.endpoint.trim().is_empty()
    }

    /// Whether any call in the batch is eligible for the judge — i.e. not on the
    /// safety floor. If every call is floored, judging is pointless: prompt directly.
    fn batch_has_judgeable(&self, calls: &[ToolCall], cwd: &PathBuf) -> bool {
        calls.iter().any(|c| !agent::needs_hard_prompt(c, cwd))
    }

    /// Spawn the async access-policy judge for `calls`. The fast judge model
    /// classifies each call allow/deny/ask against the session policy; verdicts
    /// return over `judge_rx` as an `AccessJudged` action. The batch is stashed in
    /// `self.judging` meanwhile.
    fn begin_judge(&mut self, calls: Vec<ToolCall>) -> Option<Action> {
        let sid = self.agent_sid();
        let cwd = self.agent_cwd();
        let policy = self.permissions.policy.clone().unwrap_or_default();
        let descs: Vec<(usize, String)> = calls
            .iter()
            .enumerate()
            .map(|(i, c)| (i, agent::describe_call(c, &cwd)))
            .collect();
        let (system, user) = agent::access::build_judge_prompt(&policy, &descs);
        let n = calls.len();
        let model = if self.config.api.access_judge_model.trim().is_empty() {
            self.current_model().to_string()
        } else {
            self.config.api.access_judge_model.clone()
        };
        let endpoint = self.config.api.endpoint.clone();
        let key = self.config.api.api_key.clone();

        let (tx, rx) = mpsc::channel(1);
        self.judge_rx = Some(rx);
        self.judging = Some(JudgeBatch {
            session_id: sid,
            calls,
        });
        self.set_status(format!(
            "⚖ Judging {} call{} against access policy…",
            n,
            if n == 1 { "" } else { "s" }
        ));
        self.touch();

        tokio::spawn(async move {
            // Any failure (no client, HTTP error, parse miss) degrades to all-Ask,
            // i.e. fall back to the human — the safe direction.
            let verdicts = match ApiClient::new(&endpoint, &key) {
                Ok(client) => {
                    let mut req = ChatRequest::new(
                        &model,
                        vec![ChatMessage::system(system), ChatMessage::user(user)],
                    );
                    req.stream = false;
                    req.stream_options = None;
                    req.max_tokens = Some(256);
                    match client.complete(req).await {
                        Ok(reply) => agent::access::parse_verdicts(&reply, n),
                        Err(_) => vec![AccessVerdict::Ask; n],
                    }
                }
                Err(_) => vec![AccessVerdict::Ask; n],
            };
            let _ = tx.send((sid, verdicts)).await;
        });
        None
    }

    /// Apply the judge's per-call verdicts to the in-flight batch. Allowed calls are
    /// queued to run without re-prompting; denied calls get a policy-skip result;
    /// anything left as "ask" (including safety-floor calls, forced here regardless
    /// of the model's answer) falls back to the normal permission prompt.
    ///
    /// The batch always came from one `parallel_tool_calls` turn — the model marked
    /// these calls independent — so running the auto-allowed ones alongside a human
    /// prompt for the rest does not break an ordering dependency.
    pub fn apply_access_verdicts(
        &mut self,
        sid: usize,
        verdicts: Vec<AccessVerdict>,
    ) -> Option<Action> {
        self.judge_rx = None;
        let Some(batch) = self.judging.take() else {
            return None;
        };
        // Stale result (session switched / round cancelled) — drop it.
        if batch.session_id != sid {
            return None;
        }
        let cwd = self.agent_cwd();
        let mut ask: Vec<ToolCall> = Vec::new();
        let mut allowed = 0usize;
        let mut denied = 0usize;
        for (i, call) in batch.calls.into_iter().enumerate() {
            let mut verdict = verdicts.get(i).copied().unwrap_or(AccessVerdict::Ask);
            // Safety floor overrides the judge: destructive / irreversible ops
            // always go to the human.
            if agent::needs_hard_prompt(&call, &cwd) {
                verdict = AccessVerdict::Ask;
            }
            match verdict {
                AccessVerdict::Allow => {
                    self.approved.push_back(call);
                    allowed += 1;
                }
                AccessVerdict::Deny => {
                    self.record_tool_result(ToolResult::failure(
                        call,
                        "Skipped: denied by access policy".into(),
                        0,
                    ));
                    denied += 1;
                }
                AccessVerdict::Ask => ask.push(call),
            }
        }
        if !ask.is_empty() {
            // Human still needed for some. The auto-allowed ones wait in `approved`
            // and run once the prompt resolves.
            return self.prompt_permission(ask);
        }
        if allowed + denied > 0 {
            self.set_status(format!(
                "Access policy: {} allowed · {} denied",
                allowed, denied
            ));
        }
        self.process_next_tool()
    }

    /// Set (or clear, when blank) the session access policy. If a permission prompt
    /// is already open, re-triage that batch under the new policy right away.
    pub fn set_access_policy(&mut self, text: &str) -> Option<Action> {
        self.permissions.set_policy(text);
        match self.permissions.policy.clone() {
            Some(p) => {
                let short: String = p.chars().take(60).collect();
                self.set_status(format!("Access policy set: {}", short));
            }
            None => {
                self.set_status("Access policy cleared — tool calls prompt normally");
                return None;
            }
        }
        // A prompt is open and we just gained a policy — re-judge those calls.
        if let Overlay::Permission(req) = &self.overlay {
            let calls = req.calls.clone();
            self.overlay = Overlay::None;
            let cwd = self.agent_cwd();
            if self.can_judge() && self.batch_has_judgeable(&calls, &cwd) {
                return self.begin_judge(calls);
            }
            return self.prompt_permission(calls);
        }
        None
    }

    /// Apply the buffer edited in `$EDITOR` back onto the pending permission batch.
    /// Fields are updated in place; any call whose block the user deleted is denied
    /// (a skipped result is recorded so the model still gets an answer for it). If
    /// every call was deleted the round continues as if all were denied.
    pub fn apply_permission_edits(&mut self, text: &str) -> Option<Action> {
        let dropped = match &mut self.overlay {
            Overlay::Permission(req) => req.apply_edits(text),
            _ => return None,
        };
        if dropped.is_empty() {
            self.set_status("Commands updated — a allow · d deny · e edit again · ⏎ run");
            self.touch();
            return None;
        }
        // Pull the dropped calls out of the batch (highest index first so the
        // remaining indices stay valid) and record a skip result for each.
        let mut denied = Vec::new();
        if let Overlay::Permission(req) = &mut self.overlay {
            for &idx in dropped.iter().rev() {
                if idx < req.calls.len() {
                    denied.push(req.calls.remove(idx));
                }
            }
            req.selected = req.selected.min(PERMISSION_OPTIONS - 1);
            req.scroll = 0;
        }
        for call in denied {
            let res = ToolResult::failure(call, "Skipped by user (removed in editor)".into(), 0);
            self.record_tool_result(res);
        }
        let empty = matches!(&self.overlay, Overlay::Permission(r) if r.calls.is_empty());
        if empty {
            self.overlay = Overlay::None;
            return self.process_next_tool();
        }
        self.set_status("Commands updated — a allow · d deny · e edit again · ⏎ run");
        self.touch();
        None
    }

    fn open_decision(&mut self, call: ToolCall) -> Option<Action> {
        let question = call
            .args
            .get("question")
            .and_then(|v| v.as_str())
            .unwrap_or("Choose an option")
            .to_string();
        let options: Vec<String> = call
            .args
            .get("options")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if options.is_empty() {
            self.record_tool_result(ToolResult::failure(
                call,
                "ask: missing non-empty 'options' array".into(),
                0,
            ));
            return self.process_next_tool();
        }
        let multi = call
            .args
            .get("multi")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut chosen = BTreeSet::new();
        if multi {
            chosen.insert(0);
        }
        self.notify_desktop("AiTUI — decision needed", question.clone());
        self.overlay = Overlay::Decision(DecisionRequest {
            call,
            question,
            options,
            selected: 0,
            chosen,
            multi,
        });
        self.set_status(if multi {
            "Decision — ↑↓ choose · space toggle · ⏎ confirm · Esc cancel"
        } else {
            "Decision — ↑↓ choose · ⏎ confirm · Esc cancel"
        });
        None
    }

    pub fn resolve_decision(&mut self) -> Option<Action> {
        let req = match &self.overlay {
            Overlay::Decision(req) => req.clone(),
            _ => return None,
        };
        self.overlay = Overlay::None;
        let labels = req.labels();
        let output = if req.multi {
            serde_json::to_string(&labels).unwrap_or_else(|_| "[]".to_string())
        } else {
            labels.first().cloned().unwrap_or_default()
        };
        self.record_tool_result(ToolResult::success(req.call, output, 0));
        self.process_next_tool()
    }

    fn open_plan(&mut self, call: ToolCall) -> Option<Action> {
        let Some(raw_path) = call.args.get("path").and_then(|v| v.as_str()) else {
            self.record_tool_result(ToolResult::failure(call, "plan: missing 'path'".into(), 0));
            return self.process_next_tool();
        };
        let body = call.args.get("body").and_then(|v| v.as_str()).unwrap_or("");
        let path = PathBuf::from(raw_path);
        let path = if path.is_absolute() {
            path
        } else {
            self.agent_cwd().join(path)
        };
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                self.record_tool_result(ToolResult::failure(
                    call,
                    format!("plan: failed to create {}: {}", parent.display(), e),
                    0,
                ));
                return self.process_next_tool();
            }
        }
        if let Err(e) = std::fs::write(&path, body) {
            self.record_tool_result(ToolResult::failure(
                call,
                format!("plan: failed to write {}: {}", path.display(), e),
                0,
            ));
            return self.process_next_tool();
        }
        self.notify_desktop("AiTUI — plan approval needed", path.display().to_string());
        self.overlay = Overlay::Plan(PlanRequest {
            call,
            path: path.clone(),
        });
        self.set_status(format!(
            "Plan written: {} — e edit · a accept · d deny",
            path.display()
        ));
        None
    }

    pub fn resolve_plan(&mut self, approved: bool) -> Option<Action> {
        let req = match &self.overlay {
            Overlay::Plan(req) => req.clone(),
            _ => return None,
        };
        self.overlay = Overlay::None;
        let output = if approved {
            match std::fs::read_to_string(&req.path) {
                Ok(body) => format!("APPROVED\n{}", body),
                Err(e) => {
                    self.record_tool_result(ToolResult::failure(
                        req.call,
                        format!("plan: failed to read {}: {}", req.path.display(), e),
                        0,
                    ));
                    return self.process_next_tool();
                }
            }
        } else {
            "DENIED".to_string()
        };
        self.record_tool_result(ToolResult::success(req.call, output, 0));
        self.process_next_tool()
    }

    /// The working directory of the session whose tool round is running (falls back
    /// to the process cwd), used for permission directory-scoping and execution.
    fn agent_cwd(&self) -> PathBuf {
        let sid = self.agent_sid();
        self.sessions
            .by_id(sid)
            .and_then(|s| s.cwd.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn notify_desktop(&self, title: impl Into<String>, body: impl Into<String>) {
        if !self.focused {
            crate::app::notify::desktop(title, body);
        }
    }

    /// While an agent reply is streaming, pre-run any *complete*, side-effect-free
    /// read-only tool block it has emitted so far, in the background, so the result
    /// is already sitting in `spec_results` the moment the turn finishes and the
    /// tool round starts. Never touches tools that mutate or run commands.
    pub fn speculate_read_tools(&mut self, sid: usize) {
        // TODO(audit): add backpressure/cancellation accounting for speculative tasks;
        // stale results are dropped, but the work still consumes threads and I/O.
        let (partial, cwd) = {
            let Some(s) = self.sessions.by_id(sid) else {
                return;
            };
            if !s.agent_mode {
                return;
            }
            let Some(p) = s.streaming_display() else {
                return;
            };
            (p, s.cwd.clone())
        };
        // No runtime (unit tests) → nothing to spawn onto; skip speculation.
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let cwd = cwd
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let epoch = self.spec_epoch;
        for call in crate::agent::parser::extract_tool_calls(&partial) {
            if !is_speculatable(&call) {
                continue;
            }
            let sig = spec_sig(&call);
            // insert() returns false if already dispatched this turn → skip dup work.
            if !self.spec_dispatched.insert(sig) {
                continue;
            }
            let tx = self.spec_tx.clone();
            let cwd = cwd.clone();
            tokio::task::spawn_blocking(move || {
                let _ = tx.blocking_send((epoch, agent::execute(call, &cwd)));
            });
        }
    }

    /// Whether the streaming reply for `sid` should be cut now: it's an agent-mode
    /// session, still streaming, and its partial already contains at least one
    /// complete `​```tool​```` call. Cutting here stops the model from generating a
    /// pile of redundant calls it can't get results for until the turn ends.
    pub fn should_cut_stream(&self, sid: usize) -> bool {
        let Some(s) = self.sessions.by_id(sid) else {
            return false;
        };
        if !s.agent_mode || !s.is_streaming() {
            return false;
        }
        match s.streaming_display() {
            Some(partial) => !crate::agent::parser::extract_tool_calls(&partial).is_empty(),
            None => false,
        }
    }

    /// Stash a speculative tool result, keyed so `execute_tool` can find it when the
    /// model's committed tool call matches. Results from a stale turn (epoch no
    /// longer current) are dropped so a late arrival can't be served as fresh.
    pub fn store_spec_result(&mut self, epoch: u64, result: ToolResult) {
        if epoch == self.spec_epoch {
            self.spec_results.insert(spec_sig(&result.call), result);
        }
    }

    fn execute_tool(&mut self, call: ToolCall) -> Option<Action> {
        // If this exact call was pre-run while the reply streamed, use that result
        // instantly instead of spawning the work again.
        if let Some(result) = self.spec_results.remove(&spec_sig(&call)) {
            self.set_status(format!("⚡ {}", call.summary()));
            self.record_tool_result(result);
            return self.process_next_tool();
        }
        let summary = call.summary();
        self.set_status(format!("⚙ Running: {}", summary));
        self.active_tool = Some((summary, std::time::Instant::now()));
        self.touch();
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

    /// Apply a `todo` tool call: replace the sticky panel's task list wholesale.
    /// Items may be `{text, status}` objects (status optional) or bare strings.
    fn apply_todo(&mut self, call: &ToolCall) -> ToolResult {
        let Some(arr) = call.args.get("items").and_then(|v| v.as_array()) else {
            return ToolResult::failure(call.clone(), "todo: missing 'items' array".into(), 0);
        };
        let todos: Vec<crate::app::state::TodoItem> = arr
            .iter()
            .filter_map(|it| {
                let text = it
                    .as_str()
                    .or_else(|| {
                        it.get("text")
                            .or_else(|| it.get("content"))
                            .and_then(|v| v.as_str())
                    })?
                    .trim()
                    .to_string();
                if text.is_empty() {
                    return None;
                }
                let status = it
                    .get("status")
                    .and_then(|v| v.as_str())
                    .map(crate::app::state::TodoStatus::parse)
                    .unwrap_or(crate::app::state::TodoStatus::Pending);
                Some(crate::app::state::TodoItem { text, status })
            })
            .collect();
        let n = todos.len();
        let sid = self.agent_sid();
        if let Some(s) = self.sessions.by_id_mut(sid) {
            s.todos = todos;
        }
        // TODO(audit): debounce/coalesce session persistence; frequent tool/todo
        // updates currently rewrite the whole session file synchronously.
        self.sessions.save();
        self.touch();
        ToolResult::success(
            call.clone(),
            format!(
                "Todo panel updated ({} item{})",
                n,
                if n == 1 { "" } else { "s" }
            ),
            0,
        )
    }

    /// Select the mock model (adding it to the list if missing).
    pub fn select_mock_model(&mut self) {
        use crate::app::state::{ModelLoad, MOCK_MODEL};
        match self.models.iter().position(|m| m == MOCK_MODEL) {
            Some(i) => self.model_idx = i,
            None => {
                self.models.push(MOCK_MODEL.to_string());
                self.model_idx = self.models.len() - 1;
            }
        }
        self.model_load = ModelLoad::Loaded;
    }

    /// (Re)fetch the model list from the current endpoint: clear the list, flip to
    /// Loading (the chip shows a spinner), and spawn the fetch. The main loop drains
    /// `models_rx` into `ModelsLoaded`/`ModelsFailed`.
    pub fn refresh_models(&mut self) {
        // TODO(audit): tag model-list requests so a slower older refresh cannot
        // overwrite a newer endpoint/model state when responses arrive out of order.
        use crate::app::state::ModelLoad;
        let endpoint = self.config.api.endpoint.clone();
        let key = self.config.api.api_key.clone();
        let Ok(fetch) = crate::api::ApiClient::new(&endpoint, &key) else {
            self.model_load = ModelLoad::Failed;
            return;
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _ = tx.send(fetch.fetch_models().await);
        });
        self.models = Vec::new();
        self.model_idx = 0;
        self.model_load = ModelLoad::Loading;
        self.models_rx = Some(rx);
    }

    pub fn record_tool_result(&mut self, result: ToolResult) {
        if result.is_ok() {
            self.track_edited_file(&result.call);
        }
        self.active_tool = None;
        let icon = result.call.kind().map(|k| k.icon()).unwrap_or("⚙");
        let status = if result.is_ok() { "ok" } else { "error" };
        // Canonical tool name so the renderer can pick a purpose-built result view.
        let name = result.call.kind().map(|k| k.name()).unwrap_or("");
        let body = format!(
            "[tool-result:{}] {} {} ({})\n{}",
            name,
            icon,
            result.call.summary(),
            status,
            result.text()
        );
        let sid = self.agent_sid();
        if let Some(s) = self.sessions.by_id_mut(sid) {
            let mut msg = ChatMessage::tool(body);
            msg.duration_ms = Some(result.duration_ms);
            s.push_message(msg);
        }
        self.touch();
    }

    /// Maintain the recently-edited-files list (most recent first) from a
    /// successful mutating tool call, so the user can jump back into them.
    fn track_edited_file(&mut self, call: &ToolCall) {
        use crate::agent::ToolKind;
        let kind = call.kind();
        let mutates = matches!(
            kind,
            Some(ToolKind::Write | ToolKind::Edit | ToolKind::Delete)
        );
        if !mutates {
            return;
        }
        let Some(path) = call.args.get("path").and_then(|v| v.as_str()) else {
            return;
        };
        let path = path.trim_start_matches("./").to_string();
        self.edited_files.retain(|p| p != &path);
        if kind == Some(ToolKind::Delete) {
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

    /// The model called `finish` — end this session's autonomous loop. Clears the
    /// loop so the next completed turn won't continue it, and reports the summary.
    fn apply_finish(&mut self, call: &ToolCall) -> ToolResult {
        let summary = call
            .args
            .get("summary")
            .or_else(|| call.args.get("reason"))
            .and_then(|v| v.as_str())
            .unwrap_or("done")
            .to_string();
        let sid = self.agent_sid();
        let was_looping = self
            .sessions
            .by_id_mut(sid)
            .map(|s| s.loop_state.take().is_some())
            .unwrap_or(false);
        if was_looping {
            self.sessions.save();
            self.notify_desktop("AiTUI — loop finished", summary.clone());
            self.set_status(format!("🏁 Loop finished — {}", summary));
            ToolResult::success(call.clone(), format!("Loop ended: {}", summary), 0)
        } else {
            // `finish` outside a loop is a no-op the model shouldn't have called.
            ToolResult::success(
                call.clone(),
                "finish ignored — not in loop mode".into(),
                0,
            )
        }
    }

    /// Start an autonomous loop on the active session: store the goal/criteria, turn
    /// on agent mode, seed the goal as the first user turn, and begin streaming. The
    /// agent then keeps working on its own (see `maybe_continue_loop`).
    pub fn start_loop(&mut self, goal: String, stop: String, max: usize) -> Option<Action> {
        let goal = goal.trim().to_string();
        if goal.is_empty() {
            self.set_status("Loop needs a goal — :loop <what to do>");
            return None;
        }
        let stop = if stop.trim().is_empty() {
            "The goal is fully and verifiably complete.".to_string()
        } else {
            stop.trim().to_string()
        };
        let max = max.max(1);
        let sid = self.sessions.active_id();
        if let Some(s) = self.sessions.by_id_mut(sid) {
            s.agent_mode = true;
            s.loop_state = Some(crate::domain::session::LoopState {
                goal: goal.clone(),
                stop: stop.clone(),
                iteration: 0,
                max,
            });
            s.push_message(ChatMessage::user(format!(
                "Begin working on this task autonomously.\n\nGOAL: {}\n\nSTOP CRITERIA: {}",
                goal, stop
            )));
        }
        self.sessions.save();
        self.set_status(format!("⟳ Loop started (max {} iterations) — Ctrl-C or :loop stop to halt", max));
        self.touch();
        self.begin_stream_for(sid)
    }

    /// Called when a turn finished with no tool calls (the model produced a plain
    /// reply). If the session is looping and not yet done, bump the counter and
    /// either stop (hit the cap) or nudge the model into another iteration.
    fn maybe_continue_loop(&mut self, sid: usize) -> Option<Action> {
        let (goal, stop, iteration, max) = match self.sessions.by_id(sid).and_then(|s| s.loop_state.as_ref()) {
            Some(l) => (l.goal.clone(), l.stop.clone(), l.iteration, l.max),
            None => return None,
        };
        let next = iteration + 1;
        if next >= max {
            if let Some(s) = self.sessions.by_id_mut(sid) {
                s.loop_state = None;
            }
            self.sessions.save();
            self.notify_desktop("AiTUI — loop stopped", format!("Reached the {}-iteration cap", max));
            self.set_status(format!("⟳ Loop stopped after {} iterations (cap). :loop to resume.", max));
            self.touch();
            return None;
        }
        if let Some(s) = self.sessions.by_id_mut(sid) {
            if let Some(l) = s.loop_state.as_mut() {
                l.iteration = next;
            }
            s.push_message(ChatMessage::user(format!(
                "Continue toward the goal (iteration {}/{}). GOAL: {}\nSTOP CRITERIA: {}\n\
                 If the stop criteria are now fully met, call the `finish` tool with a short \
                 summary. Otherwise make concrete progress this turn using tools.",
                next, max, goal, stop
            )));
        }
        self.sessions.save();
        self.set_status(format!("⟳ Loop iteration {}/{}", next, max));
        self.touch();
        self.begin_stream_for(sid)
    }

    /// Stop an active loop on the active session (from `:loop stop` / Ctrl-C).
    pub fn stop_loop(&mut self) {
        let sid = self.sessions.active_id();
        let stopped = self
            .sessions
            .by_id_mut(sid)
            .map(|s| s.loop_state.take().is_some())
            .unwrap_or(false);
        if stopped {
            self.sessions.save();
            self.set_status("⟳ Loop stopped.");
            self.touch();
        }
    }
}

fn active_skills_prompt(skills: &[crate::skills::Skill]) -> Option<String> {
    let active: Vec<&crate::skills::Skill> = skills.iter().filter(|s| s.active).collect();
    if active.is_empty() {
        return None;
    }
    let mut out = String::from(
        "Active skills are mandatory response-shaping instructions. Apply every active skill to every answer in this turn, including after tool calls. If a skill changes tone, format, constraints, or workflow, the final response and intermediate user-visible updates must reflect it.\n",
    );
    for skill in active {
        out.push_str(&format!(
            "\n## Skill: {}\n{}\n",
            skill.name,
            skill.body.trim()
        ));
    }
    Some(out)
}

fn prepend_or_merge_system(messages: &mut Vec<ChatMessage>, text: String) {
    if text.trim().is_empty() {
        return;
    }
    match messages.first_mut() {
        Some(first) if first.role == "system" => {
            if let MessageContent::Text(existing) = &mut first.content {
                let old = std::mem::take(existing);
                *existing = format!("{}\n\n{}", text.trim(), old.trim());
                return;
            }
        }
        _ => {}
    }
    messages.insert(0, ChatMessage::system(text.trim().to_string()));
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

/// Write a large paste to `./aitui-pastes/paste-<ts>.txt` and return its path, so
/// it can be attached instead of flooding the composer.
fn write_paste_file(text: &str) -> anyhow::Result<PathBuf> {
    let dir = PathBuf::from("aitui-pastes");
    std::fs::create_dir_all(&dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("paste-{}.txt", stamp));
    std::fs::write(&path, text)?;
    Ok(path)
}

fn agent_loop_guard_reached(iterations: usize) -> bool {
    iterations == MAX_AGENT_ITERATIONS
}

/// Whether a tool call is safe to pre-run speculatively: local, read-only, no
/// side effects. Deliberately excludes network reads (web fetch/search) and
/// anything that mutates state or runs commands.
fn is_speculatable(call: &ToolCall) -> bool {
    matches!(
        call.kind(),
        Some(ToolKind::Read | ToolKind::List | ToolKind::Search)
    )
}

/// Signature of a tool call by name + arguments, so a speculatively-run result can
/// be matched to the model's committed call regardless of any `id` difference.
fn spec_sig(call: &ToolCall) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    call.name.hash(&mut h);
    call.args.to_string().hash(&mut h);
    h.finish()
}

/// Content signature for one message: role + text + the block indices the user
/// has toggled for this message. Width and show-output are folded into the cache
/// key globally (see `DocCache::reset_if_env_changed`), so they're not hashed here.
fn message_sig(
    role: &str,
    text: &str,
    duration_ms: Option<u64>,
    toggled: &std::collections::HashSet<(usize, usize)>,
    mi: usize,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    role.hash(&mut h);
    text.hash(&mut h);
    duration_ms.hash(&mut h);
    // Only this message's toggles matter; hash them in a stable order.
    let mut bis: Vec<usize> = toggled
        .iter()
        .filter(|(m, _)| *m == mi)
        .map(|(_, b)| *b)
        .collect();
    bis.sort_unstable();
    bis.hash(&mut h);
    h.finish()
}

/// The empty-state splash, shown when there are no messages.
fn welcome_doc(theme: &Theme, width: usize) -> Vec<RenderedLine> {
    let logo = [
        "        ████████████████        ",
        "     ███▓▓▓▓▓▓▓▓▓▓▓▓▓▓███     ",
        "   ██▓▓      ▓▓▓▓      ▓▓██   ",
        "  ██▓▓   ██   ▓▓   ██   ▓▓██  ",
        " ██▓▓   ████  ▓▓  ████   ▓▓██ ",
        " ██▓▓        ▓▓▓▓        ▓▓██ ",
        " ██▓▓  ▓▓▓▓  ▓▓▓▓  ▓▓▓▓  ▓▓██ ",
        "  ██▓▓  ▓▓▓▓▓▓▓▓▓▓▓▓▓▓  ▓▓██  ",
        "   ██▓▓     ▓▓▓▓▓▓     ▓▓██   ",
        "     ███▓▓▓▓▓▓▓▓▓▓▓▓███     ",
        "        ████████████████        ",
    ];
    let cyan = Color::Rgb(11, 227, 253);
    let purple = Color::Rgb(29, 6, 72);
    let logo_w = logo.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let pad = width.saturating_sub(logo_w) / 2;
    let mut rows = Vec::new();

    for raw in logo {
        let mut spans = Vec::new();
        spans.push(Span::raw(" ".repeat(pad)));
        let mut run = String::new();
        let mut run_style = None;
        for ch in raw.chars() {
            let style = match ch {
                '█' => Some(Style::default().fg(purple).add_modifier(Modifier::BOLD)),
                '▓' => Some(Style::default().fg(cyan).add_modifier(Modifier::BOLD)),
                _ => None,
            };
            if style != run_style {
                if !run.is_empty() {
                    match run_style {
                        Some(st) => spans.push(Span::styled(std::mem::take(&mut run), st)),
                        None => spans.push(Span::raw(std::mem::take(&mut run))),
                    }
                }
                run_style = style;
            }
            run.push(ch);
        }
        if !run.is_empty() {
            match run_style {
                Some(st) => spans.push(Span::styled(run.clone(), st)),
                None => spans.push(Span::raw(run.clone())),
            }
        }
        rows.push(RenderedLine::new(
            Line::from(spans),
            format!("{}{}", " ".repeat(pad), raw),
            0,
        ));
    }

    rows.push(RenderedLine::new(Line::raw(""), String::new(), 0));
    let title = "AiTUI";
    let subtitle = "terminal-native coding agent";
    for (text, style) in [
        (
            title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        (subtitle, Style::default().fg(theme.muted)),
    ] {
        let pad = width.saturating_sub(text.chars().count()) / 2;
        rows.push(RenderedLine::new(
            Line::from(vec![
                Span::raw(" ".repeat(pad)),
                Span::styled(text.to_string(), style),
            ]),
            format!("{}{}", " ".repeat(pad), text),
            0,
        ));
    }
    rows.push(RenderedLine::new(Line::raw(""), String::new(), 0));

    let tips = [
        ("@path", "pull a file into context"),
        ("/", "open commands"),
        ("i … :w", "compose, then send"),
        ("Ctrl-A", "toggle agent mode"),
        ("?", "show every keybinding"),
    ];
    let tip_w = tips
        .iter()
        .map(|(k, v)| k.chars().count() + v.chars().count() + 5)
        .max()
        .unwrap_or(0);
    let pad = width.saturating_sub(tip_w) / 2;
    for (key, desc) in tips {
        let plain = format!("{}  {:<7} — {}", " ".repeat(pad), key, desc);
        rows.push(RenderedLine::new(
            Line::from(vec![
                Span::raw(" ".repeat(pad)),
                Span::styled(
                    format!("  {:<7}", key),
                    Style::default()
                        .fg(theme.warning)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" — ", Style::default().fg(theme.accent)),
                Span::styled(desc.to_string(), Style::default().fg(theme.text)),
            ]),
            plain,
            0,
        ));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_skills_prompt_reinforces_and_includes_only_active_skills() {
        let skills = vec![
            crate::skills::Skill {
                name: "terse".into(),
                desc: "".into(),
                body: "Answer briefly.".into(),
                active: true,
            },
            crate::skills::Skill {
                name: "off".into(),
                desc: "".into(),
                body: "Never include me.".into(),
                active: false,
            },
        ];
        let prompt = active_skills_prompt(&skills).expect("active prompt");
        assert!(prompt.contains("mandatory response-shaping instructions"));
        assert!(prompt.contains("## Skill: terse"));
        assert!(prompt.contains("Answer briefly."));
        assert!(!prompt.contains("Never include me."));
    }

    #[test]
    fn prepend_or_merge_system_merges_with_existing_first_system_message() {
        let mut messages = vec![
            ChatMessage::system("agent prompt"),
            ChatMessage::user("hello"),
        ];
        prepend_or_merge_system(&mut messages, "skill prompt".into());
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        let MessageContent::Text(text) = &messages[0].content else {
            panic!("expected text system message");
        };
        assert!(text.starts_with("skill prompt"));
        assert!(text.contains("agent prompt"));
    }

    #[test]
    fn loop_guard_trips_at_max_before_incrementing() {
        assert!(!agent_loop_guard_reached(0));
        assert!(!agent_loop_guard_reached(MAX_AGENT_ITERATIONS - 1));
        assert!(agent_loop_guard_reached(MAX_AGENT_ITERATIONS));
    }
}
