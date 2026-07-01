//! Side effects: composing the chat document, starting model requests, and the
//! agent tool-execution loop. These methods may return a follow-up `Action`
//! (e.g. attach a freshly spawned stream) for the reducer to process.

use std::path::PathBuf;
use tokio::sync::mpsc;

use crate::agent::{self, Permission, ToolCall, ToolKind, ToolResult};
use crate::api::models::MessageContent;
use crate::api::{ChatMessage, ChatRequest};
use crate::app::action::Action;
use crate::app::overlay::{Overlay, PermissionRequest};
use crate::app::state::{expand_mentions, App, MAX_AGENT_ITERATIONS};
use crate::domain::blocks::{parse_blocks, parse_tool_result};
use crate::render::document::{build, build_message, DocMessage, RenderedLine};
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

        cache.reset_if_env_changed(width, show_output);
        cache.truncate(session.messages.len());

        let mut out: Vec<RenderedLine> = Vec::new();
        for (mi, m) in session.messages.iter().enumerate() {
            let text = message_text(m);
            let sig = message_sig(&m.role, &text, toggled, mi);
            if let Some(rows) = cache.get(mi, sig) {
                out.extend_from_slice(rows);
            } else {
                let blocks = if m.role == "tool" {
                    vec![parse_tool_result(&text)]
                } else {
                    parse_blocks(&text)
                };
                let doc_msg = DocMessage { role: m.role.clone(), blocks };
                // Finalized messages don't animate — pass streaming=false so a
                // finished thinking block isn't spinning (and stays cacheable).
                let rows = build_message(&doc_msg, mi, width, &theme, toggled, show_output, false);
                out.extend_from_slice(&rows);
                cache.put(mi, sig, rows);
            }
        }

        // The live streaming partial: rebuilt every frame (its text changes each
        // token), appended after the cached history, with the spinner animating.
        if let Some(partial) = session.streaming_display() {
            let mi = session.messages.len();
            let doc_msg = DocMessage { role: "assistant".into(), blocks: parse_blocks(&partial) };
            out.extend(build_message(&doc_msg, mi, width, &theme, toggled, show_output, true));
        }

        if out.is_empty() {
            return welcome_doc(&theme);
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

        // In the `:` command line there are no chips — insert the (newline-stripped) text.
        if self.vim == crate::input::vim::VimMode::Command {
            for c in text.chars().filter(|c| *c != '\n') {
                self.command.push(c);
            }
            return;
        }

        let lines = text.lines().count().max(1);
        let chars = text.chars().count();

        if chars >= FILE_CHARS {
            match write_paste_file(&text) {
                Ok(path) => {
                    self.attachment = Some(path);
                    self.set_status(format!("🖇 Large paste attached as file ({} lines, {} chars)", lines, chars));
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
            self.set_status(format!("Pasted {} lines, {} chars — expands on send", lines, chars));
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
        // Fresh turn: bump the epoch (so any speculative result still in flight from
        // the previous turn is dropped, not served stale) and drop its state.
        self.spec_epoch = self.spec_epoch.wrapping_add(1);
        self.spec_dispatched.clear();
        self.spec_results.clear();
        let Some(session) = self.sessions.by_id_mut(sid) else { return None };
        session.begin_assistant_stream();
        // The animated status-bar spinner ("working") is the generating indicator
        // now — don't set a free-text "Generating…" that later messages clobber.
        self.status = None;
        if sid == self.sessions.active_id() {
            self.chat.stick_bottom = true;
        }
        self.touch();

        // Image-generation models use a different endpoint (chat completions 503s
        // them). Route to /v1/images/generations with the last user message as the
        // prompt; the result comes back over the same stream channel.
        let model = self.current_model().to_string();
        if crate::api::is_image_model(&model) && !self.mock {
            let prompt = self
                .sessions
                .by_id(sid)
                .and_then(|s| s.messages.iter().rev().find(|m| m.role == "user"))
                .map(message_text)
                .unwrap_or_default();
            if prompt.trim().is_empty() {
                if let Some(s) = self.sessions.by_id_mut(sid) { s.finalize_assistant_stream(); }
                self.set_status("Nothing to generate — describe the image first.");
                return None;
            }
            self.set_status("🖼 Generating image…");
            return match self.api.as_ref() {
                Some(client) => match client.generate_image(&model, &prompt) {
                    Ok(rx) => Some(Action::AttachStream(sid, rx)),
                    Err(e) => {
                        if let Some(s) = self.sessions.by_id_mut(sid) { s.finalize_assistant_stream(); }
                        self.set_status(format!("Image request failed: {}", e));
                        None
                    }
                },
                None => {
                    if let Some(s) = self.sessions.by_id_mut(sid) { s.finalize_assistant_stream(); }
                    self.set_status("No API client");
                    None
                }
            };
        }

        // Prepend active skills as system messages (personas / house styles).
        let native = self.config.api.native_tools;
        let mut messages = self.sessions.by_id(sid).map(|s| s.api_messages(native)).unwrap_or_default();
        for skill in self.skills.iter().rev().filter(|s| s.active) {
            messages.insert(0, ChatMessage::system(skill.body.clone()));
        }
        // The global system prompt from config.toml sits at the very front.
        let sys = self.config.api.system_prompt.trim();
        if !sys.is_empty() {
            messages.insert(0, ChatMessage::system(sys.to_string()));
        }
        let mut request = ChatRequest::new(self.current_model(), messages)
            .with_reasoning_effort(self.reasoning_effort.clone());
        // Native function-calling: send the tool schemas so the model returns
        // structured tool_calls instead of ```tool fences (agent turns only).
        if native {
            let agent = self.sessions.by_id(sid).map(|s| s.agent_mode).unwrap_or(false);
            if agent {
                request = request.with_tools(crate::agent::tool_schemas());
            }
        }

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

    /// While an agent reply is streaming, pre-run any *complete*, side-effect-free
    /// read-only tool block it has emitted so far, in the background, so the result
    /// is already sitting in `spec_results` the moment the turn finishes and the
    /// tool round starts. Never touches tools that mutate or run commands.
    pub fn speculate_read_tools(&mut self, sid: usize) {
        let (partial, cwd) = {
            let Some(s) = self.sessions.by_id(sid) else { return };
            if !s.agent_mode { return; }
            let Some(p) = s.streaming_display() else { return };
            (p, s.cwd.clone())
        };
        // No runtime (unit tests) → nothing to spawn onto; skip speculation.
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let cwd = cwd.or_else(|| std::env::current_dir().ok()).unwrap_or_else(|| PathBuf::from("."));
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
        let Some(s) = self.sessions.by_id(sid) else { return false };
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

/// Whether a tool call is safe to pre-run speculatively: local, read-only, no
/// side effects. Deliberately excludes network reads (web fetch/search) and
/// anything that mutates state or runs commands.
fn is_speculatable(call: &ToolCall) -> bool {
    matches!(
        call.kind(),
        Some(ToolKind::ReadFile | ToolKind::ListDir | ToolKind::SearchFiles)
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
fn message_sig(role: &str, text: &str, toggled: &std::collections::HashSet<(usize, usize)>, mi: usize) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    role.hash(&mut h);
    text.hash(&mut h);
    // Only this message's toggles matter; hash them in a stable order.
    let mut bis: Vec<usize> = toggled.iter().filter(|(m, _)| *m == mi).map(|(_, b)| *b).collect();
    bis.sort_unstable();
    bis.hash(&mut h);
    h.finish()
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
