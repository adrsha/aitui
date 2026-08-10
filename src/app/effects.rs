//! Side effects: composing the chat document, starting model requests, and the
//! agent tool-execution loop. These methods may return a follow-up `Action`
//! (e.g. attach a freshly spawned stream) for the reducer to process.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

use crate::agent::{
    self, AccessVerdict, Permission, PermissionDecision, PermissionScope, ToolCall, ToolKind,
    ToolResult,
};
use crate::api::models::MessageContent;
use crate::api::{ApiClient, ChatMessage, ChatRequest};
use crate::app::action::Action;
use crate::app::overlay::{
    DecisionRequest, Overlay, PermissionRequest, PlanRequest, PERMISSION_OPTIONS,
};
use crate::app::state::JudgeBatch;
use crate::app::state::{expand_mentions, App, MAX_AGENT_ITERATIONS};
use crate::domain::blocks::{parse_blocks, parse_tool_result};
use crate::domain::session::MAX_COLD_STREAM_RETRIES;
use crate::render::document::{build_message, DocMessage, RenderedLine};
use crate::render::theme::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// Plain display text for a stored message.
fn message_text(m: &ChatMessage) -> String {
    match &m.content {
        MessageContent::Text(t) => t.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .map(|p| match p {
                crate::api::models::ContentPart::Text { text } => text.clone(),
                crate::api::models::ContentPart::ImageUrl { .. } => "[image attached]".to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn subtask_message_body(task: &crate::app::state::Subtask) -> String {
    let status = match task.status {
        crate::app::state::SubtaskStatus::Running | crate::app::state::SubtaskStatus::Completed => {
            "ok"
        }
        crate::app::state::SubtaskStatus::Failed => "error",
    };
    let elapsed_ms = task
        .duration_ms
        .unwrap_or_else(|| task.started_at.elapsed().as_millis() as u64);
    let activity = task.activity.as_deref().unwrap_or(&task.description);
    let metadata = serde_json::json!({
        "status": match task.status {
            crate::app::state::SubtaskStatus::Running => "running",
            crate::app::state::SubtaskStatus::Completed => "completed",
            crate::app::state::SubtaskStatus::Failed => "failed",
        },
        "description": task.description,
        "activity": activity,
        "todo_index": task.todo_index,
        "cwd": task.cwd,
        "elapsed_ms": elapsed_ms,
    });
    let events = task
        .log
        .iter()
        .filter_map(|entry| serde_json::to_string(entry).ok())
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "[tool-result:agent] {} ({})\n[agent-id:{}]\n[agent-meta] {}\n[agent-events]\n{}\n[agent-report]\n{}",
        task.call.summary(),
        status,
        task.id,
        metadata,
        events,
        task.output.as_deref().unwrap_or("")
    )
}

fn build_image_prompt(session: &crate::domain::session::Session) -> String {
    const MAX_CHARS: usize = 20_000;
    let mut turns = Vec::new();
    let mut used = 0usize;
    for message in session.messages.iter().rev() {
        if message.mock || !matches!(message.role.as_str(), "user" | "assistant") {
            continue;
        }
        let text = message_text(message);
        if text.trim().is_empty()
            || (message.role == "assistant" && text.starts_with("Image saved"))
        {
            continue;
        }
        let available = MAX_CHARS.saturating_sub(used);
        if available == 0 {
            break;
        }
        let text: String = text.chars().take(available).collect();
        used += text.chars().count();
        turns.push(format!("{}: {}", capitalize_role(&message.role), text));
    }
    turns.reverse();
    if turns.is_empty() {
        return String::new();
    }
    format!(
        "Create an image for the latest User request. Use the earlier conversation as factual and visual context; do not draw the conversation itself unless requested.\n\n{}",
        turns.join("\n\n")
    )
}

fn capitalize_role(role: &str) -> &str {
    if role == "assistant" {
        "Assistant"
    } else {
        "User"
    }
}

const TOOL_PREP_FRAMES: [&str; 4] = ["⠁⠂⠄", "⠂⠄⡀", "⠄⡀⢀", "⡀⢀⠁"];

fn native_tool_prep_row(
    name: &str,
    started_at: std::time::Instant,
    mi: usize,
    theme: &Theme,
) -> RenderedLine {
    let elapsed = started_at.elapsed().as_millis();
    let frame = TOOL_PREP_FRAMES[((elapsed / 160) as usize) % TOOL_PREP_FRAMES.len()];
    let label = if name.trim().is_empty() {
        format!("  {} Preparing tool call…", frame)
    } else {
        format!("  {} Preparing {}…", frame, name)
    };
    RenderedLine::new(
        Line::from(Span::styled(
            label.clone(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        label,
        mi,
    )
}

fn fallback_session_title(prompt: &str) -> String {
    prompt
        .split_whitespace()
        .take(6)
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(['"', '\'', '.', ':'])
        .chars()
        .take(48)
        .collect::<String>()
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
        let active = self.sessions.active_id();
        let animated = self.sessions.active().is_streaming()
            || self.streams.iter().any(|s| s.session_id == active)
            || self
                .preparing_tool
                .as_ref()
                .is_some_and(|(sid, _, _)| *sid == active)
            || self
                .task_barrier
                .as_ref()
                .is_some_and(|barrier| barrier.session_id == active)
            || (self.agent_session == Some(active) && self.active_tool.is_some());
        if animated
            || self.chat.viewed_node != self.view_node
            || self.chat.needs_rebuild(self.content_rev, width)
        {
            let doc = self.build_chat_doc(width);
            self.chat.viewed_node = self.view_node;
            self.chat.set_doc(doc, self.content_rev, width, viewport_h);
        }
    }

    /// Assemble the chat document, reusing per-message cached rows for every
    /// message whose content signature is unchanged. Only cache misses (in
    /// practice: the streaming message, plus anything just appended or toggled)
    /// pay the parse + highlight + wrap cost. The in-progress streaming partial is
    /// always rebuilt fresh and never cached.
    fn build_chat_doc(&mut self, width: usize) -> Vec<RenderedLine> {
        if let Some(node_id) = self.view_node {
            if self.subtasks.iter().any(|task| task.id == node_id) {
                return self.build_node_doc(node_id, width);
            }
        }
        let theme = self.theme();
        let show_output = self.show_output;

        let active_session_id = self.sessions.active_id();
        let live_subtask_messages: std::collections::HashMap<usize, String> = self
            .subtasks
            .iter()
            .filter(|task| {
                task.session_id == active_session_id
                    && task.status == crate::app::state::SubtaskStatus::Running
                    && task.message_index != usize::MAX
            })
            .map(|task| (task.message_index, subtask_message_body(task)))
            .collect();

        // Disjoint field borrows: cache (mut), toggled + sessions (shared).
        let cache = &mut self.doc_cache;
        let toggled = &self.chat.toggled;
        let session = self.sessions.active();
        let active_tool = self.active_tool.clone();
        let active_tool_for_session = self.agent_session == Some(session.id);

        cache.reset_if_env_changed(width, show_output);
        cache.truncate(session.messages.len());

        let mut out: Vec<RenderedLine> = Vec::new();
        let mut prev_role: Option<&str> = None;
        for (mi, m) in session.messages.iter().enumerate() {
            let text = live_subtask_messages
                .get(&mi)
                .cloned()
                .unwrap_or_else(|| message_text(m));
            let live_subtask = live_subtask_messages.contains_key(&mi);
            let sig = message_sig(&m.role, &text, m.duration_ms, toggled, mi);
            let skip_sep = prev_role == Some("tool") && m.role != "tool";
            if !live_subtask {
                if let Some(rows) = cache.get(mi, sig) {
                    if skip_sep {
                        out.extend(rows.iter().filter(|r| r.role_start.is_none()).cloned());
                    } else {
                        out.extend_from_slice(rows);
                    }
                    prev_role = Some(m.role.as_str());
                    continue;
                }
            }
            {
                let blocks = if m.role == "tool" {
                    let block = parse_tool_result(&text);
                    let block = match (block, m.local_tool_call.clone()) {
                        (
                            crate::domain::blocks::Block::ToolResult {
                                ok,
                                name,
                                summary,
                                output,
                            },
                            Some(call),
                        ) => crate::domain::blocks::Block::ToolFileResult {
                            ok,
                            name,
                            summary,
                            output,
                            call,
                        },
                        (block, _) => block,
                    };
                    vec![block]
                } else {
                    parse_blocks(&text)
                };
                let doc_msg = DocMessage {
                    role: m.role.clone(),
                    blocks,
                    duration_ms: m.duration_ms,
                    created_at: Some(m.created_at),
                };
                // Finalized messages don't animate — pass streaming=false so a
                // finished thinking block isn't spinning (and stays cacheable).
                let mut rows =
                    build_message(&doc_msg, mi, width, &theme, toggled, show_output, false);
                if skip_sep {
                    rows.retain(|r| r.role_start.is_none());
                }
                out.extend_from_slice(&rows);
                cache.put(mi, sig, rows);
            }
            prev_role = Some(m.role.as_str());
        }

        if active_tool_for_session {
            if let Some((summary, _)) = active_tool {
                let mi = session.messages.len();
                let doc_msg = DocMessage {
                    role: "tool".into(),
                    blocks: vec![crate::domain::blocks::Block::Markdown(format!(
                        "About to run `{}` — executing the requested tool now.",
                        summary
                    ))],
                    duration_ms: None,
                    created_at: None,
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
            let doc_msg = DocMessage {
                role: "assistant".into(),
                blocks: parse_blocks(&partial),
                duration_ms: None,
                created_at: None,
            };
            let skip_partial_sep = prev_role == Some("tool") && doc_msg.role != "tool";
            let mut rows = build_message(&doc_msg, mi, width, &theme, toggled, show_output, true);
            if skip_partial_sep {
                rows.retain(|r| r.role_start.is_none());
            }
            if let Some((_, name, started_at)) = self
                .preparing_tool
                .as_ref()
                .filter(|(sid, _, _)| *sid == session.id)
            {
                let row = native_tool_prep_row(name, *started_at, mi, &theme);
                let insert_at = rows.len().saturating_sub(1);
                rows.insert(insert_at, row);
            }
            out.extend(rows);
        }

        if out.is_empty() {
            return welcome_doc(&theme, width);
        }
        out
    }

    /// Chat document for an entered child agent: a breadcrumb row (click to go
    /// back to the root) followed by the agent's captured conversation — its
    /// assistant replies, tool calls, and tool results in order.
    fn build_node_doc(&mut self, node_id: u64, width: usize) -> Vec<RenderedLine> {
        use crate::app::state::SubtaskRoundRole;
        use crate::domain::blocks::Block;
        use ratatui::style::{Modifier, Style};
        use ratatui::text::{Line, Span};

        let theme = self.theme();
        let show_output = self.show_output;
        let toggled = &self.chat.toggled;
        let task = self.subtasks.iter().find(|task| task.id == node_id);

        let mut out: Vec<RenderedLine> = Vec::new();
        let name = task
            .map(|task| crate::ui::sidepanel::agent_display_name(task))
            .unwrap_or_else(|| "agent".into());
        let crumb = RenderedLine::new(
            Line::from(vec![
                Span::styled(
                    " ‹ BACK TO ROOT ",
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("· {} ", name),
                    Style::default()
                        .fg(theme.warning)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            format!("‹ BACK TO ROOT · {}", name),
            usize::MAX,
        );
        let mut crumb = crumb;
        crumb.role_start = Some("tool");
        out.push(crumb);

        let mut prev_role: Option<&str> = None;
        if let Some(task) = task {
            for round in &task.transcript {
                let (role, blocks) = match round.role {
                    SubtaskRoundRole::Assistant => ("assistant", parse_blocks(&round.content)),
                    SubtaskRoundRole::ToolCall => (
                        "tool",
                        vec![Block::Markdown(format!("◧ {}", round.content))],
                    ),
                    SubtaskRoundRole::ToolResult => (
                        "tool",
                        vec![Block::ToolResult {
                            ok: true,
                            name: Some("tool".into()),
                            summary: "result".into(),
                            output: round.content.clone(),
                        }],
                    ),
                };
                let doc_msg = DocMessage {
                    role: role.to_string(),
                    blocks,
                    duration_ms: None,
                    created_at: None,
                };
                let mi = out.len();
                let mut rows =
                    build_message(&doc_msg, mi, width, &theme, toggled, show_output, false);
                if prev_role == Some("tool") && role != "tool" {
                    rows.retain(|r| r.role_start.is_none());
                }
                prev_role = Some(role);
                out.extend(rows);
            }
        }

        if out.len() == 1 {
            let activity = task
                .and_then(|task| task.activity.as_deref())
                .filter(|text| !text.trim().is_empty())
                .unwrap_or("Starting child agent…");
            let text = format!("◐ {activity}");
            out.push(RenderedLine::new(
                Line::from(Span::styled(
                    text.clone(),
                    Style::default()
                        .fg(theme.warning)
                        .add_modifier(Modifier::BOLD),
                )),
                text,
                usize::MAX,
            ));
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
                        "Large paste attached as file ({} lines, {} chars)",
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
        self.show_last_prompt = false;
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

        let mention_root = self
            .sessions
            .active()
            .cwd
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let mention_ctx = expand_mentions(&text, &mention_root);
        let text = if mention_ctx.is_empty() {
            text
        } else {
            format!("{}\n\n{}", mention_ctx, text)
        };

        let msg = build_user_message(&text, attachment.as_ref(), self);
        self.sessions.active_mut().push_message(msg);
        self.auto_name_session();
        self.sessions.save();
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
        self.show_last_prompt = false;
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
        let should_generate = {
            let s = self.sessions.active();
            s.name.starts_with("Session ") && s.messages.len() == 1
        };
        if !should_generate || self.title_rx.is_some() {
            return;
        }

        let sid = self.sessions.active_id();
        let Some(prompt) = self.sessions.active().first_message_preview(800) else {
            return;
        };
        if prompt.trim().is_empty() {
            return;
        }
        self.sessions.active_mut().name = "Naming…".into();

        let model = self.current_model().to_string();
        let Some(api) = self.api.clone() else {
            self.sessions.active_mut().name = fallback_session_title(&prompt);
            return;
        };
        let (tx, rx) = mpsc::channel(1);
        self.title_rx = Some(rx);
        tokio::spawn(async move {
            let mut req = ChatRequest::new(
                &model,
                vec![
                    ChatMessage::system(
                        "Generate a concise chat title. Return only the title, no quotes, no punctuation at the end, max 6 words.",
                    ),
                    ChatMessage::user(prompt.clone()),
                ],
            );
            req.stream = false;
            req.stream_options = None;
            req.max_tokens = Some(24);
            let title = match api.complete(req).await {
                Ok(t) => t,
                Err(_) => fallback_session_title(&prompt),
            };
            let _ = tx.send((sid, title)).await;
        });
    }

    pub(super) fn maybe_request_session_memory(&mut self, sid: usize) {
        if self.is_mock()
            || self.config.api.endpoint.trim().is_empty()
            || crate::api::is_image_model(self.current_model())
        {
            return;
        }
        if self.memory_inflight.contains(&sid) {
            self.memory_pending.insert(sid);
            return;
        }
        let Some(session) = self.sessions.by_id(sid) else {
            return;
        };
        let Some((user_message, assistant_response)) = session.latest_completed_turn() else {
            return;
        };
        let memories = session.memories.clone();
        let source_turn = session.memory_source_turn.saturating_add(1);
        let Some(api) = self.api.clone() else {
            eprintln!("Session memory skipped for session {}: no API client", sid);
            return;
        };
        let model = self.current_model().to_string();
        let (system, user) =
            crate::app::memory::build_prompt(&user_message, &assistant_response, &memories);
        if let Some(session) = self.sessions.by_id_mut(sid) {
            session.memory_source_turn = source_turn;
        }
        self.memory_inflight.insert(sid);
        let tx = self.memory_tx.clone();
        tokio::spawn(async move {
            let mut request = ChatRequest::new(
                &model,
                vec![ChatMessage::system(system), ChatMessage::user(user)],
            );
            request.stream = false;
            request.stream_options = None;
            request.max_tokens = Some(1_024);
            let result = match api.complete(request).await {
                Ok(reply) => crate::app::memory::parse_extraction(&reply),
                Err(error) => Err(format!("memory extraction request failed: {}", error)),
            };
            if tx.send((sid, source_turn, result)).await.is_err() {
                eprintln!(
                    "Session memory result dropped for session {}: application channel closed",
                    sid
                );
            }
        });
    }

    pub(super) fn apply_session_memory_result(
        &mut self,
        sid: usize,
        source_turn: u64,
        result: Result<Vec<crate::app::memory::MemoryOperation>, String>,
    ) {
        self.memory_inflight.remove(&sid);
        match result {
            Ok(operations) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|duration| duration.as_secs());
                match (self.sessions.by_id_mut(sid), now) {
                    (Some(session), Some(now)) if session.memory_source_turn == source_turn => {
                        crate::app::memory::apply_operations(
                            &mut session.memories,
                            &mut session.next_memory_id,
                            source_turn,
                            now,
                            operations,
                        );
                        self.sessions.save();
                    }
                    (Some(_), Some(_)) => eprintln!(
                        "Session memory result skipped for session {}: stale source turn {}",
                        sid, source_turn
                    ),
                    (Some(_), None) => eprintln!(
                        "Session memory result skipped for session {}: system clock unavailable",
                        sid
                    ),
                    (None, _) => eprintln!(
                        "Session memory result skipped: session {} no longer exists",
                        sid
                    ),
                }
            }
            Err(error) => eprintln!("Session memory skipped for session {}: {}", sid, error),
        }
        if self.memory_pending.remove(&sid) {
            self.maybe_request_session_memory(sid);
        }
    }

    /// Start an optional, low-cost suggestion request for an ordinary completed
    /// reply. Tool-call turns, mock replies, image models, and autonomous loops are
    /// excluded; their next action is already determined by the workflow.
    pub(super) fn maybe_request_response_suggestions(&mut self, sid: usize) {
        if !self.config.ui.response_suggestions
            || self.is_mock()
            || self.config.api.endpoint.trim().is_empty()
            || crate::api::is_image_model(self.current_model())
            || !self.tool_calls_in(sid).is_empty()
        {
            return;
        }
        let Some(session) = self.sessions.by_id(sid) else {
            return;
        };
        if session.loop_state.is_some()
            || session
                .messages
                .iter()
                .rev()
                .find(|message| message.role == "assistant")
                .is_some_and(|message| message.mock)
        {
            return;
        }
        let Some((user_message, assistant_reply)) = session.latest_completed_turn() else {
            return;
        };
        let signature = crate::app::suggestions::turn_signature(&user_message, &assistant_reply);
        if !self.suggestion_inflight.insert((sid, signature)) {
            return;
        }
        let Some(api) = self.api.clone() else {
            self.suggestion_inflight.remove(&(sid, signature));
            return;
        };
        let (system, user) = crate::app::suggestions::build_prompt(&user_message, &assistant_reply);
        let fallback_model = self.current_model().to_string();
        let configured = self.config.api.response_suggestion_model.trim();
        let primary_model = if configured.is_empty() {
            fallback_model.clone()
        } else {
            configured.to_string()
        };
        let mut models = vec![primary_model];
        if !models.contains(&fallback_model) {
            models.push(fallback_model);
        }
        let tx = self.suggestion_tx.clone();
        tokio::spawn(async move {
            let mut suggestions = Vec::new();
            for model in models {
                let mut request = ChatRequest::new(
                    &model,
                    vec![
                        ChatMessage::system(system.clone()),
                        ChatMessage::user(user.clone()),
                    ],
                );
                request.stream = false;
                request.stream_options = None;
                request.max_tokens = Some(128);
                if let Ok(reply) = api.complete(request).await {
                    suggestions = crate::app::suggestions::parse(&reply);
                    break;
                }
            }
            let _ = tx.send((sid, signature, suggestions)).await;
        });
    }

    pub(super) fn apply_response_suggestions(
        &mut self,
        sid: usize,
        signature: u64,
        suggestions: Vec<String>,
    ) {
        self.suggestion_inflight.remove(&(sid, signature));
        if !self.config.ui.response_suggestions {
            return;
        }
        let Some(session) = self.sessions.by_id_mut(sid) else {
            return;
        };
        let agent_mode = session.agent_mode;
        let Some((user_message, assistant_reply)) = session.latest_completed_turn() else {
            return;
        };
        if crate::app::suggestions::turn_signature(&user_message, &assistant_reply) != signature {
            return;
        }
        session.response_suggestions = if suggestions.is_empty() {
            crate::app::suggestions::fallback(agent_mode)
        } else {
            suggestions
        };
        self.touch();
    }

    pub(super) fn accept_response_suggestion(&mut self, index: usize) {
        if !self.input.text().trim().is_empty() {
            self.set_status("Suggestions only fill an empty composer");
            return;
        }
        let suggestion = self
            .sessions
            .active()
            .response_suggestions
            .get(index)
            .cloned();
        let Some(suggestion) = suggestion else {
            return;
        };
        self.input.set_text(&suggestion);
        self.sessions.active_mut().response_suggestions.clear();
        self.vim = crate::input::vim::VimMode::Insert;
        self.set_status("Suggestion inserted — edit or press Enter to send");
        self.touch();
    }

    /// Start a parallel task-tracker call for a completed agent turn. The active
    /// agent never maintains the checklist itself; this separate model call
    /// updates per-item status, per-item percent, and overall progress.
    /// Request a task-checklist revision from the parallel tracker agent.
    /// `child_reports` are completed child-agent summaries (name + body) whose
    /// work the tracker must fold into the checklist; empty for a plain turn.
    pub(super) fn maybe_request_todo_update(
        &mut self,
        sid: usize,
        child_reports: &[(String, String)],
    ) {
        if !self.config.ui.auto_todo_tracker
            || self.is_mock()
            || self.config.api.endpoint.trim().is_empty()
            || crate::api::is_image_model(self.current_model())
        {
            return;
        }
        let Some(session) = self.sessions.by_id(sid) else {
            return;
        };
        if !session.agent_mode {
            return;
        }
        let Some((user_message, assistant_reply)) = session.latest_completed_turn() else {
            return;
        };
        let base_signature = crate::app::todo_tracker::update_signature(
            &user_message,
            &assistant_reply,
            &session.todos,
            &[],
        );
        let signature = crate::app::todo_tracker::update_signature(
            &user_message,
            &assistant_reply,
            &session.todos,
            child_reports,
        );
        // Guard against a stale result clobbering a newer turn: track the
        // base (child-less) signature so apply can verify the conversation
        // hasn't moved on since the request was made.
        if self.todo_inflight.contains_key(&(sid, signature)) {
            return;
        }
        self.todo_inflight.insert((sid, signature), base_signature);
        let Some(api) = self.api.clone() else {
            self.todo_inflight.remove(&(sid, signature));
            return;
        };
        let todos = session.todos.clone();
        let (system, user) = crate::app::todo_tracker::build_prompt(
            &user_message,
            &assistant_reply,
            &todos,
            child_reports,
        );
        let model = if self.config.agent.task_model.trim().is_empty() {
            self.current_model().to_string()
        } else {
            self.config.agent.task_model.clone()
        };
        let tx = self.todo_tx.clone();
        tokio::spawn(async move {
            let mut request = ChatRequest::new(
                &model,
                vec![ChatMessage::system(system), ChatMessage::user(user)],
            );
            request.stream = false;
            request.stream_options = None;
            request.max_tokens = Some(512);
            let result = match api.complete(request).await {
                Ok(reply) => {
                    let update = crate::app::todo_tracker::parse(&reply);
                    if update.items.is_empty() {
                        Err("task tracker returned no items".to_string())
                    } else {
                        Ok(update)
                    }
                }
                Err(error) => Err(error.to_string()),
            };
            let _ = tx.send((sid, signature, result)).await;
        });
    }

    pub(super) fn apply_todo_update(
        &mut self,
        sid: usize,
        signature: u64,
        result: Result<crate::app::state::TodoUpdate, String>,
    ) {
        let base_signature = self.todo_inflight.remove(&(sid, signature));
        let Ok(update) = result else {
            eprintln!(
                "Todo tracker skipped for session {}: {}",
                sid,
                result.unwrap_err()
            );
            return;
        };
        let Some(session) = self.sessions.by_id_mut(sid) else {
            return;
        };
        let Some((user_message, assistant_reply)) = session.latest_completed_turn() else {
            return;
        };
        // Drop the result if the conversation moved on since the request was
        // made (the child-less base signature no longer matches). A newer turn
        // triggers its own tracker call with fresh context.
        let Some(base_signature) = base_signature else {
            return;
        };
        if crate::app::todo_tracker::update_signature(
            &user_message,
            &assistant_reply,
            &session.todos,
            &[],
        ) != base_signature
        {
            return;
        }
        session.todos = update.items;
        session.todo_overall_percent = update.overall_percent;
        self.sessions.save();
        self.touch();
    }

    pub(super) fn set_response_suggestions(&mut self, enabled: bool) {
        self.config.ui.response_suggestions = enabled;
        let _ = self.config.save();
        if !enabled {
            for session in self.sessions.all_mut() {
                session.response_suggestions.clear();
            }
        } else {
            let sid = self.sessions.active_id();
            self.maybe_request_response_suggestions(sid);
        }
        self.set_status(format!(
            "Response suggestions: {}",
            if enabled { "ON" } else { "off" }
        ));
        self.touch();
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
        self.spec_inflight
            .store(0, std::sync::atomic::Ordering::Relaxed);
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
                .map(build_image_prompt)
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
                    Ok(rx) => match cold_retries {
                        Some(n) => Some(Action::AttachRetriedStream(sid, rx, n)),
                        None => Some(Action::AttachStream(sid, rx)),
                    },
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
            .map(|s| s.api_messages_windowed(true, Some(char_budget), &self.config.agents))
            .unwrap_or_default();
        if let Some(memory) = self
            .sessions
            .by_id(sid)
            .and_then(|session| crate::app::memory::context_block(&session.memories))
        {
            prepend_or_merge_system(&mut messages, memory);
        }
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
                l.goal,
                l.stop,
                l.iteration + 1,
                l.max
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
            .with_reasoning(self.reasoning_effort.clone(), self.reasoning_mode.clone());
        // Send tool schemas so the model returns structured tool_calls instead of
        // <tool> tags (agent turns only).
        if self
            .sessions
            .by_id(sid)
            .map(|s| s.agent_mode)
            .unwrap_or(false)
        {
            let loop_active = self
                .sessions
                .by_id(sid)
                .is_some_and(|session| session.loop_state.is_some());
            request = request.with_tools(crate::agent::tool_schemas_for_loop(loop_active));
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
        // The parallel task tracker maintains the checklist: after every
        // completed response, a separate call revises task status/progress.
        if self
            .sessions
            .by_id(sid)
            .map(|s| s.agent_mode)
            .unwrap_or(false)
        {
            self.maybe_request_todo_update(sid, &[]);
        }
        let has_tools = !self.tool_calls_in(sid).is_empty();
        if !has_tools {
            self.maybe_request_session_memory(sid);
            // The turn ended with a plain reply. If this session is running an
            // autonomous loop, keep it going (or stop it at the cap).
            if let Some(follow) = self.maybe_continue_loop(sid) {
                return Some(follow);
            }
            // Delegated children still working: the parent did all it could on
            // its own; hold the hand-off until every child completes, then feed
            // the reports back for the final synthesis.
            if let Some(barrier) = self.task_barrier.as_ref() {
                if barrier.session_id == sid {
                    let running = barrier
                        .task_ids
                        .iter()
                        .filter(|task_id| {
                            self.subtasks.iter().any(|task| {
                                task.id == **task_id
                                    && task.status == crate::app::state::SubtaskStatus::Running
                            })
                        })
                        .count();
                    if running > 0 {
                        self.set_status(format!(
                            "Delegated {} parallel agent(s) still working — finishing when they complete",
                            running
                        ));
                        return None;
                    }
                }
            }
            self.maybe_request_response_suggestions(sid);
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
                "Agent stopped after {} rounds (loop guard).",
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
            Some(message) => crate::agent::parser::committed_tool_calls(&message_text(message)),
            None => Vec::new(),
        }
    }

    pub fn process_next_tool(&mut self) -> Option<Action> {
        let cwd = self.agent_cwd();
        // Calls already cleared this round (judged-allow or batch-allow) run first,
        // straight to execution with no fresh permission check or re-judge.
        if let Some(call) = self.approved.pop_front() {
            let mut calls = vec![call];
            if is_parallel_read(&calls[0]) {
                while calls.len() < 16 && self.approved.front().is_some_and(is_parallel_read) {
                    if let Some(next) = self.approved.pop_front() {
                        calls.push(next);
                    }
                }
            }
            return self.execute_tool_batch(calls);
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
            if kind == ToolKind::ProposeStep {
                return self.open_step_decision(call);
            }
            if kind == ToolKind::Task {
                let mut calls = vec![call];
                while self
                    .pending_tools
                    .front()
                    .is_some_and(|next| next.kind() == Some(ToolKind::Task))
                {
                    calls.push(self.pending_tools.pop_front().unwrap());
                }
                return self.start_task_batch(calls);
            }
            if kind == ToolKind::Finish {
                let res = self.apply_finish(&call);
                self.record_tool_result(res);
                continue;
            }
            let remembered = self.permissions.check(&call, &cwd);
            match remembered {
                Some(PermissionDecision::Deny) => {
                    self.permissions.consume(&call, &cwd);
                    let res = ToolResult::failure(
                        call.clone(),
                        "Skipped: denied by session policy".into(),
                        0,
                    );
                    self.record_tool_result(res);
                }
                Some(PermissionDecision::Allow) if agent::needs_hard_prompt(&call, &cwd) => {
                    return self.prompt_permission(vec![call]);
                }
                Some(PermissionDecision::Allow) => {
                    self.permissions.consume(&call, &cwd);
                    let mut calls = vec![call];
                    if is_parallel_read(&calls[0]) {
                        while calls.len() < 8 {
                            let Some(next) = self.pending_tools.front() else {
                                break;
                            };
                            if !is_parallel_read(next)
                                || agent::needs_hard_prompt(next, &cwd)
                                || self.permissions.check(next, &cwd)
                                    != Some(PermissionDecision::Allow)
                            {
                                break;
                            }
                            let Some(next) = self.pending_tools.pop_front() else {
                                break;
                            };
                            self.permissions.consume(&next, &cwd);
                            calls.push(next);
                        }
                    }
                    return self.execute_tool_batch(calls);
                }
                None if agent::needs_hard_prompt(&call, &cwd) => {
                    return self.prompt_permission(vec![call]);
                }
                None => {
                    let mut calls = vec![call];
                    while let Some(next) = self.pending_tools.front() {
                        if next.kind().is_none()
                            || matches!(
                                next.kind(),
                                Some(
                                    ToolKind::Todo
                                        | ToolKind::Ask
                                        | ToolKind::Plan
                                        | ToolKind::ProposeStep
                                )
                            )
                        {
                            break;
                        }
                        if self.permissions.check(next, &cwd).is_some() {
                            break;
                        }
                        calls.push(self.pending_tools.pop_front().unwrap());
                    }
                    // Let the configured reviewer triage the batch before
                    // bothering the human. A custom session policy overrides the
                    // strict/lenient default policy; offline/mock mode still prompts.
                    if self.access_review_policy().is_some()
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
    ///
    /// `reason` is the optional note the user attached to a deny; it goes back to
    /// the model as part of the tool result so it can adapt instead of retrying.
    pub fn resolve_permission(
        &mut self,
        perm: Permission,
        reason: Option<String>,
    ) -> Option<Action> {
        let calls = match &self.overlay {
            Overlay::Permission(req) => req.calls.clone(),
            _ => return None,
        };
        self.overlay = Overlay::None;
        self.notification_generation = self.notification_generation.wrapping_add(1);
        crate::app::notify::dismiss();

        let allow = matches!(
            perm,
            Permission::Allow
                | Permission::AllowKind
                | Permission::AllowDirectory
                | Permission::AllowTimed
        );
        let decision = match &perm {
            Permission::Custom(rule) => rule.decision,
            _ if allow => PermissionDecision::Allow,
            _ => PermissionDecision::Deny,
        };
        let allow = decision == PermissionDecision::Allow;
        let cwd = self.agent_cwd();

        if let Permission::Custom(rule) = &perm {
            self.permissions.remember_custom_rule(rule.clone());
            let mut unmatched = Vec::new();
            for call in calls {
                if !rule.matches(&call, &cwd) {
                    unmatched.push(call);
                } else if allow {
                    self.approved.push_back(call);
                } else {
                    let text = deny_text(reason.as_deref());
                    self.record_tool_result(ToolResult::failure(call, text, 0));
                }
            }
            for call in unmatched.into_iter().rev() {
                self.pending_tools.push_front(call);
            }
            return self.process_next_tool();
        }

        for call in &calls {
            match &perm {
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
                Permission::Allow | Permission::Deny | Permission::Custom(_) => {}
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
            let text = deny_text(reason.as_deref());
            for call in calls {
                let res = ToolResult::failure(call, text.clone(), 0);
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
            &[
                crate::app::notify::DesktopAction::Review,
                crate::app::notify::DesktopAction::AllowOnce,
                crate::app::notify::DesktopAction::DenyOnce,
            ],
        );
        self.overlay = Overlay::Permission(PermissionRequest::new(calls, self.agent_cwd()));
        self.set_status(
            "Access — ↑↓ option · ←→ phrase · a allow · d deny · e edit · p policy · ⏎ model review · Esc cancel",
        );
        None
    }

    /// Effective policy for automatic permission review. A session-specific policy
    /// wins; otherwise the configured strict/lenient baseline is used. Off returns
    /// `None`, which routes every uncovered call to the human prompt.
    pub(super) fn access_review_policy(&self) -> Option<String> {
        if self.config.api.access_review_mode == crate::config::AccessReviewMode::Off {
            return None;
        }
        self.permissions.policy.clone().or_else(|| {
            self.config
                .api
                .access_review_mode
                .default_policy()
                .map(str::to_string)
        })
    }

    /// Whether a live model is reachable to run the access judge (never in mock /
    /// offline mode — there's nothing to ask).
    fn can_judge(&self) -> bool {
        !self.is_mock() && !self.config.api.endpoint.trim().is_empty()
    }

    /// Whether any call in the batch is eligible for the judge — i.e. not on the
    /// safety floor. If every call is floored, judging is pointless: prompt directly.
    fn batch_has_judgeable(&self, calls: &[ToolCall], cwd: &Path) -> bool {
        calls.iter().any(|c| !agent::needs_hard_prompt(c, cwd))
    }

    /// Spawn the async access-policy judge for `calls`. The fast judge model
    /// classifies each call allow/deny/ask against the supplied policy; verdicts
    /// return over `judge_rx` as an `AccessJudged` action. The batch is stashed in
    /// `self.judging` meanwhile.
    fn begin_judge(&mut self, calls: Vec<ToolCall>) -> Option<Action> {
        let policy = self.access_review_policy().unwrap_or_default();
        let review_label = if self.permissions.policy.is_some() {
            "custom".to_string()
        } else {
            self.config.api.access_review_mode.label().to_string()
        };
        self.begin_judge_with_policy(calls, policy, review_label, None)
    }

    /// Review the open permission batch against the exact access rule assembled in
    /// the overlay. This does not remember the rule; it lets the model decide which
    /// pending calls satisfy it, while the hard safety floor still forces a prompt.
    pub fn review_permission(&mut self) -> Option<Action> {
        let (calls, policy, rule) = match &self.overlay {
            Overlay::Permission(req) => (
                req.calls.clone(),
                req.review_policy(),
                match req.permission() {
                    Permission::Custom(rule) => Some(rule),
                    _ => None,
                },
            ),
            _ => return None,
        };
        if !self.can_judge() {
            self.set_status("Automated review needs a configured live model");
            return None;
        }
        self.overlay = Overlay::None;
        self.begin_judge_with_policy(calls, policy, "selected rule".to_string(), rule)
    }

    fn begin_judge_with_policy(
        &mut self,
        calls: Vec<ToolCall>,
        policy: String,
        review_label: String,
        reviewed_rule: Option<crate::agent::PermissionRuleDraft>,
    ) -> Option<Action> {
        let sid = self.agent_sid();
        let cwd = self.agent_cwd();
        let user_request = self
            .sessions
            .by_id(sid)
            .and_then(|session| session.messages.iter().rev().find(|m| m.role == "user"))
            .map(message_text)
            .unwrap_or_default();
        let descs: Vec<(usize, String)> = calls
            .iter()
            .enumerate()
            .map(|(i, c)| (i, agent::describe_call(c, &cwd)))
            .collect();
        let (system, user) = agent::access::build_judge_prompt(&policy, &user_request, &descs);
        let n = calls.len();
        let fallback_model = self.current_model().to_string();
        let primary_model = self.config.api.access_judge_model.trim();
        let primary_model = if primary_model.is_empty() {
            fallback_model.clone()
        } else {
            primary_model.to_string()
        };
        let mut review_models = vec![primary_model];
        if !review_models.contains(&fallback_model) {
            review_models.push(fallback_model);
        }
        let endpoint = self.config.api.endpoint.clone();
        let key = self.config.api.api_key.clone();

        let (tx, rx) = mpsc::channel(1);
        self.judge_rx = Some(rx);
        self.judging = Some(JudgeBatch {
            session_id: sid,
            calls,
            reviewed_rule,
        });
        self.set_status(format!(
            "Reviewing {} call{} ({})…",
            n,
            if n == 1 { "" } else { "s" },
            review_label
        ));
        self.touch();

        self.judge_task = Some(tokio::spawn(async move {
            // Any failure (no client, every review model unavailable, HTTP error,
            // or parse miss) degrades to all-Ask: fall back to the human.
            let verdicts = match ApiClient::new(&endpoint, &key) {
                Ok(client) => {
                    let mut parsed = None;
                    for model in review_models {
                        let mut req = ChatRequest::new(
                            &model,
                            vec![
                                ChatMessage::system(system.clone()),
                                ChatMessage::user(user.clone()),
                            ],
                        );
                        req.stream = false;
                        req.stream_options = None;
                        req.max_tokens = Some(256);
                        if let Ok(reply) = client.complete(req).await {
                            parsed = Some(agent::access::parse_verdicts(&reply, n));
                            break;
                        }
                    }
                    parsed.unwrap_or_else(|| vec![AccessVerdict::Ask; n])
                }
                Err(_) => vec![AccessVerdict::Ask; n],
            };
            let _ = tx.send((sid, verdicts)).await;
        }));
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
        self.judge_task = None;
        let batch = self.judging.take()?;
        // Stale result (session switched / round cancelled) — drop it.
        if batch.session_id != sid {
            return None;
        }
        let cwd = self.agent_cwd();
        let reviewed_rule = batch.reviewed_rule;
        let mut ask: Vec<ToolCall> = Vec::new();
        let mut allowed = 0usize;
        let mut denied = 0usize;
        for (i, call) in batch.calls.into_iter().enumerate() {
            let mut verdict = verdicts.get(i).copied().unwrap_or(AccessVerdict::Ask);
            if reviewed_rule
                .as_ref()
                .is_some_and(|rule| !rule.matches(&call, &cwd))
            {
                verdict = AccessVerdict::Ask;
            }
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
        if let Some(rule) = reviewed_rule {
            let confirmed = match rule.decision {
                PermissionDecision::Allow => allowed > 0,
                PermissionDecision::Deny => denied > 0,
            };
            if confirmed {
                self.permissions.remember_custom_rule(rule);
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
    /// is already open, re-triage that batch under the effective policy right away.
    pub fn set_access_policy(&mut self, text: &str) -> Option<Action> {
        self.permissions.set_policy(text);
        match self.permissions.policy.clone() {
            Some(p) => {
                let short: String = p.chars().take(60).collect();
                self.set_status(
                    if self.config.api.access_review_mode == crate::config::AccessReviewMode::Off {
                        format!("Access policy saved (review agent remains off): {}", short)
                    } else {
                        format!("Access policy set: {}", short)
                    },
                );
            }
            None => {
                let mode = self.config.api.access_review_mode;
                self.set_status(if mode == crate::config::AccessReviewMode::Off {
                    "Custom access policy cleared — tool calls prompt normally".to_string()
                } else {
                    format!(
                        "Custom access policy cleared — using {} review",
                        mode.label()
                    )
                });
            }
        }
        self.retriage_open_permission()
    }

    /// Turn review off immediately. If a judge request is active, abort its task,
    /// recover the untouched batch, and route it straight to the human prompt.
    pub fn disable_access_review(&mut self) -> Option<Action> {
        self.config.api.access_review_mode = crate::config::AccessReviewMode::Off;
        let _ = self.config.save();
        if let Some(task) = self.judge_task.take() {
            task.abort();
        }
        self.judge_rx = None;
        if let Some(batch) = self.judging.take() {
            self.overlay = Overlay::None;
            self.set_status("Permission review disabled — asking you directly");
            return self.prompt_permission(batch.calls);
        }
        self.overlay = Overlay::Picker(crate::app::overlay::Picker::access(self.access_items()));
        self.set_status("Permission review: off");
        self.touch();
        None
    }

    /// Persist and apply the default automated review strictness. A custom policy
    /// remains stored and becomes active again when review is switched back on.
    pub fn set_access_review_mode(
        &mut self,
        mode: crate::config::AccessReviewMode,
    ) -> Option<Action> {
        self.config.api.access_review_mode = mode;
        let _ = self.config.save();
        self.set_status(format!("Permission review: {}", mode.label()));
        self.retriage_open_permission()
    }

    fn retriage_open_permission(&mut self) -> Option<Action> {
        let calls = match &self.overlay {
            Overlay::Permission(req) => req.calls.clone(),
            _ => return None,
        };
        self.overlay = Overlay::None;
        let cwd = self.agent_cwd();
        if self.access_review_policy().is_some()
            && self.can_judge()
            && self.batch_has_judgeable(&calls, &cwd)
        {
            return self.begin_judge(calls);
        }
        self.prompt_permission(calls)
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
            self.set_status("Commands updated — a allow · d deny · e edit again · ⏎ model review");
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
        self.set_status("Commands updated — a allow · d deny · e edit again · ⏎ model review");
        self.touch();
        None
    }

    fn open_step_decision(&mut self, call: ToolCall) -> Option<Action> {
        let title = call
            .args
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Choose path");
        let description = call
            .args
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let options: Vec<String> = call
            .args
            .get("alternatives")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let label = item.get("label")?.as_str()?;
                        let detail = item
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let feasibility = item
                            .get("feasibility")
                            .and_then(|v| v.as_str())
                            .unwrap_or("possible");
                        let actions = item
                            .get("actions")
                            .and_then(|v| v.as_array())
                            .map(|values| {
                                values
                                    .iter()
                                    .filter_map(|v| v.as_str())
                                    .collect::<Vec<_>>()
                                    .join("; ")
                            })
                            .unwrap_or_default();
                        let mut text = format!("{} — {} [{}]", label, detail, feasibility);
                        if !actions.is_empty() {
                            text.push_str(&format!(" Actions: {}", actions));
                        }
                        Some(text)
                    })
                    .collect()
            })
            .unwrap_or_default();
        if options.len() < 2 {
            let res = ToolResult::failure(
                call,
                "propose_step requires at least two explained alternatives; use the obvious path directly otherwise"
                    .into(),
                0,
            );
            self.record_tool_result(res);
            return self.process_next_tool();
        }
        let question = if description.is_empty() {
            title.to_string()
        } else {
            format!("{}\n{}", title, description)
        };
        self.notify_desktop(
            "AiTUI — path choice",
            title.to_string(),
            &[crate::app::notify::DesktopAction::Review],
        );
        self.overlay = Overlay::Decision(DecisionRequest {
            call,
            question,
            options,
            selected: 0,
            chosen: BTreeSet::new(),
            multi: false,
            answer: String::new(),
            custom_editing: false,
        });
        self.set_status("Path choice — ↑↓ move · e edit · Tab custom · Enter choose");
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
        let multi = call
            .args
            .get("multi")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut chosen = BTreeSet::new();
        if multi && !options.is_empty() {
            chosen.insert(0);
        }
        self.notify_desktop(
            "AiTUI — question",
            question.clone(),
            &[crate::app::notify::DesktopAction::Review],
        );
        self.overlay = Overlay::Decision(DecisionRequest {
            call,
            question,
            options,
            selected: 0,
            chosen,
            multi,
            answer: String::new(),
            custom_editing: false,
        });
        self.set_status(match (multi, self.overlay_decision_free_form()) {
            (_, true) => "Question — type answer · ⏎ submit · Esc cancel",
            (true, false) => "Decision — ↑↓ choose · space toggle · ⏎ confirm · Esc cancel",
            (false, false) => "Decision — ↑↓ choose · ⏎ confirm · Esc cancel",
        });
        None
    }

    fn overlay_decision_free_form(&self) -> bool {
        matches!(&self.overlay, Overlay::Decision(req) if req.free_form())
    }

    pub fn resolve_decision(&mut self) -> Option<Action> {
        let req = match &self.overlay {
            Overlay::Decision(req) => req.clone(),
            _ => return None,
        };
        self.overlay = Overlay::None;
        let output = if req.free_form() {
            req.answer
        } else {
            let labels = req.labels();
            if req.multi {
                serde_json::to_string(&labels).unwrap_or_else(|_| "[]".to_string())
            } else {
                labels.first().cloned().unwrap_or_default()
            }
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
        self.notify_desktop(
            "AiTUI — plan approval needed",
            path.display().to_string(),
            &[
                crate::app::notify::DesktopAction::Review,
                crate::app::notify::DesktopAction::AcceptPlan,
                crate::app::notify::DesktopAction::RejectPlan,
            ],
        );
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

    fn notify_desktop(
        &mut self,
        title: impl Into<String>,
        body: impl Into<String>,
        actions: &[crate::app::notify::DesktopAction],
    ) {
        self.notification_generation = self.notification_generation.wrapping_add(1);
        if !self.focused {
            crate::app::notify::desktop(
                title,
                body,
                actions,
                self.notification_generation,
                self.notification_tx.clone(),
            );
        }
    }

    /// While an agent reply is streaming, pre-run any *complete*, side-effect-free
    /// read-only tool block it has emitted so far, in the background, so the result
    /// is already sitting in `spec_results` the moment the turn finishes and the
    /// tool round starts. Never touches tools that mutate or run commands.
    /// Bounded by a semaphore (default 8 concurrent) to prevent thread I/O overload.
    pub fn speculate_read_tools(&mut self, sid: usize) {
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
        let inflight = self.spec_inflight.clone();
        const MAX_SPEC_INFLIGHT: usize = 16;
        // Educated guesses from the streaming plan text: every file the reply
        // mentions reading (or cites in a plan bullet) gets pre-read in parallel
        // while the model is still talking, so a later committed `read` of the
        // same path is answered instantly from the speculative result.
        for path in crate::agent::parser::plan_read_guesses(&partial) {
            let mut args = serde_json::Map::new();
            args.insert("path".into(), serde_json::json!(path));
            let call = ToolCall {
                name: "read".into(),
                args: serde_json::Value::Object(args),
                id: None,
            };
            if !is_speculatable(&call) {
                continue;
            }
            let sig = spec_sig(&call);
            if !self.spec_dispatched.insert(sig) {
                continue;
            }
            if inflight.load(std::sync::atomic::Ordering::Relaxed) >= MAX_SPEC_INFLIGHT {
                continue;
            }
            inflight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let tx = self.spec_tx.clone();
            let cwd = cwd.clone();
            let inflight_child = inflight.clone();
            tokio::task::spawn_blocking(move || {
                let _ = tx.blocking_send((epoch, agent::execute(call, &cwd)));
                inflight_child.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            });
        }
        for call in crate::agent::parser::visible_tool_calls(&partial) {
            if !is_speculatable(&call) {
                continue;
            }
            let sig = spec_sig(&call);
            if !self.spec_dispatched.insert(sig) {
                continue;
            }
            // Backpressure: skip speculation if too many in-flight.
            if inflight.load(std::sync::atomic::Ordering::Relaxed) >= MAX_SPEC_INFLIGHT {
                continue;
            }
            inflight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let tx = self.spec_tx.clone();
            let cwd = cwd.clone();
            let inflight_child = inflight.clone();
            tokio::task::spawn_blocking(move || {
                let _ = tx.blocking_send((epoch, agent::execute(call, &cwd)));
                inflight_child.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            });
        }
    }

    /// Whether the streaming reply for `sid` should be cut now: it's an agent-mode
    /// session, still streaming, and its partial already contains at least one
    /// complete ````tool```` call. Cutting here stops the model from generating a
    /// pile of redundant calls it can't get results for until the turn ends.
    ///
    /// Only *visible* fences count — a call sketched inside reasoning is a draft the
    /// model may still discard, and cutting on it would run something it never
    /// committed to. A reply routed entirely through the reasoning channel still runs
    /// its calls; it just runs them at the end of the turn (see `tool_calls_in`).
    ///
    /// One exception: a call inside a *closed* `<think>…</think>`/`<thinking>…</thinking>`
    /// block that the model wrote in its own content (interleaved-thinking style).
    /// Once the block is closed the model has committed to the call — and models in
    /// this mode typically stop right after, waiting for the harness to run it. Not
    /// cutting then hangs the turn forever (see `closed_thinking_calls`).
    pub fn should_cut_stream(&self, sid: usize) -> bool {
        let Some(s) = self.sessions.by_id(sid) else {
            return false;
        };
        if !s.agent_mode || !s.is_streaming() {
            return false;
        }
        let Some(partial) = s.streaming_display() else {
            return false;
        };
        if !crate::agent::parser::visible_tool_calls(&partial).is_empty() {
            return true;
        }
        // The reasoning channel wraps into `<think>` via `streaming_display`, so a
        // closed-thinking commitment must be detected on the raw content buffer:
        // only a block the model authored in its reply counts.
        match s.pending_assistant_text.as_deref() {
            Some(text) => !crate::agent::parser::closed_thinking_calls(text).is_empty(),
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

    fn execute_tool_batch(&mut self, calls: Vec<ToolCall>) -> Option<Action> {
        if calls.len() == 1 {
            let Some(call) = calls.into_iter().next() else {
                return self.process_next_tool();
            };
            return self.execute_tool(call);
        }
        let sid = self.agent_sid();
        let cwd = self
            .sessions
            .by_id(sid)
            .and_then(|s| s.cwd.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let mut results = vec![None; calls.len()];
        let mut workers = Vec::new();
        let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.agent_abort = Some(abort.clone());
        for (index, call) in calls.into_iter().enumerate() {
            if let Some(result) = self.spec_results.remove(&spec_sig(&call)) {
                results[index] = Some(result);
            } else {
                let worker_cwd = cwd.clone();
                let worker_abort = abort.clone();
                let failed_call = call.clone();
                workers.push((
                    index,
                    failed_call,
                    tokio::task::spawn_blocking(move || {
                        agent::execute_abortable(call, &worker_cwd, &worker_abort)
                    }),
                ));
            }
        }
        self.set_status(format!(
            "Running {} read-only tools in parallel",
            results.len()
        ));
        self.active_tool = Some((
            format!("{} parallel read-only tools", results.len()),
            std::time::Instant::now(),
        ));
        self.touch();
        let (tx, rx) = mpsc::channel(1);
        self.agent_tool_batch_rx = Some(rx);
        tokio::spawn(async move {
            for (index, call, worker) in workers {
                let result = worker.await.unwrap_or_else(|error| {
                    ToolResult::failure(call, format!("Tool task failed: {}", error), 0)
                });
                results[index] = Some(result);
            }
            let ordered = results.into_iter().flatten().collect();
            let _ = tx.send(ordered).await;
        });
        None
    }

    fn execute_tool(&mut self, call: ToolCall) -> Option<Action> {
        // If this exact call was pre-run while the reply streamed, use that result
        // instantly instead of spawning the work again.
        if let Some(result) = self.spec_results.remove(&spec_sig(&call)) {
            self.set_status(call.summary());
            self.record_tool_result(result);
            return self.process_next_tool();
        }
        let summary = call.summary();
        self.set_status(format!("Running: {}", summary));
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
        let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.agent_abort = Some(abort.clone());
        let (tx, rx) = mpsc::channel(1);
        self.agent_tool_rx = Some(rx);
        tokio::task::spawn_blocking(move || {
            let _ = tx.blocking_send(agent::execute_abortable(call, &cwd, &abort));
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
                Some(crate::app::state::TodoItem {
                    text,
                    status,
                    percent: None,
                })
            })
            .collect();
        let n = todos.len();
        let sid = self.agent_sid();
        if let Some(s) = self.sessions.by_id_mut(sid) {
            s.todos = todos;
        }
        self.touch();
        ToolResult::success(
            call.clone(),
            format!(
                "Todo panel updated ({} item{}). Keep updating at each task boundary; mark each completed item done immediately before unrelated work.",
                n,
                if n == 1 { "" } else { "s" }
            ),
            0,
        )
    }

    pub(crate) fn sync_subtask_message(&mut self, id: u64) {
        let Some(task) = self.subtasks.iter().find(|task| task.id == id) else {
            return;
        };
        let body = subtask_message_body(task);
        let duration_ms = task.duration_ms;
        let sid = task.session_id;
        let message_index = task.message_index;
        let mut updated = ChatMessage::tool(body);
        updated.duration_ms = duration_ms;
        let marker = format!("[agent-id:{}]", id);
        let mut inserted = None;
        if let Some(session) = self.sessions.by_id_mut(sid) {
            let target = session
                .messages
                .get(message_index)
                .filter(|message| message_text(message).contains(&marker))
                .map(|_| message_index)
                .or_else(|| {
                    session
                        .messages
                        .iter()
                        .position(|message| message_text(message).contains(&marker))
                });
            if let Some(index) = target {
                session.messages[index] = updated;
                inserted = Some(index);
            } else {
                let index = session.messages.len();
                session.push_message(updated);
                inserted = Some(index);
            }
        }
        if let Some(index) = inserted {
            if let Some(task) = self.subtasks.iter_mut().find(|task| task.id == id) {
                task.message_index = index;
            }
        }
    }

    /// Launch consecutive agent calls concurrently and pause the parent tool round at
    /// a barrier. The parent resumes only after every child has produced a report,
    /// so later tools can safely depend on the complete batch.
    fn start_task_batch(&mut self, calls: Vec<ToolCall>) -> Option<Action> {
        let sid = self.agent_sid();
        let default_cwd = self
            .sessions
            .by_id(sid)
            .and_then(|session| session.cwd.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let mut task_ids = Vec::new();

        for mut call in calls {
            let agent_name = call
                .args
                .get("agent")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let agent_def = agent_name
                .as_deref()
                .and_then(|name| self.config.agents.get(name))
                .cloned();
            let prompt = call
                .args
                .get("prompt")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if prompt.is_empty() {
                self.record_tool_result(ToolResult::failure(
                    call,
                    "agent: missing 'prompt'".into(),
                    0,
                ));
                continue;
            }
            let description = call
                .args
                .get("description")
                .and_then(|value| value.as_str())
                .unwrap_or("parallel child agent")
                .trim()
                .to_string();
            let checks: Vec<crate::agent::report::CheckSpec> = call
                .args
                .get("checks")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or_default();
            let verification = call
                .args
                .get("verification")
                .and_then(|value| value.as_str())
                .filter(|value| matches!(*value, "none" | "replicate"))
                .unwrap_or(if checks.is_empty() {
                    "none"
                } else {
                    "replicate"
                })
                .to_string();
            let todo_index = call
                .args
                .get("task_index")
                .and_then(|value| value.as_u64())
                .and_then(|value| usize::try_from(value).ok())
                .filter(|value| *value > 0);
            let cwd = call
                .args
                .get("cwd")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .map(|path| {
                    if path.is_absolute() {
                        path
                    } else {
                        default_cwd.join(path)
                    }
                })
                .unwrap_or_else(|| default_cwd.clone());
            let id = self
                .subtask_id_alloc
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let display_index = self
                .subtasks
                .iter()
                .filter(|task| task.session_id == sid)
                .count()
                + 1;
            if let Some(args) = call.args.as_object_mut() {
                args.insert("agent_index".into(), serde_json::json!(display_index));
            }
            task_ids.push(id);
            self.selected_subtask = Some(id);
            self.subtasks.push(crate::app::state::Subtask {
                id,
                session_id: sid,
                parent_id: None,
                call: call.clone(),
                description,
                todo_index,
                prompt: prompt.clone(),
                cwd: cwd.clone(),
                status: crate::app::state::SubtaskStatus::Running,
                activity: None,
                log: Vec::new(),
                transcript: Vec::new(),
                output: None,
                message_index: usize::MAX,
                started_at: std::time::Instant::now(),
                duration_ms: None,
                abort: None,
                agent: agent_name,
            });
            self.sync_subtask_message(id);
            let spec = crate::agent::subtask::SubtaskSpec {
                id,
                next_id: Some(self.subtask_id_alloc.clone()),
                capture: true,
                prompt,
                checks,
                verification,
                cwd,
                model: agent_def
                    .as_ref()
                    .and_then(|def| def.model.clone())
                    .filter(|model| !model.trim().is_empty())
                    .or_else(|| {
                        let configured = self.config.agent.child_model.trim();
                        if configured.is_empty() {
                            None
                        } else {
                            Some(configured.to_string())
                        }
                    })
                    .unwrap_or_else(|| self.current_model().to_string()),
                reasoning_effort: self.reasoning_effort.clone(),
                reasoning_mode: self.reasoning_mode.clone(),
                mock: self.is_mock(),
                api: self.api.clone(),
                tx: self.subtask_tx.clone(),
                budget: self.config.agent.clone(),
                role: agent_def.as_ref().and_then(|def| {
                    let role = def.role.trim();
                    if role.is_empty() {
                        None
                    } else {
                        Some(role.to_string())
                    }
                }),
                tool_policy: agent_def.as_ref().and_then(|def| {
                    if def.tools.is_empty() && def.deny.is_empty() {
                        None
                    } else {
                        Some(crate::agent::subtask::ToolPolicy {
                            allow: def.tools.clone(),
                            deny: def.deny.clone(),
                        })
                    }
                }),
                abort: self.agent_abort.clone(),
            };
            let handle = tokio::spawn(crate::agent::subtask::run(spec));
            if let Some(task) = self.subtasks.iter_mut().find(|task| task.id == id) {
                task.abort = Some(handle.abort_handle());
            }
        }

        if task_ids.is_empty() {
            return self.process_next_tool();
        }
        // Children run in the background: the parent's tool round continues
        // immediately with its own independent work instead of pausing at a
        // barrier. A later batch merges into the same pending set, so the
        // hand-off gate below waits for every launched child.
        if let Some(barrier) = self.task_barrier.as_mut() {
            barrier.task_ids.extend(task_ids);
        } else {
            self.task_barrier = Some(crate::app::state::TaskBarrier {
                session_id: sid,
                task_ids,
            });
        }
        self.set_status(format!(
            "Running {} parallel child agent{}",
            self.task_barrier
                .as_ref()
                .map(|barrier| barrier.task_ids.len())
                .unwrap_or(0),
            if self
                .task_barrier
                .as_ref()
                .is_some_and(|barrier| barrier.task_ids.len() == 1)
            {
                ""
            } else {
                "s"
            }
        ));
        self.touch();
        self.process_next_tool()
    }

    pub fn handle_subtask_event(
        &mut self,
        event: crate::app::state::SubtaskEvent,
    ) -> Option<Action> {
        match event {
            crate::app::state::SubtaskEvent::Progress { id, progress } => {
                use crate::app::state::{SubtaskLogEntry, SubtaskProgress, SubtaskToolStatus};
                if let Some(task) = self.subtasks.iter_mut().find(|task| {
                    task.id == id && task.status == crate::app::state::SubtaskStatus::Running
                }) {
                    match progress {
                        SubtaskProgress::Phase(text) => {
                            task.activity = Some(text.clone());
                            // Native stream adapters may emit ToolCallStarted repeatedly while
                            // assembling one call. Keep that useful live status transient instead
                            // of permanently flooding the inline child-agent history with
                            // hundreds of identical `PHASE Preparing tool: …` rows.
                            if !text.starts_with("Preparing tool:") {
                                task.log.push(SubtaskLogEntry::Phase { text });
                            }
                        }
                        SubtaskProgress::Checklist {
                            done,
                            running,
                            pending,
                        } => {
                            task.activity = Some(if running == 0 && pending == 0 {
                                "Local checklist complete · finalizing report".into()
                            } else {
                                format!(
                                    "Local checklist · {} done · {} running · {} pending",
                                    done, running, pending
                                )
                            });
                            task.log.push(SubtaskLogEntry::Checklist {
                                done,
                                running,
                                pending,
                            });
                        }
                        SubtaskProgress::ToolStarted {
                            name,
                            summary,
                            call,
                        } => {
                            task.activity = Some(format!("Running {}", summary));
                            task.log.push(SubtaskLogEntry::Tool {
                                name,
                                summary,
                                status: SubtaskToolStatus::Running,
                                duration_ms: None,
                                call: Some(call),
                                output: None,
                            });
                        }
                        SubtaskProgress::ToolFinished {
                            name,
                            summary,
                            call,
                            output,
                            ok,
                            duration_ms,
                        } => {
                            task.activity = Some("Reviewing tool result · continuing".into());
                            let finished = SubtaskLogEntry::Tool {
                                name: name.clone(),
                                summary: summary.clone(),
                                status: if ok {
                                    SubtaskToolStatus::Completed
                                } else {
                                    SubtaskToolStatus::Failed
                                },
                                duration_ms: Some(duration_ms),
                                call: Some(call),
                                output: Some(output),
                            };
                            if let Some(entry) = task.log.iter_mut().rev().find(|entry| {
                                matches!(
                                    entry,
                                    SubtaskLogEntry::Tool {
                                        name: entry_name,
                                        summary: entry_summary,
                                        status: SubtaskToolStatus::Running,
                                        ..
                                    } if *entry_name == name && *entry_summary == summary
                                )
                            }) {
                                *entry = finished;
                            } else {
                                task.log.push(finished);
                            }
                        }
                    }
                }
                self.sync_subtask_message(id);
                self.touch();
                None
            }
            crate::app::state::SubtaskEvent::Registered {
                id,
                parent_id,
                call,
                description,
                prompt,
                agent,
                cwd,
            } => {
                if self.subtasks.iter().any(|task| task.id == id) {
                    self.touch();
                    return None;
                }
                use crate::app::state::{Subtask, SubtaskStatus};
                let parent_sid = self
                    .subtasks
                    .iter()
                    .find(|task| task.id == parent_id)
                    .map(|task| task.session_id)
                    .unwrap_or_else(|| self.agent_sid());
                let sibling_index = self
                    .subtasks
                    .iter()
                    .filter(|task| task.parent_id == Some(parent_id))
                    .count();
                let index = self
                    .subtasks
                    .iter()
                    .position(|task| task.id == parent_id)
                    .map(|parent| parent + 1 + sibling_index)
                    .unwrap_or(self.subtasks.len());
                let mut call = call;
                if let Some(args) = call.args.as_object_mut() {
                    args.insert("agent_index".into(), serde_json::json!(sibling_index + 1));
                }
                self.subtasks.insert(
                    index,
                    Subtask {
                        id,
                        session_id: parent_sid,
                        parent_id: Some(parent_id),
                        call,
                        description,
                        todo_index: None,
                        prompt,
                        cwd,
                        status: SubtaskStatus::Running,
                        activity: None,
                        log: Vec::new(),
                        transcript: Vec::new(),
                        output: None,
                        message_index: usize::MAX,
                        started_at: std::time::Instant::now(),
                        duration_ms: None,
                        abort: None,
                        agent,
                    },
                );
                self.touch();
                None
            }
            crate::app::state::SubtaskEvent::Round { id, role, content } => {
                use crate::app::state::SubtaskRound;
                if let Some(task) = self.subtasks.iter_mut().find(|task| task.id == id) {
                    task.transcript.push(SubtaskRound { role, content });
                }
                self.touch();
                None
            }
            crate::app::state::SubtaskEvent::Finished {
                id,
                output,
                duration_ms,
            } => {
                let nested = self
                    .subtasks
                    .iter()
                    .any(|task| task.id == id && task.parent_id.is_some());
                if nested {
                    if let Some(task) = self.subtasks.iter_mut().find(|task| task.id == id) {
                        task.status = if output.is_ok() {
                            crate::app::state::SubtaskStatus::Completed
                        } else {
                            crate::app::state::SubtaskStatus::Failed
                        };
                        task.activity = None;
                        task.duration_ms = Some(duration_ms);
                        task.abort = None;
                        task.output = Some(match &output {
                            Ok(text) => text.clone(),
                            Err(error) => error.clone(),
                        });
                    }
                    self.sync_subtask_message(id);
                    let sid = self
                        .subtasks
                        .iter()
                        .find(|task| task.id == id)
                        .map(|task| task.session_id)
                        .unwrap_or_else(|| self.agent_sid());
                    if let Some(report) = self.child_tracker_report(id) {
                        self.maybe_request_todo_update(sid, std::slice::from_ref(&report));
                    }
                    self.touch();
                    return None;
                }
                if !self.task_barrier.as_ref().is_some_and(|barrier| {
                    barrier.task_ids.contains(&id)
                        && self.subtasks.iter().any(|task| {
                            task.id == id
                                && task.status == crate::app::state::SubtaskStatus::Running
                        })
                }) {
                    return None;
                }
                if let Some(task) = self.subtasks.iter_mut().find(|task| task.id == id) {
                    task.status = if output.is_ok() {
                        crate::app::state::SubtaskStatus::Completed
                    } else {
                        crate::app::state::SubtaskStatus::Failed
                    };
                    task.activity = None;
                    task.duration_ms = Some(duration_ms);
                    task.abort = None;
                    task.output = Some(match &output {
                        Ok(text) => text.clone(),
                        Err(error) => error.clone(),
                    });
                }
                self.sync_subtask_message(id);
                let sid = self
                    .subtasks
                    .iter()
                    .find(|task| task.id == id)
                    .map(|task| task.session_id)
                    .unwrap_or_else(|| self.agent_sid());
                if let Some(report) = self.child_tracker_report(id) {
                    self.maybe_request_todo_update(sid, std::slice::from_ref(&report));
                }
                let ready = self.task_barrier.as_ref().is_some_and(|barrier| {
                    barrier.task_ids.iter().all(|task_id| {
                        self.subtasks.iter().any(|task| {
                            task.id == *task_id
                                && task.status != crate::app::state::SubtaskStatus::Running
                        })
                    })
                });
                if !ready {
                    self.touch();
                    return None;
                }

                // Every child of the deferred batch completed. If the parent
                // round already ended (it had nothing left to do), feed the
                // reports back and ask for the final synthesis; otherwise the
                // report cards are already in the conversation and the parent
                // picks them up mid-round.
                let barrier = self.task_barrier.take().unwrap();
                self.sessions.save();
                self.status = None;
                self.touch();
                if self.agent_session.is_none() || self.agent_session != Some(barrier.session_id) {
                    return self.resume_after_children(barrier);
                }
                None
            }
        }
    }

    /// Compact tracker input for a completed child: display name, report body,
    /// and any `workflow(todo)` activity the child ran (so tasks worked on
    /// inside child agents still reach the checklist).
    fn child_tracker_report(&self, task_id: u64) -> Option<(String, String)> {
        let task = self.subtasks.iter().find(|task| task.id == task_id)?;
        let name = crate::ui::sidepanel::agent_display_name(task);
        let mut body = String::new();
        if let Some(output) = task.output.as_deref() {
            body.push_str(&output.chars().take(4000).collect::<String>());
        }
        let todo_lines: Vec<String> = task
            .transcript
            .iter()
            .filter(|round| {
                round.role == crate::app::state::SubtaskRoundRole::ToolCall
                    && round.content.trim_start().starts_with("todo")
            })
            .map(|round| round.content.trim().to_string())
            .collect();
        if !todo_lines.is_empty() {
            body.push_str("\n[todo activity]\n");
            body.push_str(&todo_lines.join("\n"));
        }
        if body.trim().is_empty() {
            body = "(no report)".to_string();
        }
        Some((name, body))
    }

    /// The deferred children all completed while the parent round was idle:
    /// collect their reports, ask the parent for the final synthesis, and
    /// stream it. The hand-off to the user happens only after this round.
    fn resume_after_children(&mut self, barrier: crate::app::state::TaskBarrier) -> Option<Action> {
        let sid = barrier.session_id;
        let mut lines = Vec::new();
        for task_id in barrier.task_ids {
            if let Some(task) = self.subtasks.iter().find(|task| task.id == task_id) {
                let name = crate::ui::sidepanel::agent_display_name(task);
                let body = match (task.status, task.output.as_deref()) {
                    (crate::app::state::SubtaskStatus::Completed, Some(out)) => {
                        format!("{name} (completed):\n{out}")
                    }
                    (crate::app::state::SubtaskStatus::Failed, Some(err)) => {
                        format!("{name} (failed):\n{err}")
                    }
                    _ => format!("{name}: no report"),
                };
                lines.push(body);
            }
        }
        if lines.is_empty() {
            return self.start_next_queued_round();
        }
        if let Some(session) = self.sessions.by_id_mut(sid) {
            session.push_message(ChatMessage::user(format!(
                "All delegated child agents have completed. Reports:\n\n{}\n\n\
                 Synthesize your final response now, incorporating these results.",
                lines.join("\n\n---\n\n")
            )));
        }
        self.sessions.save();
        self.touch();
        self.begin_stream_for(sid)
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
        let name = result.call.kind().map(|k| k.name()).unwrap_or("");
        let status = if result.is_ok() { "ok" } else { "error" };
        let body = format!(
            "[tool-result:{}] {} ({})\n{}",
            name,
            result.call.summary(),
            status,
            result.text()
        );
        let sid = self.agent_sid();
        if let Some(s) = self.sessions.by_id_mut(sid) {
            let mut msg = ChatMessage::tool(body);
            msg.duration_ms = Some(result.duration_ms);
            msg.local_tool_call = Some(result.call.clone());
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
            Some(ToolKind::Write | ToolKind::Edit | ToolKind::Delete | ToolKind::PowerPoint)
        );
        if !mutates {
            return;
        }
        let path = if kind == Some(ToolKind::PowerPoint) {
            call.args.get("output_path")
        } else {
            call.args.get("path")
        }
        .and_then(|v| v.as_str());
        let Some(path) = path else {
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
        self.sessions.save();
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
            self.notify_desktop(
                "AiTUI — loop finished",
                summary.clone(),
                &[crate::app::notify::DesktopAction::Review],
            );
            self.set_status(format!("Loop finished — {}", summary));
            ToolResult::success(call.clone(), format!("Loop ended: {}", summary), 0)
        } else {
            // `finish` outside a loop is a no-op the model shouldn't have called.
            ToolResult::success(call.clone(), "finish ignored — not in loop mode".into(), 0)
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
        self.set_status(format!(
            "⟳ Loop started (max {} iterations) — Ctrl-C or :loop stop to halt",
            max
        ));
        self.touch();
        self.begin_stream_for(sid)
    }

    /// Called when a turn finished with no tool calls (the model produced a plain
    /// reply). If the session is looping and not yet done, bump the counter and
    /// either stop (hit the cap) or nudge the model into another iteration.
    fn maybe_continue_loop(&mut self, sid: usize) -> Option<Action> {
        let loop_state = self
            .sessions
            .by_id(sid)
            .and_then(|s| s.loop_state.as_ref())?;
        let (goal, stop, iteration, max) = (
            loop_state.goal.clone(),
            loop_state.stop.clone(),
            loop_state.iteration,
            loop_state.max,
        );
        let next = iteration + 1;
        if next >= max {
            if let Some(s) = self.sessions.by_id_mut(sid) {
                s.loop_state = None;
            }
            self.sessions.save();
            self.notify_desktop(
                "AiTUI — loop stopped",
                format!("Reached the {}-iteration cap", max),
                &[crate::app::notify::DesktopAction::Review],
            );
            self.set_status(format!(
                "⟳ Loop stopped after {} iterations (cap). :loop to resume.",
                max
            ));
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

/// The tool result text for a user deny. The reason (when given) is the whole point
/// of the prompt: "Denied by user" alone reads like a transient failure worth
/// retrying, whereas a reason tells the model what to do differently.
fn deny_text(reason: Option<&str>) -> String {
    match reason {
        Some(r) => format!("Denied by user: {}", r),
        None => "Denied by user".to_string(),
    }
}

fn is_parallel_read(call: &ToolCall) -> bool {
    matches!(
        call.kind(),
        Some(
            ToolKind::Read
                | ToolKind::List
                | ToolKind::Search
                | ToolKind::WebSearch
                | ToolKind::WebImages
                | ToolKind::ReverseImage
                | ToolKind::WebFetch
        )
    )
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
    let logo = [" ▄▄ ", "█  █", "█▄▄█"];
    let title = "AiTUI";
    let subtitle = "terminal-native coding agent";
    let cwd = std::env::current_dir()
        .ok()
        .map(|path| crate::render::path::display_path(&path))
        .unwrap_or_else(|| "--".to_string());
    let lines = [
        (logo[0], title.to_string()),
        (logo[1], subtitle.to_string()),
        (logo[2], String::new()),
        ("    ", format!("cwd {}", cwd)),
    ];

    let content_w = lines
        .iter()
        .map(|(left, right)| left.chars().count() + 2 + right.chars().count())
        .max()
        .unwrap_or(1)
        .min(width.saturating_sub(4).max(1));
    let pad = width.saturating_sub(content_w.saturating_add(4)) / 2;
    let mut rows = Vec::new();

    rows.push(RenderedLine::new(Line::raw(""), String::new(), 0));
    for (logo_part, text) in lines {
        let mut spans = Vec::new();
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(
            "▌ ",
            Style::default()
                .bg(theme.subtle_pill)
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!("{}  ", logo_part),
            Style::default()
                .bg(theme.subtle_pill)
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
        let style = if text == title {
            Style::default()
                .bg(theme.subtle_pill)
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().bg(theme.subtle_pill).fg(theme.text)
        };
        spans.push(Span::styled(text.clone(), style));
        let used = logo_part.chars().count() + 2 + text.chars().count();
        spans.push(Span::styled(
            " ".repeat(content_w.saturating_sub(used).saturating_add(2)),
            Style::default().bg(theme.subtle_pill),
        ));
        let plain = format!(
            "{}▌ {}  {}{}",
            " ".repeat(pad),
            logo_part,
            text,
            " ".repeat(content_w.saturating_sub(used).saturating_add(2))
        );
        rows.push(RenderedLine::new(Line::from(spans), plain, 0));
    }
    rows.push(RenderedLine::new(Line::raw(""), String::new(), 0));

    let tips = [
        ("@path", "pull a file into context"),
        ("/", "open commands"),
        ("i ... :w", "compose, then send"),
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
        let plain = format!("{}  {:<9} - {}", " ".repeat(pad), key, desc);
        rows.push(RenderedLine::new(
            Line::from(vec![
                Span::raw(" ".repeat(pad)),
                Span::styled(
                    format!("  {:<9}", key),
                    Style::default()
                        .fg(theme.warning)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" - ", Style::default().fg(theme.accent)),
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
    fn image_prompt_carries_text_model_context_into_latest_request() {
        let mut session = crate::domain::session::Session::new(1);
        session.messages.push(ChatMessage::user(
            "Research a historically accurate observatory.",
        ));
        session.messages.push(ChatMessage::assistant(
            "Use a brass refractor, a rotating copper dome, and red night lamps.",
        ));
        session
            .messages
            .push(ChatMessage::user("Now create an exterior view at dusk."));

        let prompt = build_image_prompt(&session);
        assert!(prompt.contains("brass refractor"));
        assert!(prompt.contains("rotating copper dome"));
        assert!(prompt.contains("exterior view at dusk"));
        assert!(prompt.find("brass refractor") < prompt.find("exterior view at dusk"));
    }

    #[test]
    fn image_prompt_ignores_previous_generated_image_receipts() {
        let mut session = crate::domain::session::Session::new(1);
        session.messages.push(ChatMessage::user("Draw a fox."));
        session
            .messages
            .push(ChatMessage::assistant("Image saved → `old.png`"));
        session
            .messages
            .push(ChatMessage::user("Make the fur silver."));

        let prompt = build_image_prompt(&session);
        assert!(!prompt.contains("old.png"));
        assert!(prompt.contains("Draw a fox"));
        assert!(prompt.contains("fur silver"));
    }

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
    fn parallel_read_classifier_keeps_mutations_as_barriers() {
        let call = |action| ToolCall {
            name: "file_management".into(),
            args: serde_json::json!({"action": action, "path": "."}),
            id: None,
        };
        assert!(is_parallel_read(&call("read")));
        assert!(is_parallel_read(&call("list")));
        assert!(is_parallel_read(&call("search")));
        assert!(!is_parallel_read(&call("write")));
        assert!(!is_parallel_read(&call("edit")));
        assert!(!is_parallel_read(&call("delete")));
    }

    #[test]
    fn loop_guard_trips_at_max_before_incrementing() {
        assert!(!agent_loop_guard_reached(0));
        assert!(!agent_loop_guard_reached(MAX_AGENT_ITERATIONS - 1));
        assert!(agent_loop_guard_reached(MAX_AGENT_ITERATIONS));
    }

    #[test]
    fn todo_update_applies_when_signature_matches_and_drops_stale() {
        let mut app = crate::app::state::App::new(crate::config::Config::default()).unwrap();
        let sid = app.sessions.active_id();
        app.sessions.active_mut().todos.clear();
        app.sessions
            .active_mut()
            .messages
            .push(ChatMessage::user("do the thing"));
        app.sessions
            .active_mut()
            .messages
            .push(ChatMessage::assistant("Started the work"));
        let (user, reply) = app.sessions.active().latest_completed_turn().unwrap();
        let signature = crate::app::todo_tracker::update_signature(
            &user,
            &reply,
            &app.sessions.active().todos,
            &[],
        );
        app.todo_inflight.insert((sid, signature), signature);
        let update = crate::app::state::TodoUpdate {
            items: vec![crate::app::state::TodoItem {
                text: "Fix build".into(),
                status: crate::app::state::TodoStatus::Done,
                percent: Some(100),
            }],
            overall_percent: Some(100),
        };
        app.apply_todo_update(sid, signature, Ok(update));
        assert_eq!(app.sessions.active().todos.len(), 1);
        assert_eq!(app.sessions.active().todos[0].percent, Some(100));
        assert_eq!(app.sessions.active().todo_overall_percent, Some(100));

        // A stale result (signature no longer matches) must not clobber state.
        app.apply_todo_update(
            sid,
            signature.wrapping_add(1),
            Ok(crate::app::state::TodoUpdate::default()),
        );
        assert_eq!(app.sessions.active().todos.len(), 1);
    }

    #[test]
    fn todo_update_skips_non_agent_sessions_on_request() {
        let mut app = crate::app::state::App::new(crate::config::Config::default()).unwrap();
        let sid = app.sessions.active_id();
        app.sessions.active_mut().agent_mode = false;
        app.sessions
            .active_mut()
            .messages
            .push(ChatMessage::user("hi"));
        app.sessions
            .active_mut()
            .messages
            .push(ChatMessage::assistant("hello"));
        app.maybe_request_todo_update(sid, &[]);
        assert!(
            app.todo_inflight.is_empty(),
            "no tracker call for non-agent sessions"
        );
    }
}
