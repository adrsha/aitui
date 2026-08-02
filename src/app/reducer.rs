//! The reducer: applies one `Action` to the `App`, optionally returning a
//! follow-up action. All mutation funnels through here so behaviour is easy to
//! trace and test.

use std::path::PathBuf;

use crate::agent::{Permission, PermissionDecision, ToolKind};
use crate::app::action::{Action, Dir};
use crate::app::overlay::{
    BrowsePurpose, CommandLine, FileBrowser, Overlay, Picker, PickerKind, Settings, SettingsRow,
};
use crate::app::state::{App, MouseSelection, PendingExternal};
use crate::domain::session::LoopState;
use crate::input::vim::VimMode;

fn access_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn clean_session_title(title: &str) -> String {
    title
        .trim()
        .trim_matches(['"', '\'', '.', ':'])
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(48)
        .collect::<String>()
        .trim()
        .to_string()
}

impl App {
    fn chat_h(&self) -> usize {
        self.layout.chat.height.saturating_sub(2) as usize
    }

    fn open_subtask_detail(&mut self, task_id: u64) {
        if self.subtasks.iter().any(|task| task.id == task_id) {
            self.selected_subtask = Some(task_id);
            self.overlay = Overlay::SubtaskDetail { task_id, scroll: 0 };
            self.touch();
        }
    }

    fn cycle_subtask(&mut self, delta: isize) {
        let ids: Vec<u64> = self
            .subtasks
            .iter()
            .filter(|task| task.session_id == self.sessions.active_id())
            .map(|task| task.id)
            .collect();
        if ids.is_empty() {
            self.selected_subtask = None;
            return;
        }
        let current = self
            .selected_subtask
            .and_then(|id| ids.iter().position(|candidate| *candidate == id))
            .unwrap_or(ids.len() - 1);
        let next = (current as isize + delta).rem_euclid(ids.len() as isize) as usize;
        let task_id = ids[next];
        self.selected_subtask = Some(task_id);
        self.overlay = Overlay::SubtaskDetail { task_id, scroll: 0 };
        self.touch();
    }

    fn inspect_stream_tools(&mut self, sid: usize) {
        // Runs on both stream channels: an endpoint may emit the fence through
        // visible content or interleave it with its separate reasoning channel, and
        // either delta can be the one that completes a call. Only visible fences are
        // acted on — see `should_cut_stream`.
        self.speculate_read_tools(sid);
        if self.cut_stream.is_none() && self.should_cut_stream(sid) {
            if let Some(session) = self.sessions.by_id_mut(sid) {
                session.finalize_assistant_stream();
            }
            self.streams.retain(|stream| stream.session_id != sid);
            self.sessions.save();
            self.cut_stream = Some(sid);
        }
    }

    /// Whether the permission prompt is currently collecting a deny reason.
    #[allow(clippy::wrong_self_convention)]
    fn permission_reason_open(&self) -> bool {
        matches!(&self.overlay, Overlay::Permission(r) if r.writing_reason())
    }

    /// Open the reason box for a chosen deny. The deny applies on the next Enter,
    /// with whatever reason has been typed (empty is fine — it's optional).
    fn begin_deny(&mut self, perm: Permission) -> Option<Action> {
        if let Overlay::Permission(r) = &mut self.overlay {
            r.begin_deny(perm);
            self.set_status("Deny — type an optional reason for the model · ⏎ deny · Esc back");
            self.touch();
        }
        None
    }

    fn snapshot_input(&mut self) {
        self.input_undo.push(self.input.clone());
        if self.input_undo.len() > 200 {
            self.input_undo.remove(0);
        }
        self.input_redo.clear();
    }

    fn undo_input(&mut self) {
        if let Some(prev) = self.input_undo.pop() {
            self.input_redo.push(self.input.clone());
            self.input = prev;
            self.input.end_visual();
            self.update_mention();
        }
    }

    fn redo_input(&mut self) {
        if let Some(next) = self.input_redo.pop() {
            self.input_undo.push(self.input.clone());
            self.input = next;
            self.input.end_visual();
            self.update_mention();
        }
    }

    fn motion_target(&self, dir: Dir) -> ((usize, usize), bool) {
        let mut probe = self.input.clone();
        match dir {
            Dir::Left => probe.left(),
            Dir::Right => probe.right(),
            Dir::Up => probe.up_normal(),
            Dir::Down => probe.down_normal(),
            Dir::WordForward => probe.word_forward(),
            Dir::WordBackward => probe.word_backward(),
            Dir::WordEnd => probe.word_end(),
        }
        let inclusive = matches!(dir, Dir::WordEnd | Dir::Left | Dir::Right);
        (probe.cursor(), inclusive)
    }

    fn yank_to(&mut self, dir: Dir) {
        let start = self.input.cursor();
        let (end, inclusive) = self.motion_target(dir);
        let text = self.input.range_text(start, end, inclusive);
        self.set_yank(text);
        self.vim = VimMode::Normal;
    }

    fn delete_to(&mut self, dir: Dir, insert_after: bool) {
        let start = self.input.cursor();
        let (end, inclusive) = self.motion_target(dir);
        let can_delete =
            start != end || (inclusive && self.input.current_line().chars().nth(start.1).is_some());
        if can_delete {
            self.snapshot_input();
            let text = self.input.delete_range(start, end, inclusive);
            self.set_yank(text);
            self.update_mention();
        }
        self.vim = if insert_after {
            VimMode::Insert
        } else {
            VimMode::Normal
        };
        if !insert_after {
            self.input.clamp_normal();
        }
    }

    pub fn apply(&mut self, action: Action) -> Option<Action> {
        if !matches!(&action, Action::Move(Dir::Up | Dir::Down)) {
            self.input.reset_visual_goal();
        }
        // TODO(audit): break this reducer into domain-specific reducers once new
        // action families are added; this match is already the central change bottleneck.
        match action {
            Action::Quit => {
                // Persist the live composer draft with the session before exiting.
                self.stash_draft();
                self.sessions.save();
                self.should_quit = true;
            }
            Action::DesktopNotification(response) => {
                if response.generation != self.notification_generation {
                    self.set_status("That notification is no longer current");
                } else {
                    match response.action {
                        crate::app::notify::DesktopAction::Review => {
                            self.set_status("Review the pending request in AiTUI");
                        }
                        crate::app::notify::DesktopAction::AllowOnce
                            if matches!(self.overlay, Overlay::Permission(_)) =>
                        {
                            return self.resolve_permission(Permission::Allow, None);
                        }
                        crate::app::notify::DesktopAction::DenyOnce
                            if matches!(self.overlay, Overlay::Permission(_)) =>
                        {
                            return self.resolve_permission(Permission::Deny, None);
                        }
                        crate::app::notify::DesktopAction::AcceptPlan
                            if matches!(self.overlay, Overlay::Plan(_)) =>
                        {
                            return self.resolve_plan(true);
                        }
                        crate::app::notify::DesktopAction::RejectPlan
                            if matches!(self.overlay, Overlay::Plan(_)) =>
                        {
                            return self.resolve_plan(false);
                        }
                        _ => self.set_status("That notification is no longer current"),
                    }
                }
            }
            Action::FocusGained => self.focused = true,
            Action::FocusLost => self.focused = false,
            Action::Resize => {}
            Action::ToggleHelp => {
                if self.show_help {
                    self.show_help = false;
                    self.help_detail = None;
                } else {
                    self.show_help = true;
                    self.help_selected = 0;
                    self.help_scroll = 0;
                }
            }
            Action::HelpBack => {
                if self.help_detail.is_some() {
                    self.help_detail = None;
                } else if self.show_help {
                    self.show_help = false;
                }
            }
            Action::HelpUp => {
                if self.help_detail.is_some() {
                    self.help_scroll = self.help_scroll.saturating_sub(1);
                } else if self.help_selected > 0 {
                    self.help_selected -= 1;
                    if self.help_selected < self.help_scroll {
                        self.help_scroll = self.help_selected;
                    }
                }
            }
            Action::HelpDown => {
                if self.help_detail.is_some() {
                    self.help_scroll += 1;
                } else {
                    let max = crate::app::commands::HELP_ENTRIES.len().saturating_sub(1);
                    if self.help_selected < max {
                        self.help_selected += 1;
                    }
                    if self.help_selected >= self.help_scroll + 20 {
                        self.help_scroll += 1;
                    }
                }
            }
            Action::HelpPageUp => {
                if self.help_detail.is_some() {
                    self.help_scroll = self.help_scroll.saturating_sub(10);
                } else {
                    self.help_scroll = self.help_scroll.saturating_sub(10);
                    self.help_selected = self.help_selected.saturating_sub(10);
                }
            }
            Action::HelpPageDown => {
                let max = crate::app::commands::HELP_ENTRIES.len().saturating_sub(1);
                if self.help_detail.is_some() {
                    self.help_scroll += 10;
                } else {
                    self.help_scroll += 10;
                    self.help_selected = (self.help_selected + 10).min(max);
                }
            }
            Action::HelpSelect => {
                if self.help_detail.is_some() {
                    self.help_detail = None;
                    self.help_scroll = 0;
                } else if self.show_help {
                    self.help_detail = Some(self.help_selected);
                    self.help_scroll = 0;
                }
            }

            // ── Modes ───────────────────────────────────────────────────────
            Action::EnterInsert => {
                self.vim = VimMode::Insert;
                self.status = None;
                self.last_insert = None;
            }
            Action::EnterNormal => {
                self.vim = VimMode::Normal;
                self.command.clear();
                self.input.end_visual();
                self.input.clamp_normal();
                self.mention.reset();
                self.last_insert = None;
            }
            Action::EnterVisual => {
                self.vim = VimMode::Visual;
                self.input.begin_visual();
            }
            Action::VisualYank => {
                let sel = self.input.selection_text();
                self.set_yank(sel);
                self.input.end_visual();
                self.vim = VimMode::Normal;
                self.input.clamp_normal();
            }
            Action::VisualDelete => {
                let sel = self.input.delete_selection();
                self.set_yank(sel);
                self.vim = VimMode::Normal;
                self.input.clamp_normal();
                self.update_mention();
            }
            Action::VisualChange => {
                let sel = self.input.delete_selection();
                self.set_yank(sel);
                self.vim = VimMode::Insert;
                self.update_mention();
            }
            Action::EnterVisualLine => {
                self.vim = VimMode::Visual;
                self.input.begin_visual_line();
            }
            Action::EnterOperator(op) => self.vim = VimMode::Operator(op),

            // ── Input editing ───────────────────────────────────────────────
            Action::InsertChar(c) => {
                self.snapshot_input();
                self.input.insert_char(c);
                self.update_mention();
                self.last_insert = Some(c); // track for the jk-style chord
            }
            Action::Newline => {
                self.snapshot_input();
                self.mention.reset();
                self.input.insert_newline();
                self.last_insert = None;
            }
            Action::Backspace => {
                self.snapshot_input();
                self.input.backspace();
                self.update_mention();
                self.last_insert = None;
            }
            Action::DeleteWordBack => {
                self.snapshot_input();
                self.input.delete_word_back();
                self.update_mention();
                self.last_insert = None;
            }
            Action::DeleteWordForward => {
                self.snapshot_input();
                self.input.delete_word_forward();
                self.update_mention();
            }
            Action::DeleteAt => {
                self.snapshot_input();
                let deleted = self.input.delete_at();
                if let Some(c) = deleted {
                    self.set_yank(c.to_string());
                }
                self.update_mention();
            }
            Action::DeleteLine => {
                self.snapshot_input();
                let line = self.input.yank_line();
                self.input.delete_line();
                self.set_yank(line);
                self.update_mention();
            }
            Action::ChangeLine => {
                self.snapshot_input();
                let line = self.input.change_line();
                self.set_yank(line);
                self.vim = VimMode::Insert;
                self.update_mention();
            }
            Action::DeleteTo(dir) => self.delete_to(dir, false),
            Action::ChangeTo(dir) => self.delete_to(dir, true),
            Action::DeleteToLineEnd => {
                self.snapshot_input();
                let text = self.input.delete_to_line_end();
                self.set_yank(text);
                self.input.clamp_normal();
                self.update_mention();
            }
            Action::ChangeToLineEnd => {
                self.snapshot_input();
                let text = self.input.change_to_line_end();
                self.set_yank(text);
                self.vim = VimMode::Insert;
                self.update_mention();
            }
            Action::YankToLineEnd => {
                let start = self.input.cursor();
                let end = (self.input.row, self.input.current_line().chars().count());
                let text = self.input.range_text(start, end, false);
                self.set_yank(text);
                self.vim = VimMode::Normal;
            }
            Action::YankTo(dir) => self.yank_to(dir),
            Action::OpenLineBelow => {
                self.snapshot_input();
                self.input.open_line_below();
                self.vim = VimMode::Insert;
                self.update_mention();
            }
            Action::OpenLineAbove => {
                self.snapshot_input();
                self.input.open_line_above();
                self.vim = VimMode::Insert;
                self.update_mention();
            }
            Action::UndoInput => self.undo_input(),
            Action::RedoInput => self.redo_input(),
            Action::YankLine => {
                let line = self.input.yank_line();
                self.set_yank(line);
            }
            Action::Paste => {
                if let Some(t) = self.yank.clone() {
                    self.snapshot_input();
                    self.input.paste(&t);
                    self.update_mention();
                }
            }
            Action::PasteText(t) => self.smart_paste(t),
            Action::Move(dir) => {
                let normal = self.vim != VimMode::Insert;
                match dir {
                    Dir::Left => self.input.left(),
                    Dir::Right => self.input.right(),
                    Dir::Up if normal => self.input.up_normal(),
                    Dir::Down if normal => self.input.down_normal(),
                    Dir::Up => self.input.up(),
                    Dir::Down => self.input.down(),
                    Dir::WordForward => self.input.word_forward(),
                    Dir::WordBackward => self.input.word_backward(),
                    Dir::WordEnd => self.input.word_end(),
                }
                if normal {
                    self.input.clamp_normal();
                }
            }
            Action::LineStart => self.input.line_start(),
            Action::FirstNonBlank => self.input.first_nonblank(),
            Action::LineEnd => self.input.line_end(),

            // ── Command palette ─────────────────────────────────────────────
            Action::RunCommand(cmd) => return self.run_command(&cmd),
            Action::InputHistoryPrev => self.input_history_prev(),
            Action::InputHistoryNext => self.input_history_next(),

            // ── Submission / streaming ──────────────────────────────────────
            Action::Submit => return self.submit(),
            Action::RetryLast => return self.retry_last(),
            Action::EditLast => self.edit_last(),
            Action::CopyLastReply => self.copy_last_reply(),
            Action::CopyLastCode => self.copy_last_code(),
            Action::AttachStream(sid, rx) => {
                self.streams.push(crate::app::state::StreamHandle {
                    session_id: sid,
                    rx,
                    cold_retries: 0,
                });
            }
            Action::RetryStream(sid, cold_retries) => {
                return self.retry_cold_stream(sid, cold_retries)
            }
            Action::AttachRetriedStream(sid, rx, cold_retries) => {
                self.streams.push(crate::app::state::StreamHandle {
                    session_id: sid,
                    rx,
                    cold_retries,
                });
            }
            Action::StreamToken(sid, t) => {
                if let Some(s) = self.sessions.by_id_mut(sid) {
                    s.append_stream_token(&t);
                }
                self.inspect_stream_tools(sid);
                self.touch();
            }
            Action::StreamReasoning(sid, t) => {
                if let Some(s) = self.sessions.by_id_mut(sid) {
                    s.append_reasoning(&t);
                }
                self.inspect_stream_tools(sid);
                self.touch();
            }
            Action::StreamUsage(sid, usage) => {
                if self.sessions.has_id(sid) {
                    self.session_usage.insert(sid, usage);
                }
            }
            Action::StreamToolCallStarted(sid, name) => {
                if let Some(s) = self.sessions.by_id_mut(sid) {
                    s.mark_stream_progress();
                }
                match &mut self.preparing_tool {
                    Some((prep_sid, prep_name, _)) if *prep_sid == sid => *prep_name = name,
                    _ => {
                        self.preparing_tool = Some((sid, name, std::time::Instant::now()));
                    }
                }
                self.touch();
            }
            Action::StreamImageReady(sid, path) => {
                if self.sessions.active_id() == sid {
                    self.pending_image = Some(path);
                }
                self.touch();
            }
            Action::StreamImageError(sid, error) => {
                if let Some(session) = self.sessions.by_id_mut(sid) {
                    session.finalize_assistant_stream();
                }
                self.streams.retain(|handle| handle.session_id != sid);
                self.set_status(format!("Image request failed: {}", error));
                self.sessions.save();
                self.touch();
            }
            Action::StreamDone(sid) => {
                if let Some(s) = self.sessions.by_id_mut(sid) {
                    s.finalize_assistant_stream();
                }
                self.streams.retain(|h| h.session_id != sid);
                if self
                    .preparing_tool
                    .as_ref()
                    .is_some_and(|(prep_sid, _, _)| *prep_sid == sid)
                {
                    self.preparing_tool = None;
                }
                // StreamToken may have already cut this same stream early (tool call
                // detected) and queued a round via `cut_stream`. We're starting the
                // round right here, so clear that flag or main.rs would start it a
                // second time on the next pass, re-running every tool call.
                if self.cut_stream == Some(sid) {
                    self.cut_stream = None;
                }
                self.status = None;
                self.sessions.save();
                self.touch();
                return self.maybe_start_agent_round(sid);
            }
            Action::StartAgentRound(sid) => return self.maybe_start_agent_round(sid),
            Action::StreamError(sid, e) => {
                if let Some(s) = self.sessions.by_id_mut(sid) {
                    s.finalize_assistant_stream();
                }
                self.streams.retain(|h| h.session_id != sid);
                if self
                    .preparing_tool
                    .as_ref()
                    .is_some_and(|(prep_sid, _, _)| *prep_sid == sid)
                {
                    self.preparing_tool = None;
                }
                // If the endpoint rejected the native `tools` field, fall back to
                // fenced parsing so the app keeps working (the user resends).
                if looks_like_base_url_error(&e) {
                    // No / invalid endpoint URL — prompt for the URL + key.
                    self.set_status("No valid API endpoint — enter your URL and key.");
                    let ep = self.config.api.endpoint.clone();
                    let key = self.config.api.api_key.clone();
                    self.overlay = Overlay::ApiSetup(crate::app::overlay::ApiSetup::new(ep, key));
                } else if looks_like_context_overflow(&e) {
                    // Safety net: the proactive window still overflowed the model's
                    // real context. Drop the oldest turns and resend automatically.
                    // compact_history shrinks each time and returns false once only
                    // the current turn is left, so this can't loop forever.
                    let compacted = self
                        .sessions
                        .by_id_mut(sid)
                        .map(|s| s.compact_history())
                        .unwrap_or(false);
                    if compacted {
                        self.set_status("Context full — summarized older messages and retrying…");
                        self.sessions.save();
                        self.touch();
                        return self.begin_stream_for(sid);
                    }
                    self.set_status(
                        "Context full and this turn alone is too large — shorten it or :clear.",
                    );
                } else {
                    self.set_status(format!("Stream error: {}", e));
                }
                self.sessions.save();
                self.touch();
            }
            Action::CancelStream => {
                // Cancel only the active session's stream. Also halt an autonomous
                // loop on it — Ctrl-C is the user's "stop everything" for this session.
                let active = self.sessions.active_id();
                self.streams.retain(|h| h.session_id != active);
                if self
                    .preparing_tool
                    .as_ref()
                    .is_some_and(|(prep_sid, _, _)| *prep_sid == active)
                {
                    self.preparing_tool = None;
                }
                self.sessions.active_mut().finalize_assistant_stream();
                self.stop_loop();
                self.sessions.save();
                self.touch();
            }

            // ── Transcript scrolling ────────────────────────────────────────
            Action::ChatTop => {
                self.chat.top(self.chat_h());
            }
            Action::ChatBottom => {
                self.chat.bottom(self.chat_h());
            }
            Action::ChatPageDown => {
                self.chat.page_down(self.chat_h());
            }
            Action::ChatPageUp => {
                self.chat.page_up(self.chat_h());
            }
            Action::ChatHalfDown => {
                self.chat.half_page_down(self.chat_h());
            }
            Action::ChatHalfUp => {
                self.chat.half_page_up(self.chat_h());
            }
            Action::ChatScroll(d) => {
                self.chat.scroll_by(d, self.chat_h());
            }
            Action::SidebarTaskScroll(delta) => {
                if delta < 0 {
                    self.sidebar_task_scroll = self
                        .sidebar_task_scroll
                        .saturating_sub(delta.unsigned_abs() as usize);
                } else {
                    self.sidebar_task_scroll =
                        self.sidebar_task_scroll.saturating_add(delta as usize);
                }
                self.touch();
            }
            Action::ToggleOutput => {
                // The status bar shows an independent `output` chip; don't clobber
                // the free-text status (e.g. "Generating…") with a redundant line.
                self.show_output = !self.show_output;
                self.chat.stick_bottom = true;
                self.touch();
            }
            Action::ChatClick(col, row) => {
                if !matches!(self.overlay, Overlay::None) {
                    return None;
                }
                if self.layout.prompt_goto.is_some_and(|goto| {
                    col >= goto.area.x
                        && col < goto.area.x + goto.area.width
                        && row >= goto.area.y
                        && row < goto.area.y + goto.area.height
                }) {
                    let user_idx = if self.chat.stick_bottom {
                        self.sessions
                            .active()
                            .messages
                            .iter()
                            .rposition(|m| m.role == "user")
                    } else {
                        self.chat.viewport_message().and_then(|mi| {
                            let msgs = &self.sessions.active().messages;
                            let end = mi.min(msgs.len().saturating_sub(1));
                            msgs[..=end].iter().rposition(|m| m.role == "user")
                        })
                    };
                    if let Some(user_idx) = user_idx {
                        let doc = self.chat.doc();
                        if let Some(row) = doc
                            .iter()
                            .position(|r| r.msg == user_idx && r.role_start.is_some())
                        {
                            let h = self.chat_h();
                            self.chat.scroll = row.min(doc.len().saturating_sub(h));
                            self.chat.stick_bottom = false;
                            self.chat.focus_msg = None;
                        }
                    }
                    self.touch();
                    return None;
                }
                if self.layout.prompt.is_some_and(|prompt| {
                    col >= prompt.area.x
                        && col < prompt.area.x + prompt.area.width
                        && row >= prompt.area.y
                        && row < prompt.area.y + prompt.area.height
                }) {
                    self.show_last_prompt = !self.show_last_prompt;
                    self.touch();
                    return None;
                }
                if self.layout.access.is_some_and(|access| {
                    col >= access.area.x
                        && col < access.area.x + access.area.width
                        && row >= access.area.y
                        && row < access.area.y + access.area.height
                }) {
                    return Some(Action::OpenAccessManager);
                }
                if let Some(entry) = self.layout.access_rows.iter().copied().find(|hitbox| {
                    col >= hitbox.area.x
                        && col < hitbox.area.x + hitbox.area.width
                        && row >= hitbox.area.y
                        && row < hitbox.area.y + hitbox.area.height
                }) {
                    if entry.index == usize::MAX {
                        return Some(Action::OpenAccessManager);
                    }
                    return Some(Action::EditAccessEntry(entry.index));
                }
                if let Some(task_id) = self.layout.panel_agents.iter().find_map(|hitbox| {
                    (col >= hitbox.area.x
                        && col < hitbox.area.x + hitbox.area.width
                        && row >= hitbox.area.y
                        && row < hitbox.area.y + hitbox.area.height)
                        .then_some(hitbox.task_id)
                }) {
                    self.open_subtask_detail(task_id);
                    return None;
                }
                if let Some(task_id) = self
                    .layout
                    .sidebar_agents
                    .iter()
                    .chain(self.layout.panel_agents.iter())
                    .find_map(|hitbox| {
                        (col >= hitbox.area.x
                            && col < hitbox.area.x + hitbox.area.width
                            && row >= hitbox.area.y
                            && row < hitbox.area.y + hitbox.area.height)
                            .then_some(hitbox.task_id)
                    })
                {
                    return Some(Action::InspectSubtask(task_id));
                }
                if let Some(tab) = self.layout.session_tabs.iter().copied().find(|tab| {
                    col >= tab.area.x
                        && col < tab.area.x + tab.area.width
                        && row >= tab.area.y
                        && row < tab.area.y + tab.area.height
                }) {
                    if tab.session_idx != self.sessions.active_idx() {
                        return Some(Action::SelectSession(tab.session_idx));
                    }
                    return None;
                }
                // A click on the "↓ N below" jump pill snaps back to the live tail.
                if let Some((pill, _)) = self.jump_pill() {
                    if col >= pill.x
                        && col < pill.x + pill.width
                        && row >= pill.y
                        && row < pill.y + pill.height
                    {
                        self.show_last_prompt = false;
                        self.chat.bottom(self.chat_h());
                        self.touch();
                        return None;
                    }
                }
                // Map the click to a transcript row and toggle the collapsible
                // tool output whose header sits there. Ignore clicks outside the
                // chat pane or on non-header rows.
                let area = self.layout.chat;
                let scrollbar_x = area.x + area.width;
                let on_scrollbar = matches!(self.overlay, Overlay::None)
                    && col >= scrollbar_x
                    && col < scrollbar_x.saturating_add(2)
                    && row >= area.y
                    && row < area.y + area.height;
                if on_scrollbar {
                    self.chat.scroll_to_track(
                        (row - area.y) as usize,
                        area.height as usize,
                        self.chat_h(),
                    );
                    self.touch();
                    return None;
                }
                let inside = col >= area.x
                    && col < area.x + area.width
                    && row >= area.y
                    && row < area.y + area.height;
                if inside {
                    // Entered a child agent: the first doc row is the breadcrumb;
                    // clicking it (or anywhere on it) navigates back to the root.
                    if self.view_node.is_some() {
                        let doc_row = self.chat.scroll + (row - area.y) as usize;
                        if doc_row == 0 {
                            return Some(Action::NavigateBack);
                        }
                    }
                    let vp_row = (row - area.y) as usize;
                    // Record the click position for drag selection.
                    self.mouse_select = Some(MouseSelection {
                        anchor_row: row,
                        drag_row: row,
                        active: false,
                    });
                    // Toggle collapsible blocks: match the header directly, or
                    // walk backwards to find the enclosing toggle when clicking
                    // on a content row.
                    if let Some(key) = self
                        .chat
                        .toggle_at_viewport_row(vp_row)
                        .or_else(|| self.chat.enclosing_toggle(vp_row))
                    {
                        // Preserve the reading position: only reveal/stick-to-bottom
                        // if the view was already at the bottom. When browsing up,
                        // toggling a block leaves the scroll where it is.
                        let at_bottom = self.chat.stick_bottom;
                        self.chat.toggle_block(key);
                        if !at_bottom {
                            self.chat.focus_msg = None;
                            self.chat.stick_bottom = false;
                        }
                        self.touch();
                    }
                }
            }
            Action::ChatDrag(col, row) => {
                if let Some(sel) = &mut self.mouse_select {
                    let area = self.layout.chat;
                    let inside = col >= area.x
                        && col < area.x + area.width
                        && row >= area.y
                        && row < area.y + area.height;
                    if inside {
                        sel.active = true;
                        sel.drag_row = row;
                    }
                }
            }
            Action::ChatRelease => {
                if let Some(sel) = self.mouse_select.take() {
                    if sel.active && self.config.ui.auto_copy_selection {
                        let area = self.layout.chat;
                        let r0 =
                            (sel.anchor_row.saturating_sub(area.y)) as usize + self.chat.scroll;
                        let r1 = (sel.drag_row.saturating_sub(area.y)) as usize + self.chat.scroll;
                        let start = r0.min(r1);
                        let end = r1.max(r0);
                        let text: Vec<&str> = self.chat.doc()[start..=end]
                            .iter()
                            .map(|r| r.plain.as_str())
                            .collect();
                        if !text.is_empty() {
                            self.pending_clipboard = Some(text.join("\n"));
                        }
                    }
                }
            }
            Action::DismissNotice => {
                if matches!(
                    self.overlay,
                    Overlay::Notice { .. } | Overlay::SubtaskDetail { .. }
                ) {
                    self.overlay = Overlay::None;
                }
            }
            Action::PrevSubtask => self.cycle_subtask(-1),
            Action::NextSubtask => self.cycle_subtask(1),
            Action::SubtaskDetailUp => {
                if let Overlay::SubtaskDetail { scroll, .. } = &mut self.overlay {
                    *scroll = scroll.saturating_sub(5);
                }
            }
            Action::SubtaskDetailDown => {
                if let Overlay::SubtaskDetail { scroll, .. } = &mut self.overlay {
                    *scroll = scroll.saturating_add(5);
                }
            }
            Action::InspectSubtask(task_id) => {
                // Enter the agent: chat + sidebar switch to its own content.
                // Clicking the already-entered agent collapses back to the root.
                if self.view_node == Some(task_id) {
                    self.view_node = None;
                    self.selected_subtask = None;
                    self.touch();
                } else {
                    self.view_node = Some(task_id);
                    self.selected_subtask = Some(task_id);
                    self.touch();
                }
            }
            Action::NavigateBack => {
                // Exit the currently viewed agent, going up to its parent (or
                // the root chat when the agent is a direct child of the root).
                if let Some(node) = self.view_node {
                    let parent = self
                        .subtasks
                        .iter()
                        .find(|task| task.id == node)
                        .and_then(|task| task.parent_id);
                    self.view_node = parent;
                    self.selected_subtask = parent;
                    self.touch();
                }
            }

            // ── External programs (editor / shell) ──────────────────────────
            Action::OpenEditor => {
                self.pending_external =
                    Some(PendingExternal::EditorText(self.conversation_markdown()));
            }
            Action::OpenEditPicker => {
                // Toggle: a second press closes the browser.
                if self.overlay.is_browser() {
                    self.overlay = Overlay::None;
                } else {
                    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                    let preselect = self.edited_files.iter().map(PathBuf::from).collect();
                    self.overlay =
                        Overlay::Browser(FileBrowser::open(cwd, BrowsePurpose::Edit, preselect));
                }
            }
            Action::OpenFilesInEditor(paths) => {
                if !paths.is_empty() {
                    self.pending_external = Some(PendingExternal::EditorFiles(paths));
                }
            }
            Action::OpenShell => {
                self.pending_external = Some(PendingExternal::Shell);
            }

            // ── File browser navigation ─────────────────────────────────────
            Action::BrowserDown => {
                if let Overlay::Browser(b) = &mut self.overlay {
                    b.down()
                }
            }
            Action::BrowserUp => {
                if let Overlay::Browser(b) = &mut self.overlay {
                    b.up()
                }
            }
            Action::BrowserParent => {
                if let Overlay::Browser(b) = &mut self.overlay {
                    b.parent()
                }
            }
            Action::BrowserSelect => {
                if let Overlay::Browser(b) = &mut self.overlay {
                    b.toggle_select()
                }
            }
            Action::BrowserClose => self.overlay = Overlay::None,
            Action::BrowserOpen => return self.browser_open(),

            // ── Sessions ────────────────────────────────────────────────────
            Action::SyncSessions => {
                let old_ids: Vec<usize> = self.sessions.all().iter().map(|s| s.id).collect();
                self.remote_running_sessions = self.sessions.remote_running_sessions();
                if self.sessions.sync_from_disk() {
                    let live_ids: std::collections::HashSet<usize> =
                        self.sessions.all().iter().map(|s| s.id).collect();
                    self.streams
                        .retain(|stream| live_ids.contains(&stream.session_id));
                    self.session_permissions
                        .retain(|id, _| live_ids.contains(id));
                    self.session_usage.retain(|id, _| live_ids.contains(id));
                    if old_ids.iter().any(|id| !live_ids.contains(id)) {
                        self.load_active_permissions();
                        self.load_active_draft();
                    }
                    self.chat.stick_bottom = true;
                    self.touch();
                }
                let session_items = self.session_items();
                if let Overlay::Picker(picker) = &mut self.overlay {
                    if picker.kind == PickerKind::Session {
                        picker.items = session_items;
                        picker.refilter();
                    }
                }
            }
            Action::NewSession => {
                self.stash_active_permissions();
                self.sessions.new_session();
                self.load_active_permissions();
                self.sessions.active_mut().agent_mode = true;
                self.sessions.save();
                self.chat.stick_bottom = true;
                self.touch();
            }
            Action::ForkSession => {
                self.stash_active_permissions();
                self.sessions.fork_active();
                self.load_active_permissions();
                self.set_status(format!("Forked → {}", self.sessions.active().name));
                self.sessions.save();
                self.chat.stick_bottom = true;
                self.touch();
            }
            Action::DeleteSession => {
                let old_id = self.sessions.active_id();
                let name = self.sessions.active().name.clone();
                self.sessions.remove_active();
                self.session_permissions.remove(&old_id);
                self.session_usage.remove(&old_id);
                self.load_active_permissions();
                self.set_status(format!("Deleted: {}", name));
                self.sessions.save();
                self.touch();
            }
            Action::NextSession => {
                self.stash_active_permissions();
                self.stash_draft();
                self.sessions.select_next();
                self.load_active_permissions();
                self.load_active_draft();
                self.show_last_prompt = false;
                self.chat.stick_bottom = true;
                self.touch();
            }
            Action::PrevSession => {
                self.stash_active_permissions();
                self.stash_draft();
                self.sessions.select_prev();
                self.load_active_permissions();
                self.load_active_draft();
                self.show_last_prompt = false;
                self.chat.stick_bottom = true;
                self.touch();
            }
            Action::OpenSessionPicker => {
                if matches!(&self.overlay, Overlay::Picker(p) if p.kind == PickerKind::Session) {
                    self.overlay = Overlay::None;
                } else {
                    self.overlay = Overlay::Picker(Picker::sessions(
                        self.session_items(),
                        self.sessions.active_idx() + 1,
                    ));
                }
            }
            Action::SelectSession(i) => {
                // An entered child-agent view belongs to the old session's tree;
                // pop back to that session's root chat before switching.
                self.view_node = None;
                self.stash_active_permissions();
                self.stash_draft();
                self.sessions.select(i);
                self.load_active_permissions();
                self.load_active_draft();
                // Resume in the session's own folder so file tools / @-mentions
                // resolve against the right project.
                let cwd = self.sessions.active().cwd.clone();
                let mut where_ = String::new();
                if let Some(dir) = cwd {
                    if std::env::set_current_dir(&dir).is_ok() {
                        where_ = format!("  ({})", dir.display());
                    }
                }
                self.set_status(format!(
                    "Session: {}{}",
                    self.sessions.active().name,
                    where_
                ));
                self.show_last_prompt = false;
                self.chat.stick_bottom = true;
                self.touch();
            }
            Action::RenameSession(name) => {
                self.sessions.active_mut().name = name.clone();
                self.set_status(format!("Renamed: {}", name));
                self.sessions.save();
            }
            Action::SessionTitleGenerated(sid, title) => {
                let title = clean_session_title(&title);
                if !title.is_empty() {
                    if let Some(s) = self.sessions.by_id_mut(sid) {
                        if s.name.starts_with("Session ") || s.name == "Naming…" {
                            s.name = title;
                            self.sessions.save();
                            self.touch();
                        }
                    }
                }
            }
            Action::ResponseSuggestionsReady(sid, signature, suggestions) => {
                self.apply_response_suggestions(sid, signature, suggestions);
            }
            Action::SessionMemoryExtracted {
                session_id,
                source_turn,
                result,
            } => {
                self.apply_session_memory_result(session_id, source_turn, result);
            }
            Action::TodoUpdateReady(sid, signature, result) => {
                self.apply_todo_update(sid, signature, result);
            }
            Action::AcceptResponseSuggestion(index) => {
                self.accept_response_suggestion(index);
            }
            Action::SetResponseSuggestions(enabled) => {
                self.set_response_suggestions(enabled);
            }
            Action::DeleteSessionAt(idx) => {
                let active_id = self.sessions.active_id();
                let deleted_id = self.sessions.all().get(idx).map(|s| s.id);
                let name = self
                    .sessions
                    .all()
                    .get(idx)
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                self.sessions.remove_at(idx);
                if let Some(id) = deleted_id {
                    self.session_permissions.remove(&id);
                    self.session_usage.remove(&id);
                    if id == active_id {
                        self.load_active_permissions();
                    }
                }
                self.sessions.save();
                self.set_status(format!("Deleted: {}", name));
                self.chat.stick_bottom = true;
                self.touch();
                // Keep the picker open on the refreshed list (or close it if this was
                // opened outside the picker).
                if matches!(&self.overlay, Overlay::Picker(p) if p.kind == PickerKind::Session) {
                    self.overlay = Overlay::Picker(Picker::sessions(
                        self.session_items(),
                        self.sessions.active_idx() + 1,
                    ));
                }
            }

            // ── Skills ──────────────────────────────────────────────────────
            Action::OpenSkillPicker => {
                if matches!(&self.overlay, Overlay::Picker(p) if p.kind == PickerKind::Skill) {
                    self.overlay = Overlay::None;
                } else {
                    self.skills = crate::skills::reload_preserving_active(&self.skills);
                    if self.skills.is_empty() {
                        self.set_status(format!(
                            "No skills. Add .md files in {}",
                            crate::skills::skills_dir().display()
                        ));
                    } else {
                        self.overlay = Overlay::Picker(Picker::skills(self.skill_items()));
                    }
                }
            }
            Action::ToggleSkill(i) => {
                if i < self.skills.len() {
                    self.skills[i].active = !self.skills[i].active;
                    let (name, on) = (self.skills[i].name.clone(), self.skills[i].active);
                    self.set_status(format!("Skill {}: {}", name, if on { "ON" } else { "off" }));
                    // Sticky: remember active skills across restarts.
                    if self.config.ui.sticky_skills {
                        crate::skills::save_active(&self.skills);
                    }
                    // Refresh the open picker's rows so the ✓ marks update.
                    let sel = match &self.overlay {
                        Overlay::Picker(p) if p.kind == PickerKind::Skill => Some(p.selected),
                        _ => None,
                    };
                    if let Some(sel) = sel {
                        let mut np = Picker::skills(self.skill_items());
                        np.selected = sel.min(np.filtered.len().saturating_sub(1));
                        self.overlay = Overlay::Picker(np);
                    }
                }
            }

            // ── Models ──────────────────────────────────────────────────────
            Action::OpenModelPicker => {
                if matches!(&self.overlay, Overlay::Picker(p) if p.kind == PickerKind::Model) {
                    self.overlay = Overlay::None;
                } else {
                    self.overlay = Overlay::Picker(Picker::models(self.models.clone()));
                }
            }
            Action::SelectModel(m) => {
                if let Some(i) = self.models.iter().position(|x| x == &m) {
                    self.model_idx = i;
                } else {
                    self.models.push(m.clone());
                    self.model_idx = self.models.len() - 1;
                }
            }
            Action::NextModel => {
                if !self.models.is_empty() {
                    self.model_idx = (self.model_idx + 1) % self.models.len();
                }
            }
            Action::PrevModel => {
                if !self.models.is_empty() {
                    self.model_idx = (self.model_idx + self.models.len() - 1) % self.models.len();
                }
            }
            Action::ReloadModels => {
                self.refresh_models();
                self.set_status("Reloading models…");
            }
            Action::ModelsLoaded(mut models) => {
                use crate::app::state::{ModelLoad, MOCK_MODEL};
                // `mock` is always available as the last-resort model.
                if !models.iter().any(|m| m == MOCK_MODEL) {
                    models.push(MOCK_MODEL.to_string());
                }
                // Selection priority: whatever was already chosen (on a refresh), then
                // the configured default if it exists, then mock.
                let current = self.current_model().to_string();
                let default = self.config.api.default_model.clone();
                self.models = models;
                self.model_idx = self
                    .models
                    .iter()
                    .position(|m| m == &current && m != MOCK_MODEL)
                    .or_else(|| self.models.iter().position(|m| m == &default))
                    .or_else(|| self.models.iter().position(|m| m == MOCK_MODEL))
                    .unwrap_or(0);
                self.model_load = ModelLoad::Loaded;
                let real = self.models.iter().filter(|m| *m != MOCK_MODEL).count();
                self.set_status(format!(
                    "Loaded {} model{}",
                    real,
                    if real == 1 { "" } else { "s" }
                ));
            }
            Action::ModelsFailed => {
                // Connection/timeout — fall back to mock only and flag the failure.
                use crate::app::state::{ModelLoad, MOCK_MODEL};
                self.models = vec![MOCK_MODEL.to_string()];
                self.model_idx = 0;
                self.model_load = ModelLoad::Failed;
                self.set_status(
                    "Could not load models — using mock. Check endpoint/key, then :api",
                );
            }

            // ── Files / attachment ──────────────────────────────────────────
            Action::OpenFilePicker => {
                // Toggle: a second press closes the browser.
                if self.overlay.is_browser() {
                    self.overlay = Overlay::None;
                } else {
                    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                    self.overlay =
                        Overlay::Browser(FileBrowser::open(cwd, BrowsePurpose::Attach, Vec::new()));
                }
            }
            Action::AttachFile(path) => {
                if path.exists() {
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("?")
                        .to_string();
                    self.attachment = Some(path);
                    self.set_status(format!("Attached: {}", name));
                } else {
                    self.set_status(format!("Not found: {}", path.display()));
                }
            }
            Action::ClearAttachment => {
                self.attachment = None;
                self.set_status("Attachment cleared");
            }

            // ── Overlays ────────────────────────────────────────────────────
            Action::OpenCommandPalette => {
                if matches!(&self.overlay, Overlay::Palette(_)) {
                    self.overlay = Overlay::None;
                } else {
                    self.mention.reset();
                    self.overlay = Overlay::Palette(crate::app::overlay::Palette::new());
                }
            }
            Action::OpenCommandLine => {
                if matches!(&self.overlay, Overlay::CommandLine(_)) {
                    self.overlay = Overlay::None;
                } else {
                    self.mention.reset();
                    self.overlay = Overlay::CommandLine(crate::app::overlay::CommandLine::new());
                }
            }
            Action::OpenSettings => {
                if matches!(&self.overlay, Overlay::Settings(_)) {
                    self.overlay = Overlay::None;
                } else {
                    self.overlay = Overlay::Settings(Settings {
                        selected: 0,
                        editing: false,
                        edit_buf: String::new(),
                    });
                }
            }
            Action::OpenApiSetup => {
                let ep = self.config.api.endpoint.clone();
                let key = self.config.api.api_key.clone();
                self.overlay = Overlay::ApiSetup(crate::app::overlay::ApiSetup::new(ep, key));
                self.set_status("Enter API URL + key · Tab switch · ⏎ save · Esc cancel");
            }
            Action::PickerUp => self.picker_up(),
            Action::PickerDown => self.picker_down(),
            Action::PickerConfirm => return self.picker_confirm(),
            Action::PickerCancel => self.overlay = Overlay::None,
            Action::PickerChar(c) => self.picker_char(c),
            Action::PickerBackspace => return self.picker_backspace(),
            Action::CommandLineNext => self.command_line_next(),
            Action::CommandLinePrev => self.command_line_prev(),
            Action::CommandLineAccept => {
                if let Overlay::CommandLine(cl) = &mut self.overlay {
                    if let Some(name) = cl.selected_name() {
                        cl.accept_completion(name);
                    }
                }
            }
            Action::SettingsLeft => {
                if let Overlay::Permission(request) = &mut self.overlay {
                    request.adjust(-1);
                } else {
                    self.settings_adjust(-1);
                }
            }
            Action::SettingsRight => {
                if let Overlay::Permission(request) = &mut self.overlay {
                    request.adjust(1);
                } else {
                    self.settings_adjust(1);
                }
            }

            // ── @ mentions ──────────────────────────────────────────────────
            Action::MentionUp => self.mention.up(),
            Action::MentionDown => self.mention.down(),
            Action::MentionAccept => self.accept_mention(),
            Action::MentionCancel => self.mention.reset(),

            // ── Agent ───────────────────────────────────────────────────────
            // Agent is always on. Permission actions below.
            Action::AgentReviewPermission => return self.review_permission(),
            Action::AgentResolvePermission => {
                let (perm, reason, editing_access) = match &self.overlay {
                    Overlay::Permission(r) => match r.deny_choice() {
                        Some(choice) => (choice.0, choice.1, r.editing_access),
                        None => (r.permission(), None, r.editing_access),
                    },
                    _ => return None,
                };
                if let Some(index) = editing_access {
                    if let Permission::Custom(draft) = perm {
                        if self.replace_access_entry(index, draft) {
                            self.overlay = Overlay::Picker(Picker::access(self.access_items()));
                            self.set_status("Access rule updated for this session");
                            self.touch();
                        }
                    }
                    return None;
                }
                if is_deny(&perm) && !self.permission_reason_open() {
                    return self.begin_deny(perm);
                }
                return self.resolve_permission(perm, reason);
            }
            Action::AgentQuickAllow => return self.resolve_permission(Permission::Allow, None),
            Action::AgentQuickDeny => return self.begin_deny(Permission::Deny),
            Action::AgentDenyCancel => {
                if let Overlay::Permission(r) = &mut self.overlay {
                    r.cancel_deny();
                    self.set_status(
                        "Access — ↑↓ option · ←→ phrase · a allow · d deny · e edit · p policy · ⏎ model review · Esc cancel",
                    );
                    self.touch();
                }
            }
            Action::AgentDecisionToggle => {
                if let Overlay::Decision(r) = &mut self.overlay {
                    r.toggle();
                }
            }
            Action::AgentDecisionCustom => {
                if let Overlay::Decision(r) = &mut self.overlay {
                    r.toggle_custom_editor();
                    self.touch();
                }
            }
            Action::AgentDecisionEdit => {
                if let Overlay::Decision(r) = &self.overlay {
                    let text = if r.custom_selected() {
                        r.answer.clone()
                    } else {
                        r.options.get(r.selected).cloned().unwrap_or_default()
                    };
                    let path = std::env::temp_dir()
                        .join(format!("aitui-decision-{}.txt", std::process::id()));
                    if std::fs::write(&path, text).is_ok() {
                        self.pending_external = Some(PendingExternal::DecisionReadback(path));
                    } else {
                        self.set_status("Couldn't edit option — temp file write failed");
                    }
                }
            }
            Action::AgentDecisionEdited(text) => {
                if let Overlay::Decision(r) = &mut self.overlay {
                    if r.custom_selected() {
                        r.answer = text.trim().to_string();
                    } else if let Some(option) = r.options.get_mut(r.selected) {
                        *option = text.trim().to_string();
                    }
                    self.set_status("Option updated — Enter to choose");
                    self.touch();
                }
            }
            Action::AgentResolveDecision => return self.resolve_decision(),
            Action::AgentPermScrollUp => {
                if let Overlay::Permission(r) = &mut self.overlay {
                    r.scroll_up();
                    self.touch();
                }
            }
            Action::AgentPermScrollDown => {
                if let Overlay::Permission(r) = &mut self.overlay {
                    r.scroll_down();
                    self.touch();
                }
            }
            Action::AgentPermScrollLeft => {
                if let Overlay::Permission(r) = &mut self.overlay {
                    r.scroll_left();
                    self.touch();
                }
            }
            Action::AgentPermScrollRight => {
                if let Overlay::Permission(r) = &mut self.overlay {
                    r.scroll_right();
                    self.touch();
                }
            }
            Action::AgentPermissionSelector => {
                if let Overlay::Permission(r) = &mut self.overlay {
                    r.toggle_selector();
                    self.touch();
                }
            }
            Action::AgentPermissionFolderParent => {
                if let Overlay::Permission(r) = &mut self.overlay {
                    r.selector_parent();
                    self.touch();
                }
            }
            Action::AgentPermissionSelectorCancel => {
                if let Overlay::Permission(r) = &mut self.overlay {
                    r.close_selector();
                    self.touch();
                }
            }
            Action::AgentPermissionCustom => {
                if let Overlay::Permission(r) = &mut self.overlay {
                    r.toggle_custom_edit();
                    self.touch();
                }
            }
            Action::AgentPermissionEdit => {
                if let Overlay::Permission(r) = &self.overlay {
                    // Write the batch to a temp file and open $EDITOR; the edited
                    // contents come back as AgentPermissionEdited on return.
                    let path = std::env::temp_dir()
                        .join(format!("aitui-commands-{}.txt", std::process::id()));
                    if std::fs::write(&path, r.edit_buffer()).is_ok() {
                        self.pending_external = Some(PendingExternal::EditReadback(path));
                    } else {
                        self.set_status("Couldn't open editor — temp file write failed");
                    }
                }
            }
            Action::AgentPermissionEdited(text) => return self.apply_permission_edits(&text),
            Action::OpenAccessManager => {
                self.overlay = Overlay::Picker(Picker::access(self.access_items()));
            }
            Action::DisableAccessReview => return self.disable_access_review(),
            Action::EditAccessEntry(index) => {
                if let Some(request) = self.access_request_for_entry(index) {
                    self.overlay = Overlay::Permission(request);
                    self.set_status("Edit access — change fields · Enter save · Esc cancel");
                    self.touch();
                } else if index == 0 && self.permissions.policy.is_some() {
                    return Some(Action::AgentEditPolicy);
                }
            }
            Action::RemoveAccessEntry(index) => {
                if self.remove_access_entry(index) {
                    self.set_status("Access entry crossed off");
                    self.overlay = Overlay::Picker(Picker::access(self.access_items()));
                    self.touch();
                }
            }
            Action::SetAccessPolicy(text) => return self.set_access_policy(&text),
            Action::SetAccessReviewMode(mode) => return self.set_access_review_mode(mode),
            Action::AgentEditPolicy => {
                // Write the current policy (with a header) to a temp file and open
                // $EDITOR; the saved contents return as SetAccessPolicy.
                let current = self.permissions.policy.clone().unwrap_or_default();
                let seed = format!(
                    "# Session access policy — describe, in plain language, what tool\n\
                     # access to auto-allow this session. Lines starting with # are\n\
                     # ignored. Save empty to clear the policy.\n\
                     # e.g. \"Allow reads and searches anywhere, and any git or cargo\n\
                     #       command. Ask before writing files or deleting anything.\"\n\
                     {}\n",
                    current
                );
                let path =
                    std::env::temp_dir().join(format!("aitui-policy-{}.txt", std::process::id()));
                if std::fs::write(&path, seed).is_ok() {
                    self.pending_external = Some(PendingExternal::PolicyReadback(path));
                } else {
                    self.set_status("Couldn't open editor — temp file write failed");
                }
            }
            Action::AccessJudged(sid, verdicts) => {
                return self.apply_access_verdicts(sid, verdicts)
            }
            Action::StartLoop(goal) => {
                return self.start_loop(goal, String::new(), LoopState::DEFAULT_MAX)
            }
            Action::StartLoopSpec(text) => {
                let (goal, stop, max) = parse_loop_spec(&text);
                return self.start_loop(goal, stop, max);
            }
            Action::AgentEditLoop => {
                let seed =
                    "# Autonomous loop. Fill in and save; lines starting with # are ignored.\n\
                            # The agent will keep working on GOAL each turn until STOP is met\n\
                            # (it calls `finish`), you hit MAX iterations, or you press Ctrl-C.\n\
                            GOAL: \n\
                            STOP: \n\
                            MAX: 25\n"
                        .to_string();
                let path =
                    std::env::temp_dir().join(format!("aitui-loop-{}.txt", std::process::id()));
                if std::fs::write(&path, seed).is_ok() {
                    self.pending_external = Some(PendingExternal::LoopReadback(path));
                } else {
                    self.set_status("Couldn't open editor — temp file write failed");
                }
            }
            Action::StopLoop => self.stop_loop(),
            Action::AgentPlanEdit => {
                if let Overlay::Plan(r) = &self.overlay {
                    self.pending_external =
                        Some(PendingExternal::EditorFiles(vec![r.path.clone()]));
                }
            }
            Action::AgentPlanAccept => return self.resolve_plan(true),
            Action::AgentPlanDeny => return self.resolve_plan(false),
            Action::AgentToolResult(result) => {
                self.agent_tool_rx = None;
                self.record_tool_result(result);
                return self.process_next_tool();
            }
            Action::AgentToolBatchResult(results) => {
                self.agent_tool_batch_rx = None;
                for result in results {
                    self.record_tool_result(result);
                }
                return self.process_next_tool();
            }
            Action::SubtaskEvent(event) => return self.handle_subtask_event(event),
            Action::AgentCancel => {
                self.overlay = Overlay::None;
                // Kill the in-flight tool round: shells are terminated at their
                // process group; the result (if it still lands) is dropped below.
                if let Some(abort) = self.agent_abort.take() {
                    abort.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                self.pending_tools.clear();
                self.approved.clear();
                self.judging = None;
                self.judge_rx = None;
                if let Some(task) = self.judge_task.take() {
                    task.abort();
                }
                self.agent_tool_rx = None;
                self.agent_tool_batch_rx = None;
                if let Some(barrier) = self.task_barrier.take() {
                    let mut cancelled = Vec::new();
                    for task_id in barrier.task_ids {
                        if let Some(task) = self.subtasks.iter_mut().find(|task| {
                            task.id == task_id
                                && task.status == crate::app::state::SubtaskStatus::Running
                        }) {
                            task.status = crate::app::state::SubtaskStatus::Failed;
                            task.activity = None;
                            task.output = Some("Cancelled by user".into());
                            task.duration_ms = Some(task.started_at.elapsed().as_millis() as u64);
                            if let Some(abort) = task.abort.take() {
                                abort.abort();
                            }
                            cancelled.push(task_id);
                        }
                    }
                    for task_id in cancelled {
                        self.sync_subtask_message(task_id);
                    }
                    self.sessions.save();
                }
                self.agent_session = None;
                self.active_tool = None;
                self.agent_iterations = 0;
                self.set_status("Agent round cancelled");
            }
            Action::AgentEnableTools => return self.enable_agent_and_run(),
            Action::AgentDeclineTools => return self.decline_agent_tools(),

            // ── System prompt ───────────────────────────────────────────────
            Action::SetSystemPrompt(p) => {
                self.sessions.active_mut().system_prompt = p.clone();
                self.set_status(match &p {
                    Some(s) => format!("System prompt set ({} chars)", s.len()),
                    None => "System prompt cleared".to_string(),
                });
                self.sessions.save();
            }
        }
        None
    }

    // ── Picker helpers ──────────────────────────────────────────────────────

    fn picker_up(&mut self) {
        match &mut self.overlay {
            Overlay::Picker(p) => p.up(),
            Overlay::Palette(p) => p.up(),
            Overlay::Settings(s) => {
                if !s.editing {
                    s.selected = s.selected.saturating_sub(1);
                }
            }
            Overlay::Permission(r) => r.selector_up(),
            Overlay::Decision(r) => r.up(),
            Overlay::ApiSetup(a) => a.next_field(),
            Overlay::CommandLine(_)
            | Overlay::ToolRequest(_)
            | Overlay::Plan(_)
            | Overlay::Browser(_)
            | Overlay::SubtaskDetail { .. }
            | Overlay::Notice { .. }
            | Overlay::None => {}
        }
    }
    fn picker_down(&mut self) {
        match &mut self.overlay {
            Overlay::Picker(p) => p.down(),
            Overlay::Palette(p) => p.down(),
            Overlay::Settings(s) => {
                if !s.editing && s.selected + 1 < SettingsRow::all().len() {
                    s.selected += 1;
                }
            }
            Overlay::Permission(r) => r.selector_down(),
            Overlay::Decision(r) => r.down(),
            Overlay::ApiSetup(a) => a.next_field(),
            Overlay::CommandLine(_)
            | Overlay::ToolRequest(_)
            | Overlay::Plan(_)
            | Overlay::Browser(_)
            | Overlay::SubtaskDetail { .. }
            | Overlay::Notice { .. }
            | Overlay::None => {}
        }
    }
    fn picker_char(&mut self, c: char) {
        match &mut self.overlay {
            Overlay::Picker(p) => {
                p.query.push(c);
                p.refilter();
            }
            Overlay::Palette(p) => {
                p.query.push(c);
                p.refilter();
            }
            Overlay::Settings(s) if s.editing => s.edit_buf.push(c),
            Overlay::CommandLine(cl) => cl.push(c),
            Overlay::ApiSetup(a) => a.push(c),
            Overlay::Decision(r) => r.push(c),
            Overlay::Permission(r) => r.push(c),
            _ => {}
        }
    }
    fn picker_backspace(&mut self) -> Option<Action> {
        match &mut self.overlay {
            Overlay::Picker(p) => {
                p.query.pop();
                p.refilter();
            }
            Overlay::Palette(p) => {
                p.query.pop();
                p.refilter();
            }
            Overlay::Settings(s) if s.editing => {
                s.edit_buf.pop();
            }
            Overlay::CommandLine(cl) => cl.pop(),
            Overlay::ApiSetup(a) => a.backspace(),
            Overlay::Decision(r) => r.backspace(),
            Overlay::Permission(r) => r.backspace(),
            _ => {}
        }
        None
    }

    fn command_line_next(&mut self) {
        if let Overlay::CommandLine(cl) = &mut self.overlay {
            cl.next();
        }
    }
    fn command_line_prev(&mut self) {
        if let Overlay::CommandLine(cl) = &mut self.overlay {
            cl.prev();
        }
    }

    /// Apply the API setup: save endpoint + key to config, rebuild the client, and
    /// leave mock mode if a real endpoint is now set.
    fn apply_api_setup(&mut self) {
        let (ep, key) = match &self.overlay {
            Overlay::ApiSetup(a) => (a.endpoint.trim().to_string(), a.api_key.trim().to_string()),
            _ => return,
        };
        self.overlay = Overlay::None;
        self.config.api.endpoint = ep.clone();
        self.config.api.api_key = key.clone();
        let _ = self.config.save();
        self.api = crate::api::ApiClient::new(&ep, &key).ok();
        if ep.is_empty() {
            self.select_mock_model();
            self.set_status("API endpoint cleared — using mock".to_string());
        } else {
            self.refresh_models();
            self.set_status(format!("API endpoint set: {} — loading models…", ep));
        }
    }

    fn input_history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let idx = match self.input_history_idx {
            None => {
                self.input_draft = self.input.text();
                self.input_history.len() - 1
            }
            Some(i) => i.saturating_sub(1),
        };
        self.input_history_idx = Some(idx);
        self.input.set_text(&self.input_history[idx]);
        self.mention.reset();
    }

    fn input_history_next(&mut self) {
        match self.input_history_idx {
            Some(i) if i + 1 < self.input_history.len() => {
                self.input_history_idx = Some(i + 1);
                let text = self.input_history[self.input_history_idx.unwrap()].clone();
                self.input.set_text(&text);
            }
            Some(_) => {
                // Past the newest entry: restore the draft.
                self.input_history_idx = None;
                self.input.set_text(&self.input_draft);
                self.input_draft.clear();
            }
            None => {}
        }
        self.mention.reset();
    }

    /// Picker rows for skills: a ✓/· active marker, name, and description.
    fn skill_items(&self) -> Vec<String> {
        self.skills
            .iter()
            .map(|s| {
                let mark = if s.active { "✓" } else { "·" };
                if s.desc.is_empty() {
                    format!("{} {}", mark, s.name)
                } else {
                    format!("{} {}  — {}", mark, s.name, s.desc)
                }
            })
            .collect()
    }

    pub(crate) fn access_items(&self) -> Vec<String> {
        let mode = self.config.api.access_review_mode;
        let state = if mode == crate::config::AccessReviewMode::Off {
            "OFF  Review model disabled".to_string()
        } else if self.judging.is_some() {
            format!(
                "ON   Review model {} · reviewing now · Enter to disable",
                mode.label()
            )
        } else {
            format!("ON   Review model {} · Enter to disable", mode.label())
        };
        std::iter::once(state)
            .chain(
                self.access_entries()
                    .into_iter()
                    .map(|(summary, _)| summary),
            )
            .collect()
    }

    fn access_entry_details(&self, index: usize) -> Option<String> {
        if index == 0 {
            let mode = self.config.api.access_review_mode;
            return Some(if mode == crate::config::AccessReviewMode::Off {
                "Review model: off\n\nTool calls not covered by remembered rules are sent directly to you for approval."
                    .to_string()
            } else {
                format!(
                    "Review model: {}\nStatus: {}\n\nChoose this row to disable automated review. Pending review work will stop and its access request will be shown directly.",
                    mode.label(),
                    if self.judging.is_some() { "reviewing now" } else { "on" }
                )
            });
        }
        self.access_entries()
            .get(index - 1)
            .map(|(_, detail)| detail.clone())
    }

    pub(crate) fn access_entries(&self) -> Vec<(String, String)> {
        let mut entries = Vec::new();
        if let Some(policy) = self.permissions.policy.as_ref() {
            entries.push((
                "POLICY  custom review instructions".to_string(),
                format!("Automated review policy\n\n{}", policy),
            ));
        }
        entries.extend(self.permissions.always_allow.iter().map(|kind| {
            (
                format!("ALLOW  {} · current session", kind.name()),
                format!(
                    "Decision: allow\nAccess type: {}\nLocation: anywhere\nLifetime: current session",
                    kind.name()
                ),
            )
        }));
        entries.extend(self.permissions.always_deny.iter().map(|kind| {
            (
                format!("DENY  {} · current session", kind.name()),
                format!(
                    "Decision: deny\nAccess type: {}\nLocation: anywhere\nLifetime: current session",
                    kind.name()
                ),
            )
        }));
        entries.extend(self.permissions.rules.iter().map(|rule| {
            let decision = match rule.decision {
                PermissionDecision::Allow => "ALLOW",
                PermissionDecision::Deny => "DENY",
            };
            let kind = rule.kind.map(|kind| kind.name()).unwrap_or("all access types");
            let location = rule
                .directory
                .as_ref()
                .map(|dir| crate::render::path::display_path(dir))
                .unwrap_or_else(|| "anywhere".to_string());
            let children = if rule.include_children && rule.directory.is_some() {
                " + child directories"
            } else {
                ""
            };
            let lifetime = rule
                .remaining_matching
                .map(|n| format!("next {} matching requests", n))
                .or_else(|| rule.remaining_general.map(|n| format!("next {} total requests", n)))
                .or_else(|| {
                    rule.expires_at.map(|expires| {
                        let remaining = expires.saturating_sub(access_now_secs());
                        format!("{}m {}s remaining", remaining / 60, remaining % 60)
                    })
                })
                .unwrap_or_else(|| "current session".to_string());
            (
                format!(
                    "{}  {} · {}{} · {}",
                    decision, kind, location, children, lifetime
                ),
                format!(
                    "Decision: {}\nAccess type: {}\nLocation: {}\nChild directories: {}\nLifetime: {}",
                    decision.to_lowercase(),
                    kind,
                    location,
                    if rule.include_children && rule.directory.is_some() {
                        "included"
                    } else {
                        "not included"
                    },
                    lifetime
                ),
            )
        }));
        if entries.is_empty() {
            entries.push((
                "EMPTY  Access is requested when needed".to_string(),
                "No remembered access rules. Tool calls will request approval when needed."
                    .to_string(),
            ));
        }
        entries
    }

    fn access_request_for_entry(
        &self,
        index: usize,
    ) -> Option<crate::app::overlay::PermissionRequest> {
        let policy = usize::from(self.permissions.policy.is_some());
        if index < policy {
            return None;
        }
        let index = index - policy;
        let cwd = self
            .sessions
            .active()
            .cwd
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let mut request = crate::app::overlay::PermissionRequest::new(Vec::new(), cwd);
        request.selected = 0;
        request.lifetime_index = 1;
        request.lifetime_explicit = true;
        request.editing_access = Some(index + policy);
        if let Some(kind) = self.permissions.always_allow.get(index).copied() {
            request.decision = PermissionDecision::Allow;
            request.tool_index = ToolKind::all()
                .iter()
                .position(|candidate| *candidate == kind)?
                + 1;
            return Some(request);
        }
        let index = index.saturating_sub(self.permissions.always_allow.len());
        if let Some(kind) = self.permissions.always_deny.get(index).copied() {
            request.decision = PermissionDecision::Deny;
            request.tool_index = ToolKind::all()
                .iter()
                .position(|candidate| *candidate == kind)?
                + 1;
            return Some(request);
        }
        let index = index.saturating_sub(self.permissions.always_deny.len());
        let rule = self.permissions.rules.get(index)?;
        request.decision = rule.decision;
        request.tool_index = rule
            .kind
            .or_else(|| match rule.scope {
                crate::agent::PermissionScope::Kind(kind) => Some(kind),
                _ => None,
            })
            .and_then(|kind| {
                ToolKind::all()
                    .iter()
                    .position(|candidate| *candidate == kind)
            })
            .map(|index| index + 1)
            .unwrap_or(0);
        request.custom_directory = rule
            .directory
            .as_ref()
            .or_else(|| match &rule.scope {
                crate::agent::PermissionScope::Directory(directory) => Some(directory),
                _ => None,
            })
            .map(|directory| directory.to_string_lossy().to_string())
            .unwrap_or_default();
        request.location_index = usize::from(!request.custom_directory.is_empty()) * 4;
        request.include_children = rule.include_children;
        request.lifetime_index = if let Some(remaining) = rule.remaining_matching {
            request.custom_value = remaining.to_string();
            3
        } else if let Some(remaining) = rule.remaining_general {
            request.custom_value = remaining.to_string();
            4
        } else if let Some(expires) = rule.expires_at {
            let remaining = expires
                .saturating_sub(access_now_secs())
                .div_ceil(60)
                .max(1);
            request.custom_value = remaining.to_string();
            2
        } else {
            1
        };
        Some(request)
    }

    fn replace_access_entry(
        &mut self,
        index: usize,
        draft: crate::agent::PermissionRuleDraft,
    ) -> bool {
        if !self.remove_access_entry(index) {
            return false;
        }
        self.permissions.remember_custom_rule(draft);
        true
    }

    fn remove_access_entry(&mut self, index: usize) -> bool {
        let policy = usize::from(self.permissions.policy.is_some());
        if index < policy {
            self.permissions.policy = None;
            return true;
        }
        let index = index.saturating_sub(policy);
        if index < self.permissions.always_allow.len() {
            self.permissions.always_allow.remove(index);
            return true;
        }
        let index = index.saturating_sub(self.permissions.always_allow.len());
        if index < self.permissions.always_deny.len() {
            self.permissions.always_deny.remove(index);
            return true;
        }
        let index = index.saturating_sub(self.permissions.always_deny.len());
        if index < self.permissions.rules.len() {
            self.permissions.rules.remove(index);
            return true;
        }
        false
    }

    /// Open/attach the current target(s) in the file browser. Folders descend.
    fn browser_open(&mut self) -> Option<Action> {
        let Overlay::Browser(b) = &mut self.overlay else {
            return None;
        };
        if b.current().map(|e| e.is_dir).unwrap_or(false) {
            b.enter_dir();
            return None;
        }
        let targets = b.resolve_targets();
        if targets.is_empty() {
            return None;
        }
        let purpose = b.purpose;
        self.overlay = Overlay::None;
        match purpose {
            BrowsePurpose::Edit => Some(Action::OpenFilesInEditor(targets)),
            // Attach takes a single file (the current one).
            BrowsePurpose::Attach => targets.into_iter().next().map(Action::AttachFile),
        }
    }

    fn picker_confirm(&mut self) -> Option<Action> {
        // Skill picker multi-toggles and stays open, so handle it before the
        // replace-with-None that closes every other overlay.
        if let Overlay::Picker(p) = &self.overlay {
            if p.kind == PickerKind::Skill {
                return p.selected_index().map(Action::ToggleSkill);
            }
        }
        match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Picker(p) => match p.kind {
                PickerKind::Model => p
                    .selected_item()
                    .map(|m| Action::SelectModel(m.to_string())),
                PickerKind::Session => p.selected_index().map(|i| {
                    if i == 0 {
                        Action::NewSession
                    } else {
                        Action::SelectSession(i - 1)
                    }
                }),
                PickerKind::Skill => None,
                PickerKind::Access => {
                    if let Some(body) = p
                        .selected_index()
                        .and_then(|index| self.access_entry_details(index))
                    {
                        self.overlay = Overlay::Notice {
                            title: "Access rule".to_string(),
                            body,
                        };
                    }
                    None
                }
            },
            Overlay::Palette(p) => p
                .selected_cmd()
                .map(|c| Action::RunCommand(c.run.to_string())),
            // Put the overlay back and go through the one Enter path: it owns the
            // deny-reason step, and `resolve_permission` reads the batch off the
            // overlay (so resolving with it already cleared would silently no-op).
            Overlay::Permission(r) => {
                self.overlay = Overlay::Permission(r);
                Some(Action::AgentResolvePermission)
            }
            Overlay::Decision(r) => {
                self.overlay = Overlay::Decision(r);
                self.resolve_decision()
            }
            Overlay::Plan(r) => {
                self.overlay = Overlay::Plan(r);
                self.resolve_plan(true)
            }
            Overlay::Settings(s) => {
                self.overlay = Overlay::Settings(s);
                self.settings_confirm();
                None
            }
            Overlay::Browser(b) => {
                self.overlay = Overlay::Browser(b);
                self.browser_open()
            }
            Overlay::ApiSetup(a) => {
                self.overlay = Overlay::ApiSetup(a);
                self.apply_api_setup();
                None
            }
            Overlay::CommandLine(_)
            | Overlay::ToolRequest(_)
            | Overlay::SubtaskDetail { .. }
            | Overlay::Notice { .. }
            | Overlay::None => None,
        }
    }

    fn settings_confirm(&mut self) {
        let (row, editing) = match &self.overlay {
            Overlay::Settings(s) => (SettingsRow::all().get(s.selected).copied(), s.editing),
            _ => return,
        };
        match row {
            Some(SettingsRow::ReasoningEffort)
            | Some(SettingsRow::ReasoningMode)
            | Some(SettingsRow::SystemPrompt) => {
                if editing {
                    let value = match &mut self.overlay {
                        Overlay::Settings(s) => {
                            s.editing = false;
                            s.edit_buf.trim().to_string()
                        }
                        _ => return,
                    };
                    match row {
                        Some(SettingsRow::ReasoningEffort) => {
                            self.reasoning_effort = reasoning_value(&value);
                            self.config.api.reasoning_effort =
                                self.reasoning_effort.clone().unwrap_or_default();
                            let _ = self.config.save();
                        }
                        Some(SettingsRow::ReasoningMode) => {
                            self.reasoning_mode = reasoning_value(&value);
                            self.config.api.reasoning_mode =
                                self.reasoning_mode.clone().unwrap_or_default();
                            let _ = self.config.save();
                        }
                        Some(SettingsRow::SystemPrompt) => {
                            let prompt = match value.trim() {
                                "" => None,
                                value => Some(value.to_string()),
                            };
                            self.sessions.active_mut().system_prompt = prompt;
                            self.sessions.save();
                        }
                        _ => {}
                    }
                } else {
                    let initial = match row {
                        Some(SettingsRow::ReasoningEffort) => {
                            self.reasoning_effort.clone().unwrap_or_default()
                        }
                        Some(SettingsRow::ReasoningMode) => {
                            self.reasoning_mode.clone().unwrap_or_default()
                        }
                        Some(SettingsRow::SystemPrompt) => self
                            .sessions
                            .active()
                            .system_prompt
                            .clone()
                            .unwrap_or_default(),
                        _ => String::new(),
                    };
                    if let Overlay::Settings(s) = &mut self.overlay {
                        s.editing = true;
                        s.edit_buf = initial;
                    }
                }
            }
            Some(SettingsRow::AutoApprove) | Some(SettingsRow::AccessReview) => {
                self.settings_adjust(0)
            }
            _ => {}
        }
    }

    fn settings_adjust(&mut self, dir: i32) {
        let Overlay::Settings(s) = &self.overlay else {
            return;
        };
        let Some(row) = SettingsRow::all().get(s.selected).copied() else {
            return;
        };
        match row {
            SettingsRow::AutoApprove => {
                self.config.ui.auto_approve_reads = !self.config.ui.auto_approve_reads;
                crate::app::overlay::sync_auto_approvals(
                    &mut self.permissions,
                    self.config.ui.auto_approve_reads,
                );
            }
            SettingsRow::AccessReview => {
                let direction = if dir < 0 { -1 } else { 1 };
                self.config.api.access_review_mode =
                    self.config.api.access_review_mode.cycle(direction);
                self.set_status(format!(
                    "Permission review: {}",
                    self.config.api.access_review_mode.label()
                ));
            }
            SettingsRow::InputHeight => {
                let h = self.config.ui.input_height as i32 + dir;
                self.config.ui.input_height = h.clamp(2, 20) as u16;
            }
            SettingsRow::ReasoningEffort
            | SettingsRow::ReasoningMode
            | SettingsRow::SystemPrompt => {}
        }
        let _ = self.config.save();
    }

    // ── : commands ──────────────────────────────────────────────────────────

    /// Set the internal yank register and mirror it to the system clipboard, so a
    /// vim `y`/`d` copies out of the app too. No-op on empty text.
    fn set_yank(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.pending_clipboard = Some(text.clone());
        self.yank = Some(text);
    }

    fn run_command(&mut self, cmd: &str) -> Option<Action> {
        let cmd = cmd.trim().to_string();
        self.vim = VimMode::Normal;
        if !cmd.is_empty() && self.command_history.last().map(|s| s.as_str()) != Some(&cmd) {
            self.command_history.push(cmd.clone());
            if self.command_history.len() > 100 {
                self.command_history.remove(0);
            }
        }
        self.command_history_idx = None;
        self.command.clear();

        if let Some(action) = crate::app::commands::exact_command_action(&cmd) {
            return Some(action);
        }

        match cmd.as_str() {
            "q" | "quit" => return Some(Action::Quit),
            "w" | "write" | "send" => return Some(Action::Submit),
            "wq" | "x" => {
                let r = self.submit();
                self.should_quit = true;
                return r;
            }
            "new" | "n" => return Some(Action::NewSession),
            "fork" | "branch" => return Some(Action::ForkSession),
            "delete" | "rm" | "ds" => return Some(Action::DeleteSession),
            "clear" => {
                let session = self.sessions.active_mut();
                session.messages.clear();
                session.memories.clear();
                session.next_memory_id = 1;
                session.memory_source_turn = 0;
                self.sessions.save();
                self.chat.stick_bottom = true;
                self.touch();
                self.set_status("Chat cleared");
            }
            "models" | "model" => return Some(Action::OpenModelPicker),
            "reload-models" | "models-reload" | "refresh-models" | "model-reload" => {
                return Some(Action::ReloadModels)
            }
            "files" | "attach" => return Some(Action::OpenFilePicker),
            "detach" | "noattach" => return Some(Action::ClearAttachment),
            "mock" | "test" | "offline" => {
                // Mock is a model now — this just selects it.
                self.select_mock_model();
                self.set_status("Switched to the mock model (offline)");
            }
            "native" | "nativetools" => {
                self.set_status("Tool-calling: always on (required)");
            }
            "setup" | "apikey" | "endpoint" => return Some(Action::OpenApiSetup),
            "settings" | "config" | "set" => return Some(Action::OpenSettings),
            "sessions" | "ls" => return Some(Action::OpenSessionPicker),
            "skill" | "skills" => return Some(Action::OpenSkillPicker),
            "sticky" | "stickyskills" => {
                self.config.ui.sticky_skills = !self.config.ui.sticky_skills;
                let on = self.config.ui.sticky_skills;
                let _ = self.config.save();
                if on {
                    crate::skills::save_active(&self.skills);
                }
                self.set_status(format!(
                    "Sticky skills: {}",
                    if on {
                        "ON (remembered across restarts)"
                    } else {
                        "off"
                    }
                ));
            }
            "suggestions" | "response-suggestions" | "followups" => {
                return Some(Action::SetResponseSuggestions(
                    !self.config.ui.response_suggestions,
                ));
            }
            other
                if other.starts_with("suggestions ")
                    || other.starts_with("response-suggestions ")
                    || other.starts_with("followups ") =>
            {
                let value = other.split_once(' ').map(|(_, value)| value).unwrap_or("");
                let enabled = match value.trim().to_lowercase().as_str() {
                    "on" | "true" | "yes" | "1" => true,
                    "off" | "false" | "no" | "0" => false,
                    _ => {
                        self.set_status("Usage: :suggestions [on|off]");
                        return None;
                    }
                };
                return Some(Action::SetResponseSuggestions(enabled));
            }
            "effort" | "reasoning-effort" => {
                self.overlay = Overlay::CommandLine(CommandLine {
                    input: "reasoning-effort ".to_string(),
                    filtered: Vec::new(),
                    selected: 0,
                });
                return None;
            }
            other if other.starts_with("effort ") || other.starts_with("reasoning-effort ") => {
                let value = other
                    .split_once(' ')
                    .map(|(_, value)| value)
                    .unwrap_or("")
                    .trim();
                self.reasoning_effort = reasoning_value(value);
                self.config.api.reasoning_effort =
                    self.reasoning_effort.clone().unwrap_or_default();
                let _ = self.config.save();
                self.set_status(format!(
                    "Reasoning › effort: {}",
                    self.reasoning_effort.as_deref().unwrap_or("off")
                ));
            }
            "mode" | "reasoning-mode" => {
                self.overlay = Overlay::CommandLine(CommandLine {
                    input: "reasoning-mode ".to_string(),
                    filtered: Vec::new(),
                    selected: 0,
                });
                return None;
            }
            other if other.starts_with("mode ") || other.starts_with("reasoning-mode ") => {
                let value = other
                    .split_once(' ')
                    .map(|(_, value)| value)
                    .unwrap_or("")
                    .trim();
                self.reasoning_mode = reasoning_value(value);
                self.config.api.reasoning_mode = self.reasoning_mode.clone().unwrap_or_default();
                let _ = self.config.save();
                self.set_status(format!(
                    "Reasoning › mode: {}",
                    self.reasoning_mode.as_deref().unwrap_or("off")
                ));
            }
            "reasoning" => {
                self.set_status(format!(
                    "Reasoning › effort: {} · mode: {}",
                    self.reasoning_effort.as_deref().unwrap_or("off"),
                    self.reasoning_mode.as_deref().unwrap_or("off")
                ));
            }
            "retry" | "r" | "regen" | "regenerate" => return Some(Action::RetryLast),
            "edit-last" | "el" | "redo" => return Some(Action::EditLast),
            "copy" | "y" | "yank" => return Some(Action::CopyLastReply),
            "copy-code" | "yc" | "code" => return Some(Action::CopyLastCode),
            "editor" | "history" => return Some(Action::OpenEditor),
            "edit" | "e" | "edited" => return Some(Action::OpenEditPicker),
            "shell" | "term" | "terminal" | "sh" => return Some(Action::OpenShell),
            "?" | "help" => return Some(Action::ToggleHelp),
            "nosystem" | "system" => return Some(Action::SetSystemPrompt(None)),
            "loop" => return Some(Action::AgentEditLoop),
            "loop stop" | "loopstop" | "noloop" | "unloop" => return Some(Action::StopLoop),
            other if other.starts_with("loop ") => {
                return Some(Action::StartLoop(other[5..].trim().to_string()))
            }
            "access" | "policy" => return Some(Action::OpenAccessManager),
            "access edit" | "policy edit" => return Some(Action::AgentEditPolicy),
            "noaccess" | "nopolicy" => return Some(Action::SetAccessPolicy(String::new())),
            "review" | "permission-review" | "access-review" => {
                let mode = self.config.api.access_review_mode.cycle(1);
                return Some(Action::SetAccessReviewMode(mode));
            }
            other
                if other.starts_with("review ")
                    || other.starts_with("permission-review ")
                    || other.starts_with("access-review ") =>
            {
                let value = other.split_once(' ').map(|(_, value)| value).unwrap_or("");
                let mode = match value.trim().to_lowercase().as_str() {
                    "strict" | "on" => crate::config::AccessReviewMode::Strict,
                    "lenient" => crate::config::AccessReviewMode::Lenient,
                    "off" | "none" | "manual" => crate::config::AccessReviewMode::Off,
                    _ => {
                        self.set_status("Usage: :review [strict|lenient|off]");
                        return None;
                    }
                };
                return Some(Action::SetAccessReviewMode(mode));
            }
            other if other.starts_with("access ") => {
                return Some(Action::SetAccessPolicy(other[7..].trim().to_string()))
            }
            other if other.starts_with("model ") => {
                return Some(Action::SelectModel(other[6..].trim().to_string()))
            }
            other if other.starts_with("edit ") || other.starts_with("e ") => {
                let p = other.split_once(' ').map(|x| x.1).unwrap_or("").trim();
                if !p.is_empty() {
                    return Some(Action::OpenFilesInEditor(vec![PathBuf::from(p)]));
                }
            }
            other if other.starts_with("attach ") => {
                return Some(Action::AttachFile(PathBuf::from(other[7..].trim())))
            }
            other if other.starts_with("rename ") => {
                let name = other[7..].trim().to_string();
                if !name.is_empty() {
                    return Some(Action::RenameSession(name));
                }
            }
            other if other.starts_with("system ") => {
                return Some(Action::SetSystemPrompt(Some(other[7..].trim().to_string())))
            }
            other => self.set_status(format!("Unknown command: :{}", other)),
        }
        None
    }
}

/// Heuristic: does a stream error look like the endpoint rejecting the native
/// `tools` field (so we should fall back to fenced parsing)? Matches a 4xx that
/// mentions tools/functions or an explicit "not supported".
/// Does a stream error indicate a missing/relative endpoint URL (so we should
/// prompt for the API URL + key)?
/// Whether a permission choice denies (as opposed to allows), whatever its scope.
fn reasoning_value(value: &str) -> Option<String> {
    match value.trim() {
        "" => None,
        value if value.eq_ignore_ascii_case("off") || value.eq_ignore_ascii_case("none") => None,
        value => Some(value.to_string()),
    }
}

fn is_deny(perm: &Permission) -> bool {
    matches!(
        perm,
        Permission::Deny
            | Permission::DenyKind
            | Permission::DenyDirectory
            | Permission::DenyTimed
            | Permission::Custom(crate::agent::PermissionRuleDraft {
                decision: PermissionDecision::Deny,
                ..
            })
    )
}

fn looks_like_base_url_error(err: &str) -> bool {
    let e = err.to_lowercase();
    e.contains("relative url without a base")
        || e.contains("without a base")
        || e.contains("builder error")
        || e.contains("no api client")
}

/// Parse a `GOAL:`/`STOP:`/`MAX:` loop spec (from the `:loop` editor form) into
/// `(goal, stop, max)`. Comment (`#`) lines are ignored; a bad/absent MAX falls
/// back to the default cap.
fn parse_loop_spec(text: &str) -> (String, String, usize) {
    let (mut goal, mut stop, mut max) = (String::new(), String::new(), LoopState::DEFAULT_MAX);
    for line in text.lines() {
        let l = line.trim_start();
        if l.starts_with('#') {
            continue;
        }
        if let Some(v) = l.strip_prefix("GOAL:") {
            goal = v.trim().to_string();
        } else if let Some(v) = l.strip_prefix("STOP:") {
            stop = v.trim().to_string();
        } else if let Some(v) = l.strip_prefix("MAX:") {
            if let Ok(n) = v.trim().parse::<usize>() {
                max = n.max(1);
            }
        }
    }
    (goal, stop, max)
}

/// Whether a stream error is the endpoint rejecting the request for exceeding the
/// model's context window. Providers word this differently (OpenAI, Anthropic-
/// compatible gateways, vLLM, …) so match the common phrasings.
fn looks_like_context_overflow(err: &str) -> bool {
    let e = err.to_lowercase();
    e.contains("context length")
        || e.contains("context_length")
        || e.contains("context window")
        || e.contains("maximum context")
        || e.contains("too many tokens")
        || e.contains("reduce the length")
        || e.contains("prompt is too long")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::input_buffer::InputBuffer;
    use crate::app::overlay::{Mention, Overlay, Picker};
    use crate::app::state::{PanelLayout, Subtask};
    use crate::config::Config;
    use crate::domain::session::SessionManager;
    use crate::input::vim::VimMode;
    use crate::render::chat::ChatState;
    use std::collections::VecDeque;

    fn test_app() -> App {
        let config = Config::default();
        let keymap = crate::input::keymap::Keymap::from_config(&config.keybinds);
        let (spec_tx, spec_rx) = tokio::sync::mpsc::channel(64);
        let (suggestion_tx, suggestion_rx) = tokio::sync::mpsc::channel(32);
        let (todo_tx, todo_rx) = tokio::sync::mpsc::channel(32);
        let (memory_tx, memory_rx) = tokio::sync::mpsc::channel(32);
        let (subtask_tx, subtask_rx) = tokio::sync::mpsc::channel(64);
        let (notification_tx, notification_rx) = std::sync::mpsc::channel();
        App {
            config,
            keymap,
            agent_abort: None,
            sessions: SessionManager::new(),
            chat: ChatState::new(),
            doc_cache: crate::render::chat::DocCache::default(),
            vim: VimMode::Normal,
            input: InputBuffer::default(),
            command: String::new(),
            command_history: Vec::new(),
            command_history_idx: None,
            input_history: Vec::new(),
            input_history_idx: None,
            input_draft: String::new(),
            input_undo: Vec::new(),
            input_redo: Vec::new(),
            overlay: Overlay::None,
            mention: Mention::default(),
            pastes: Vec::new(),
            models: vec!["gpt-5.5".into(), "claude-sonnet-4-6".into(), "mock".into()],
            model_idx: 0,
            model_load: crate::app::state::ModelLoad::Loaded,
            attachment: None,
            status: None,
            focused: true,
            notification_tx,
            notification_rx,
            notification_generation: 0,
            show_help: false,
            help_detail: None,
            help_selected: 0,
            help_scroll: 0,
            sidebar_task_scroll: 0,
            should_quit: false,
            yank: None,
            last_insert: None,
            show_output: false,
            show_last_prompt: false,
            pending_image: None,
            pending_clipboard: None,
            edited_files: Vec::new(),
            pending_external: None,
            remote_running_sessions: std::collections::HashSet::new(),
            session_usage: std::collections::HashMap::new(),
            skills: Vec::new(),
            reasoning_effort: None,
            reasoning_mode: None,
            content_rev: 0,
            session_permissions: std::collections::HashMap::new(),
            permissions: crate::agent::PermissionMemory::default(),
            pending_tools: VecDeque::new(),
            approved: VecDeque::new(),
            judging: None,
            judge_rx: None,
            judge_task: None,
            agent_iterations: 0,
            streams: Vec::new(),
            agent_session: None,
            agent_queue: VecDeque::new(),
            agent_tool_rx: None,
            agent_tool_batch_rx: None,
            subtasks: Vec::new(),
            selected_subtask: None,
            view_node: None,
            subtask_id_alloc: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1)),
            task_barrier: None,
            subtask_tx,
            subtask_rx,
            active_tool: None,
            preparing_tool: None,
            models_rx: None,
            title_rx: None,
            suggestion_tx,
            suggestion_rx,
            suggestion_inflight: std::collections::HashSet::new(),
            todo_tx,
            todo_rx,
            todo_inflight: std::collections::HashMap::new(),
            memory_tx,
            memory_rx,
            memory_inflight: std::collections::HashSet::new(),
            memory_pending: std::collections::HashSet::new(),
            spec_results: std::collections::HashMap::new(),
            spec_dispatched: std::collections::HashSet::new(),
            spec_epoch: 0,
            spec_inflight: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            cut_stream: None,
            spec_tx,
            spec_rx,
            mention_files: Vec::new(),
            mention_files_at: None,
            mention_files_root: None,
            mouse_select: None,
            layout: PanelLayout::default(),
            api: None,
        }
    }

    #[test]
    fn stale_desktop_notification_cannot_resolve_a_new_permission_prompt() {
        let mut app = test_app();
        app.notification_generation = 2;
        app.overlay = Overlay::Permission(crate::app::overlay::PermissionRequest::single(
            tool_call("write", serde_json::json!({"path": "x"})),
        ));
        app.apply(Action::DesktopNotification(
            crate::app::notify::DesktopResponse {
                generation: 1,
                action: crate::app::notify::DesktopAction::AllowOnce,
            },
        ));
        assert!(matches!(app.overlay, Overlay::Permission(_)));
        assert_eq!(
            app.status.as_deref(),
            Some("That notification is no longer current")
        );
    }

    #[test]
    fn current_desktop_review_keeps_the_pending_prompt_open() {
        let mut app = test_app();
        app.notification_generation = 3;
        app.overlay = Overlay::Permission(crate::app::overlay::PermissionRequest::single(
            tool_call("write", serde_json::json!({"path": "x"})),
        ));
        app.apply(Action::DesktopNotification(
            crate::app::notify::DesktopResponse {
                generation: 3,
                action: crate::app::notify::DesktopAction::Review,
            },
        ));
        assert!(matches!(app.overlay, Overlay::Permission(_)));
        assert_eq!(
            app.status.as_deref(),
            Some("Review the pending request in AiTUI")
        );
    }

    #[test]
    fn selecting_access_option_invalidates_desktop_notification() {
        let mut app = test_app();
        app.notification_generation = 9;
        app.overlay = Overlay::Permission(crate::app::overlay::PermissionRequest::single(
            tool_call("write", serde_json::json!({"path": "x"})),
        ));

        app.resolve_permission(Permission::Deny, None);

        assert_eq!(app.notification_generation, 10);
        assert!(!matches!(app.overlay, Overlay::Permission(_)));
    }

    #[test]
    fn full_frame_render_smoke_covers_transcript_input_todo_and_permission_overlay() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = test_app();
        app.sessions
            .active_mut()
            .push_message(crate::api::ChatMessage::user("Check the project"));
        app.sessions
            .active_mut()
            .push_message(crate::api::ChatMessage::assistant("Everything renders."));
        app.sessions.active_mut().todos = vec![crate::app::state::TodoItem {
            text: "Render smoke test".into(),
            status: crate::app::state::TodoStatus::InProgress,
            percent: None,
        }];
        app.input.set_text("draft response");
        app.overlay = Overlay::Permission(crate::app::overlay::PermissionRequest::new(
            vec![tool_call(
                "write",
                serde_json::json!({"path": "src/lib.rs", "content": "x"}),
            )],
            app.sessions
                .active()
                .cwd
                .clone()
                .unwrap_or_else(|| std::env::current_dir().unwrap()),
        ));

        let backend = TestBackend::new(100, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();

        assert!(app.layout.chat.width > 0);
        assert!(app.layout.session_tabs.is_empty());
        assert!(!app.chat.doc().is_empty());
    }

    #[test]
    fn image_request_errors_are_not_reported_as_stream_errors() {
        let mut app = test_app();
        let sid = app.sessions.active_id();
        app.sessions
            .by_id_mut(sid)
            .unwrap()
            .begin_assistant_stream();
        app.apply(Action::StreamImageError(
            sid,
            "provider rejected prompt".into(),
        ));
        assert_eq!(
            app.status.as_deref(),
            Some("Image request failed: provider rejected prompt")
        );
        assert!(app
            .sessions
            .by_id(sid)
            .unwrap()
            .pending_assistant_text
            .is_none());
    }

    #[test]
    fn completed_image_event_queues_preview_for_active_session() {
        let mut app = test_app();
        let sid = app.sessions.active_id();
        let path = PathBuf::from("aitui-images/generated.png");
        app.apply(Action::StreamImageReady(sid, path.clone()));
        assert_eq!(app.pending_image, Some(path));
    }

    #[test]
    fn bare_at_mention_with_many_paths_renders_in_a_short_terminal() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = test_app();
        app.vim = VimMode::Insert;
        app.input.set_text("@");
        app.input.col = 1;
        app.mention.active = true;
        app.mention.matches = (0..50).map(|i| format!("src/file_{i}.rs")).collect();

        let backend = TestBackend::new(60, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
    }

    #[tokio::test]
    async fn representative_tool_round_executes_and_records_the_result() {
        let mut app = test_app();
        let dir = std::env::temp_dir().join(format!(
            "aitui_tool_round_{}_{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.txt");
        std::fs::write(&path, "tool round works").unwrap();
        let sid = app.sessions.active_id();
        app.agent_session = Some(sid);
        app.sessions.active_mut().cwd = Some(dir.clone());
        app.permissions.remember_allow(crate::agent::ToolKind::Read);
        app.pending_tools
            .push_back(tool_call("read", serde_json::json!({"path": "sample.txt"})));

        let _ = app.process_next_tool();
        let result = app
            .agent_tool_rx
            .as_mut()
            .expect("tool result receiver")
            .recv()
            .await
            .expect("tool result");
        app.agent_tool_rx = None;
        let _ = app.apply(Action::AgentToolResult(result));

        assert!(tool_msg_text(&app).contains("tool round works"));
        assert!(app.active_tool.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Access-policy judging ───────────────────────────────────────────────────

    fn tool_call(name: &str, args: serde_json::Value) -> crate::agent::ToolCall {
        crate::agent::ToolCall {
            name: name.to_string(),
            args,
            id: None,
        }
    }

    #[test]
    fn selected_review_rule_cannot_classify_non_matching_calls() {
        use crate::agent::AccessVerdict;
        use crate::app::state::JudgeBatch;

        let mut app = test_app();
        let sid = app.sessions.active_id();
        let cwd = std::env::current_dir().unwrap();
        app.agent_session = Some(sid);
        app.judging = Some(JudgeBatch {
            session_id: sid,
            calls: vec![
                tool_call("read", serde_json::json!({ "path": "src/main.rs" })),
                tool_call("list", serde_json::json!({ "path": "src" })),
            ],
            reviewed_rule: Some(crate::agent::PermissionRuleDraft {
                decision: PermissionDecision::Allow,
                kind: Some(crate::agent::ToolKind::Read),
                directory: Some(cwd),
                include_children: true,
                lifetime: crate::agent::PermissionLifetime::Session,
            }),
        });

        app.apply(Action::AccessJudged(
            sid,
            vec![AccessVerdict::Allow, AccessVerdict::Allow],
        ));

        assert_eq!(app.approved.len(), 1);
        assert_eq!(app.approved[0].kind(), Some(crate::agent::ToolKind::Read));
        let Overlay::Permission(req) = &app.overlay else {
            panic!("non-matching list call must still require human review");
        };
        assert_eq!(req.calls.len(), 1);
        assert_eq!(req.calls[0].kind(), Some(crate::agent::ToolKind::List));
    }

    #[test]
    fn remembered_allow_cannot_bypass_the_hard_prompt_or_consume_its_limit() {
        let mut app = test_app();
        app.permissions
            .remember_custom_rule(crate::agent::PermissionRuleDraft {
                decision: PermissionDecision::Allow,
                kind: Some(crate::agent::ToolKind::Delete),
                directory: None,
                include_children: false,
                lifetime: crate::agent::PermissionLifetime::MatchingRequests(1),
            });
        app.pending_tools.push_back(tool_call(
            "delete",
            serde_json::json!({"path": "never-delete-this.txt"}),
        ));

        let _ = app.process_next_tool();

        assert!(matches!(app.overlay, Overlay::Permission(_)));
        assert_eq!(app.permissions.rules[0].remaining_matching, Some(1));
        assert!(app.active_tool.is_none());
    }

    #[test]
    fn remembered_deny_still_blocks_hard_floor_calls_without_prompting() {
        let mut app = test_app();
        app.permissions
            .remember_custom_rule(crate::agent::PermissionRuleDraft {
                decision: PermissionDecision::Deny,
                kind: Some(crate::agent::ToolKind::Delete),
                directory: None,
                include_children: false,
                lifetime: crate::agent::PermissionLifetime::MatchingRequests(1),
            });
        app.pending_tools.push_back(tool_call(
            "delete",
            serde_json::json!({"path": "never-delete-this.txt"}),
        ));

        let _ = app.process_next_tool();

        assert!(matches!(app.overlay, Overlay::None));
        assert!(app.permissions.rules.is_empty());
        assert!(tool_msg_text(&app).contains("denied by session policy"));
        assert!(app.active_tool.is_none());
    }

    #[test]
    fn permission_prompt_uses_the_agent_sessions_working_directory() {
        let mut app = test_app();
        let session_cwd = std::env::temp_dir().join("aitui-access-session-cwd");
        app.sessions.active_mut().cwd = Some(session_cwd.clone());
        app.pending_tools.push_back(tool_call(
            "write",
            serde_json::json!({"path": "src/new.rs", "content": "x"}),
        ));

        let _ = app.process_next_tool();

        let Overlay::Permission(req) = &app.overlay else {
            panic!("write must open the access request");
        };
        assert_eq!(req.cwd, session_cwd);
        let Permission::Custom(rule) = req.permission() else {
            panic!("custom rule");
        };
        assert_eq!(rule.directory, Some(req.cwd.join("src")));
    }

    #[test]
    fn verdicts_route_allow_deny_and_floor_overrides_to_ask() {
        use crate::agent::AccessVerdict;
        use crate::app::state::JudgeBatch;

        let mut app = test_app();
        let sid = app.sessions.active_id();
        app.agent_session = Some(sid);
        let calls = vec![
            tool_call("read", serde_json::json!({ "path": "a.rs" })),
            tool_call("list", serde_json::json!({ "path": "." })),
            tool_call("delete", serde_json::json!({ "path": "a.rs" })),
        ];
        app.judging = Some(JudgeBatch {
            session_id: sid,
            calls,
            reviewed_rule: None,
        });
        // Judge says allow all three — but delete is on the safety floor, so it must
        // be forced to a human prompt regardless.
        app.apply(Action::AccessJudged(
            sid,
            vec![
                AccessVerdict::Allow,
                AccessVerdict::Deny,
                AccessVerdict::Allow,
            ],
        ));

        // read → approved (queued to run), list → denied (skip result recorded),
        // delete → floored → the permission prompt is shown for it.
        assert_eq!(app.approved.len(), 1);
        assert_eq!(app.approved.front().unwrap().name, "read");
        assert!(matches!(app.overlay, Overlay::Permission(_)));
        if let Overlay::Permission(req) = &app.overlay {
            assert_eq!(req.calls.len(), 1);
            assert_eq!(req.calls[0].name, "delete");
        }
        assert!(app.judging.is_none());
    }

    #[test]
    fn stale_verdicts_for_wrong_session_are_ignored() {
        use crate::agent::AccessVerdict;
        use crate::app::state::JudgeBatch;

        let mut app = test_app();
        let sid = app.sessions.active_id();
        app.judging = Some(JudgeBatch {
            session_id: sid,
            calls: vec![tool_call("read", serde_json::json!({ "path": "a.rs" }))],
            reviewed_rule: None,
        });
        app.apply(Action::AccessJudged(sid + 999, vec![AccessVerdict::Allow]));
        // Wrong session id → the batch is dropped, nothing queued.
        assert!(app.approved.is_empty());
        assert!(app.judging.is_none());
    }

    #[test]
    fn selected_review_rule_is_remembered_after_a_confirming_verdict() {
        use crate::agent::AccessVerdict;
        use crate::app::state::JudgeBatch;

        let mut app = test_app();
        let sid = app.sessions.active_id();
        let rule = crate::agent::PermissionRuleDraft {
            decision: PermissionDecision::Allow,
            kind: Some(crate::agent::ToolKind::Read),
            directory: Some(std::env::current_dir().unwrap()),
            include_children: true,
            lifetime: crate::agent::PermissionLifetime::MatchingRequests(2),
        };
        app.agent_session = Some(sid);
        app.judging = Some(JudgeBatch {
            session_id: sid,
            calls: vec![
                tool_call("read", serde_json::json!({ "path": "README.md" })),
                tool_call("delete", serde_json::json!({ "path": "README.md" })),
            ],
            reviewed_rule: Some(rule),
        });
        app.apply(Action::AccessJudged(
            sid,
            vec![AccessVerdict::Allow, AccessVerdict::Allow],
        ));

        assert_eq!(app.permissions.rules.len(), 1);
        let remembered = &app.permissions.rules[0];
        assert_eq!(remembered.kind, Some(crate::agent::ToolKind::Read));
        assert!(remembered.include_children);
        assert_eq!(remembered.remaining_matching, Some(2));
    }

    #[test]
    fn access_manager_preserves_custom_rule_conditions_and_opens_details() {
        let mut app = test_app();
        let directory = std::env::current_dir().unwrap().join("src");
        app.permissions
            .remember_custom_rule(crate::agent::PermissionRuleDraft {
                decision: PermissionDecision::Allow,
                kind: Some(crate::agent::ToolKind::Read),
                directory: Some(directory.clone()),
                include_children: true,
                lifetime: crate::agent::PermissionLifetime::MatchingRequests(3),
            });

        let items = app.access_items();
        assert_eq!(items.len(), 2);
        assert!(items[0].contains("Review model"));
        assert!(items[1].contains("read"));
        assert!(items[1].contains("child directories"));
        assert!(items[1].contains("next 3 matching requests"));

        app.overlay = Overlay::Picker(Picker::access(items));
        if let Overlay::Picker(picker) = &mut app.overlay {
            picker.selected = 1;
        }
        app.apply(Action::PickerConfirm);
        let Overlay::Notice { title, body } = &app.overlay else {
            panic!("Enter should open detailed access information");
        };
        assert_eq!(title, "Access rule");
        assert!(body.contains("Decision: allow"));
        assert!(body.contains("Access type: read"));
        assert!(body.contains(&crate::render::path::display_path(&directory)));
        assert!(body.contains("Child directories: included"));
        assert!(body.contains("Lifetime: next 3 matching requests"));
    }

    #[test]
    fn edited_access_rule_replaces_the_active_session_entry_immediately() {
        let mut app = test_app();
        app.permissions.always_allow.push(ToolKind::Read);

        app.apply(Action::EditAccessEntry(0));
        let Overlay::Permission(request) = &mut app.overlay else {
            panic!("access entry should open in the structured editor");
        };
        request.decision = PermissionDecision::Deny;
        request.tool_index = ToolKind::all()
            .iter()
            .position(|kind| *kind == ToolKind::Write)
            .unwrap()
            + 1;
        request.location_index = 0;
        request.lifetime_index = 1;

        app.apply(Action::AgentResolvePermission);

        assert!(app.permissions.always_allow.is_empty());
        assert_eq!(app.permissions.rules.len(), 1);
        assert_eq!(app.permissions.rules[0].decision, PermissionDecision::Deny);
        assert_eq!(app.permissions.rules[0].kind, Some(ToolKind::Write));
        assert!(matches!(app.overlay, Overlay::Picker(_)));
    }

    #[test]
    fn custom_policy_text_is_visible_from_access_manager() {
        let mut app = test_app();
        app.permissions
            .set_policy("Allow tests, ask before writes outside src");
        assert!(app.access_items()[1].contains("custom review instructions"));
        let details = app.access_entry_details(1).unwrap();
        assert!(details.contains("Allow tests, ask before writes outside src"));
    }

    #[test]
    fn disabling_active_review_immediately_prompts_the_user() {
        use crate::app::state::JudgeBatch;

        let mut app = test_app();
        let sid = app.sessions.active_id();
        app.config.api.access_review_mode = crate::config::AccessReviewMode::Strict;
        app.agent_session = Some(sid);
        app.judging = Some(JudgeBatch {
            session_id: sid,
            calls: vec![tool_call(
                "read",
                serde_json::json!({ "path": "README.md" }),
            )],
            reviewed_rule: None,
        });

        app.apply(Action::DisableAccessReview);

        assert_eq!(
            app.config.api.access_review_mode,
            crate::config::AccessReviewMode::Off
        );
        assert!(app.judging.is_none());
        assert!(app.judge_rx.is_none());
        assert!(app.judge_task.is_none());
        let Overlay::Permission(request) = &app.overlay else {
            panic!("pending review batch should be shown directly to the user");
        };
        assert_eq!(request.calls.len(), 1);
        assert_eq!(request.calls[0].kind(), Some(ToolKind::Read));
    }

    #[test]
    fn review_permission_keeps_overlay_open_without_a_live_model() {
        let mut app = test_app();
        app.config.api.endpoint.clear();
        app.overlay = Overlay::Permission(crate::app::overlay::PermissionRequest::single(
            tool_call("read", serde_json::json!({ "path": "README.md" })),
        ));
        app.apply(Action::AgentReviewPermission);
        assert!(matches!(app.overlay, Overlay::Permission(_)));
        assert_eq!(
            app.status.as_deref(),
            Some("Automated review needs a configured live model")
        );
    }

    #[test]
    fn reasoning_commands_accept_free_form_values_and_clear_with_off() {
        let mut app = test_app();
        assert!(app
            .run_command("reasoning-effort provider-specific turbo plus")
            .is_none());
        assert_eq!(
            app.reasoning_effort.as_deref(),
            Some("provider-specific turbo plus")
        );
        assert_eq!(
            app.config.api.reasoning_effort,
            "provider-specific turbo plus"
        );

        assert!(app
            .run_command("reasoning-mode interleaved-thinking/custom")
            .is_none());
        assert_eq!(
            app.reasoning_mode.as_deref(),
            Some("interleaved-thinking/custom")
        );
        assert_eq!(app.config.api.reasoning_mode, "interleaved-thinking/custom");

        assert!(app.run_command("effort off").is_none());
        assert!(app.reasoning_effort.is_none());
        assert!(app.config.api.reasoning_effort.is_empty());
    }

    #[test]
    fn access_review_mode_commands_are_typed_and_persist_on_apply() {
        let mut app = test_app();
        let action = app.run_command("review lenient");
        assert!(matches!(
            action,
            Some(Action::SetAccessReviewMode(
                crate::config::AccessReviewMode::Lenient
            ))
        ));
        app.apply(action.unwrap());
        assert_eq!(
            app.config.api.access_review_mode,
            crate::config::AccessReviewMode::Lenient
        );

        let action = app.run_command("review off").unwrap();
        app.apply(action);
        assert_eq!(
            app.config.api.access_review_mode,
            crate::config::AccessReviewMode::Off
        );
        assert!(app.access_review_policy().is_none());
    }

    #[test]
    fn custom_access_policy_overrides_enabled_baseline_but_not_off_mode() {
        let mut app = test_app();
        app.config.api.access_review_mode = crate::config::AccessReviewMode::Strict;
        app.permissions.set_policy("allow cargo test");
        assert_eq!(
            app.access_review_policy().as_deref(),
            Some("allow cargo test")
        );
        app.config.api.access_review_mode = crate::config::AccessReviewMode::Off;
        assert!(app.access_review_policy().is_none());
    }

    #[test]
    fn parse_loop_spec_reads_fields_and_defaults() {
        let (g, s, m) = parse_loop_spec(
            "# comment\nGOAL: make tests pass\nSTOP: cargo test is green\nMAX: 8\n",
        );
        assert_eq!(g, "make tests pass");
        assert_eq!(s, "cargo test is green");
        assert_eq!(m, 8);
        // Missing MAX → default; blank goal stays blank.
        let (g2, _, m2) = parse_loop_spec("GOAL: x\n");
        assert_eq!(g2, "x");
        assert_eq!(m2, LoopState::DEFAULT_MAX);
    }

    #[test]
    fn start_loop_sets_state_and_seeds_goal() {
        let mut app = test_app();
        app.apply(Action::StartLoop("build the thing".into()));
        let s = app.sessions.active();
        assert!(s.loop_state.is_some());
        assert!(s.agent_mode, "loop turns agent mode on");
        assert_eq!(s.loop_state.as_ref().unwrap().goal, "build the thing");
        // The goal was seeded as the first user turn.
        assert!(s.messages.iter().any(|m| matches!(&m.content,
                crate::api::models::MessageContent::Text(t) if t.contains("build the thing"))));
    }

    #[test]
    fn stop_loop_clears_state() {
        let mut app = test_app();
        app.apply(Action::StartLoop("x".into()));
        assert!(app.sessions.active().loop_state.is_some());
        app.apply(Action::StopLoop);
        assert!(app.sessions.active().loop_state.is_none());
    }

    #[test]
    fn set_policy_then_clear_toggles_memory() {
        let mut app = test_app();
        app.apply(Action::SetAccessPolicy("allow all reads".into()));
        assert_eq!(app.permissions.policy.as_deref(), Some("allow all reads"));
        app.apply(Action::SetAccessPolicy(String::new()));
        assert!(app.permissions.policy.is_none());
    }

    // ── Mode transitions ───────────────────────────────────────────────────────

    #[test]
    fn enter_insert_sets_vim_mode_and_focus() {
        let mut app = test_app();
        app.apply(Action::EnterInsert);
        assert_eq!(app.vim, VimMode::Insert);
    }

    #[test]
    fn enter_normal_clears_command_and_mention() {
        let mut app = test_app();
        app.command = "test".into();
        app.vim = VimMode::Insert;
        app.input.paste("hello");
        app.input.col = 3;
        app.apply(Action::EnterNormal);
        assert_eq!(app.vim, VimMode::Normal);
        assert!(app.command.is_empty());
        assert!(!app.mention.active);
    }

    #[test]
    fn enter_visual_sets_vim_mode() {
        let mut app = test_app();
        app.apply(Action::EnterVisual);
        assert_eq!(app.vim, VimMode::Visual);
    }

    #[test]
    fn open_command_palette_opens_overlay_without_mode_switch() {
        let mut app = test_app();
        app.command = "old".into();
        app.apply(Action::OpenCommandPalette);
        assert_eq!(app.vim, VimMode::Normal);
        assert!(matches!(app.overlay, Overlay::Palette(_)));
    }

    #[test]
    fn enter_operator_sets_pending_operator() {
        let mut app = test_app();
        app.apply(Action::EnterOperator('d'));
        assert_eq!(app.vim, VimMode::Operator('d'));
    }

    // ── Input editing ──────────────────────────────────────────────────────────

    #[test]
    fn insert_char_appends_to_input() {
        let mut app = test_app();
        app.apply(Action::InsertChar('h'));
        app.apply(Action::InsertChar('i'));
        assert_eq!(app.input.text(), "hi");
    }

    #[test]
    fn insert_char_always_appends_to_input() {
        let mut app = test_app();
        app.apply(Action::InsertChar('w'));
        assert_eq!(app.input.text(), "w");
        assert!(app.command.is_empty());
    }

    #[test]
    fn newline_inserts_break() {
        let mut app = test_app();
        app.input.paste("ab");
        app.input.col = 1;
        app.apply(Action::Newline);
        assert_eq!(app.input.lines, vec![String::from("a"), String::from("b")]);
    }

    #[test]
    fn backspace_removes_char() {
        let mut app = test_app();
        app.input.paste("abc");
        app.input.col = 3;
        app.apply(Action::Backspace);
        assert_eq!(app.input.text(), "ab");
    }

    #[test]
    fn backspace_always_edits_input() {
        let mut app = test_app();
        app.input.paste("wr");
        app.input.col = 2;
        app.apply(Action::Backspace);
        assert_eq!(app.input.text(), "w");
    }

    #[test]
    fn delete_at_removes_char_under_cursor() {
        let mut app = test_app();
        app.input.paste("abcd");
        app.input.col = 1;
        app.apply(Action::DeleteAt);
        assert_eq!(app.input.text(), "acd");
    }

    #[test]
    fn delete_line_removes_current_line() {
        let mut app = test_app();
        app.input.paste("line1");
        app.apply(Action::Newline);
        app.input.paste("line2");
        app.input.row = 0;
        app.apply(Action::DeleteLine);
        assert_eq!(app.input.text(), "line2");
    }

    #[test]
    fn yank_line_copies_text() {
        let mut app = test_app();
        app.input.paste("yank me");
        app.apply(Action::YankLine);
        assert_eq!(app.yank.as_deref(), Some("yank me"));
        assert!(app.status.is_none());
    }

    #[test]
    fn paste_inserts_yanked_text() {
        let mut app = test_app();
        app.input.paste("hello");
        app.yank = Some(" world".into());
        app.input.col = 5;
        app.apply(Action::Paste);
        assert_eq!(app.input.text(), "hello world");
    }

    #[test]
    fn undo_and_redo_restore_input_states() {
        let mut app = test_app();
        app.apply(Action::InsertChar('a'));
        app.apply(Action::InsertChar('b'));
        assert_eq!(app.input.text(), "ab");
        app.apply(Action::UndoInput);
        assert_eq!(app.input.text(), "a");
        app.apply(Action::RedoInput);
        assert_eq!(app.input.text(), "ab");
    }

    #[test]
    fn vim_delete_and_change_word_motions() {
        let mut app = test_app();
        app.input.set_text("hello world");
        app.input.row = 0;
        app.input.col = 0;
        app.apply(Action::DeleteTo(Dir::WordForward));
        assert_eq!(app.input.text(), "world");
        assert_eq!(app.yank.as_deref(), Some("hello "));
        app.input.set_text("hello world");
        app.input.row = 0;
        app.input.col = 0;
        app.apply(Action::ChangeTo(Dir::WordForward));
        assert_eq!(app.input.text(), "world");
        assert_eq!(app.vim, VimMode::Insert);
    }

    #[test]
    fn vim_change_word_deletes_a_single_character() {
        let mut app = test_app();
        app.input.set_text("a");
        app.input.row = 0;
        app.input.col = 0;
        app.apply(Action::ChangeTo(Dir::WordEnd));
        assert_eq!(app.input.text(), "");
        assert_eq!(app.yank.as_deref(), Some("a"));
        assert_eq!(app.vim, VimMode::Insert);
        assert_eq!(app.input.cursor(), (0, 0));
    }

    #[test]
    fn vim_delete_and_change_line_commands() {
        let mut app = test_app();
        app.input.set_text("one\ntwo");
        app.input.row = 0;
        app.apply(Action::DeleteLine);
        assert_eq!(app.input.text(), "two");
        assert_eq!(app.yank.as_deref(), Some("one"));
        app.input.set_text("replace me");
        app.apply(Action::ChangeLine);
        assert_eq!(app.input.text(), "");
        assert_eq!(app.vim, VimMode::Insert);
    }

    #[test]
    fn vim_open_line_commands_enter_insert() {
        let mut app = test_app();
        app.input.set_text("one");
        app.apply(Action::OpenLineBelow);
        assert_eq!(app.input.text(), "one\n");
        assert_eq!((app.input.row, app.input.col), (1, 0));
        assert_eq!(app.vim, VimMode::Insert);
        app.apply(Action::EnterNormal);
        app.apply(Action::OpenLineAbove);
        assert_eq!(app.input.text(), "one\n\n");
        assert_eq!((app.input.row, app.input.col), (1, 0));
    }

    #[test]
    fn medium_paste_chips_and_expands_on_send() {
        let mut app = test_app();
        app.vim = VimMode::Insert;
        let blob = (0..10)
            .map(|i| format!("line{}", i))
            .collect::<Vec<_>>()
            .join("\n");
        app.apply(Action::PasteText(blob.clone()));
        // The composer shows a compact chip, not the raw blob.
        assert!(app.input.text().contains("[PASTED#1-10lines-"));
        assert!(!app.input.text().contains("line5"));
        assert_eq!(app.pastes.len(), 1);
        // Sending expands the chip back to the full text and clears the store.
        let _ = app.submit();
        let sent = app
            .sessions
            .active()
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .unwrap();
        let text = match &sent.content {
            crate::api::models::MessageContent::Text(t) => t.clone(),
            _ => String::new(),
        };
        assert!(
            text.contains("line5"),
            "full pasted text must be restored on send"
        );
        assert!(app.pastes.is_empty());
    }

    #[test]
    fn small_paste_inserted_verbatim() {
        let mut app = test_app();
        app.vim = VimMode::Insert;
        app.apply(Action::PasteText("hello world".into()));
        assert_eq!(app.input.text(), "hello world");
        assert!(app.pastes.is_empty(), "small pastes don't create chips");
    }

    #[test]
    fn move_directions_update_cursor() {
        let mut app = test_app();
        app.input.paste("hello\nworld");
        app.apply(Action::Move(Dir::Up));
        assert_eq!(app.input.row, 0);
        app.apply(Action::Move(Dir::Down));
        assert_eq!(app.input.row, 1);
    }

    #[test]
    fn line_start_and_end() {
        let mut app = test_app();
        app.input.paste("hello world");
        app.input.col = 5;
        app.apply(Action::LineStart);
        assert_eq!(app.input.col, 0);
        app.apply(Action::LineEnd);
        assert_eq!(app.input.col, 10);
    }

    // ── Skills ─────────────────────────────────────────────────────────────────

    #[test]
    fn toggle_skill_flips_active_and_refreshes_picker() {
        let mut app = test_app();
        app.config.ui.sticky_skills = false; // don't touch disk in tests
        app.skills = vec![crate::skills::Skill {
            name: "alpha".into(),
            desc: "terse".into(),
            body: "be terse".into(),
            active: false,
        }];
        app.overlay = Overlay::Picker(Picker::skills(app.skill_items()));
        app.apply(Action::ToggleSkill(0));
        assert!(app.skills[0].active);
        // Picker stays open with a ✓ marker after toggling.
        if let Overlay::Picker(p) = &app.overlay {
            assert!(p.items[0].starts_with('✓'));
        } else {
            panic!("skill picker should stay open");
        }
        app.apply(Action::ToggleSkill(0));
        assert!(!app.skills[0].active);
    }

    #[test]
    fn empty_skills_opens_no_picker() {
        let base =
            std::env::temp_dir().join(format!("aitui_empty_skills_{}_reducer", std::process::id()));
        std::fs::create_dir_all(base.join("aitui").join("skills")).unwrap();

        let mut app = test_app();
        crate::skills::with_test_config_base(base.clone(), || {
            app.skills.clear();
            app.apply(Action::OpenSkillPicker);
        });
        let _ = std::fs::remove_dir_all(&base);

        assert!(matches!(app.overlay, Overlay::None));
    }

    // ── Sessions ───────────────────────────────────────────────────────────────

    #[test]
    fn new_session_creates_and_switches() {
        let mut app = test_app();
        assert_eq!(app.sessions.all().len(), 1);
        app.apply(Action::NewSession);
        assert_eq!(app.sessions.all().len(), 2);
        assert_eq!(app.sessions.active_idx(), 1);
    }

    #[test]
    fn new_session_always_has_agent_on() {
        let mut app = test_app();
        app.apply(Action::NewSession);
        assert!(app.sessions.active().agent_mode);
    }

    #[test]
    fn delete_session_removes_or_resets() {
        let mut app = test_app();
        app.apply(Action::DeleteSession);
        assert_eq!(app.sessions.all().len(), 1); // resets, doesn't remove last
    }

    #[test]
    fn next_session_cycles_forward() {
        let mut app = test_app();
        app.apply(Action::NewSession);
        app.apply(Action::PrevSession);
        assert_eq!(app.sessions.active_idx(), 0);
        app.apply(Action::NextSession);
        assert_eq!(app.sessions.active_idx(), 1);
    }

    #[test]
    fn session_tab_click_routes_to_session_selection() {
        let mut app = test_app();
        app.apply(Action::NewSession);
        app.apply(Action::PrevSession);
        app.layout.session_tabs = vec![crate::app::state::SessionTabHitbox {
            session_idx: 1,
            area: ratatui::layout::Rect::new(8, 0, 12, 1),
        }];

        let follow = app.apply(Action::ChatClick(10, 0));
        assert!(matches!(follow, Some(Action::SelectSession(1))));
    }

    #[test]
    fn access_chip_click_opens_access_manager() {
        let mut app = test_app();
        app.layout.access = Some(crate::app::state::AccessHitbox {
            index: 0,
            area: ratatui::layout::Rect::new(70, 2, 18, 1),
        });

        let follow = app.apply(Action::ChatClick(75, 2));
        assert!(matches!(follow, Some(Action::OpenAccessManager)));
    }

    #[test]
    fn sidebar_access_row_click_edits_entry() {
        let mut app = test_app();
        app.layout.access_rows = vec![crate::app::state::AccessHitbox {
            index: 2,
            area: ratatui::layout::Rect::new(2, 6, 20, 1),
        }];

        let follow = app.apply(Action::ChatClick(5, 6));
        assert!(matches!(follow, Some(Action::EditAccessEntry(2))));
    }

    #[test]
    fn sidebar_access_empty_row_click_opens_manager() {
        let mut app = test_app();
        app.layout.access_rows = vec![crate::app::state::AccessHitbox {
            index: usize::MAX,
            area: ratatui::layout::Rect::new(2, 6, 20, 1),
        }];

        let follow = app.apply(Action::ChatClick(5, 6));
        assert!(matches!(follow, Some(Action::OpenAccessManager)));
    }

    #[test]
    fn prompt_preview_click_toggles_expansion() {
        let mut app = test_app();
        app.layout.prompt = Some(crate::app::state::PromptHitbox {
            area: ratatui::layout::Rect::new(0, 2, 80, 1),
            msg: None,
        });
        assert!(!app.show_last_prompt);

        app.apply(Action::ChatClick(12, 2));
        assert!(app.show_last_prompt);
        app.apply(Action::ChatClick(12, 2));
        assert!(!app.show_last_prompt);
    }

    #[test]
    fn goto_click_does_not_toggle_last_prompt() {
        let mut app = test_app();
        app.show_last_prompt = true;
        app.layout.prompt = Some(crate::app::state::PromptHitbox {
            area: ratatui::layout::Rect::new(0, 2, 74, 1),
            msg: None,
        });
        app.layout.prompt_goto = Some(crate::app::state::PromptHitbox {
            area: ratatui::layout::Rect::new(74, 2, 6, 1),
            msg: None,
        });

        app.apply(Action::ChatClick(76, 2));

        assert!(app.show_last_prompt);
    }

    #[test]
    fn stream_usage_is_kept_per_session() {
        let mut app = test_app();
        let first = app.sessions.active_id();
        app.apply(Action::NewSession);
        let second = app.sessions.active_id();
        let usage = crate::api::Usage {
            prompt_tokens: 100,
            completion_tokens: 25,
            total_tokens: 125,
        };

        app.apply(Action::StreamUsage(first, usage));

        assert_eq!(
            app.session_usage.get(&first).map(|u| u.total_tokens),
            Some(125)
        );
        assert!(!app.session_usage.contains_key(&second));
    }

    // ── Sessions ─────────────────────────────────────────────────────────────────

    #[test]
    fn open_session_picker_sets_overlay() {
        let mut app = test_app();
        app.apply(Action::OpenSessionPicker);
        assert!(matches!(app.overlay, Overlay::Picker(_)));
    }

    #[test]
    fn session_picker_items_include_cwd_prompt_time_and_remote_state() {
        let mut app = test_app();
        let session_id = app.sessions.active_id();
        app.sessions
            .active_mut()
            .push_message(crate::api::ChatMessage::user("hello"));
        app.remote_running_sessions.insert(session_id);

        let items = app.session_items();

        assert!(items[1].contains("RUNNING elsewhere"));
        assert!(items[1].contains("last just now"));
        assert!(items[1].contains("cwd "));
        assert!(items[1].contains("1 msg"));
    }

    #[test]
    fn open_editor_sets_request() {
        let mut app = test_app();
        app.sessions
            .active_mut()
            .push_message(crate::api::ChatMessage::user("hi there"));
        app.apply(Action::OpenEditor);
        match app.pending_external {
            Some(crate::app::state::PendingExternal::EditorText(ref t)) => {
                assert!(t.contains("hi there"))
            }
            _ => panic!("expected EditorText request"),
        }
    }

    #[test]
    fn open_files_in_editor_sets_external() {
        let mut app = test_app();
        app.apply(Action::OpenFilesInEditor(vec![std::path::PathBuf::from(
            "src/main.rs",
        )]));
        assert!(matches!(
            app.pending_external,
            Some(crate::app::state::PendingExternal::EditorFiles(_))
        ));
    }

    #[test]
    fn open_shell_sets_external() {
        let mut app = test_app();
        app.apply(Action::OpenShell);
        assert!(matches!(
            app.pending_external,
            Some(crate::app::state::PendingExternal::Shell)
        ));
    }

    #[test]
    fn open_edit_picker_opens_browser() {
        use crate::app::overlay::BrowsePurpose;
        let mut app = test_app();
        app.apply(Action::OpenEditPicker);
        match &app.overlay {
            Overlay::Browser(b) => assert_eq!(b.purpose, BrowsePurpose::Edit),
            _ => panic!("expected a file browser"),
        }
    }

    #[test]
    fn open_edit_picker_toggles_closed() {
        let mut app = test_app();
        app.apply(Action::OpenEditPicker);
        assert!(matches!(app.overlay, Overlay::Browser(_)));
        app.apply(Action::OpenEditPicker);
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[test]
    fn successful_write_tracks_edited_file() {
        use crate::agent::{ToolCall, ToolResult};
        let mut app = test_app();
        let call = ToolCall {
            name: "write_file".into(),
            args: serde_json::json!({"path": "./src/x.rs"}),
            id: None,
        };
        app.apply(Action::AgentToolResult(ToolResult::success(
            call,
            "ok".into(),
            1,
        )));
        assert_eq!(app.edited_files, vec!["src/x.rs".to_string()]);
    }

    #[test]
    fn delete_removes_from_edited_files() {
        use crate::agent::{ToolCall, ToolResult};
        let mut app = test_app();
        app.edited_files = vec!["src/x.rs".into()];
        let call = ToolCall {
            name: "delete_file".into(),
            args: serde_json::json!({"path": "src/x.rs"}),
            id: None,
        };
        app.apply(Action::AgentToolResult(ToolResult::success(
            call,
            "deleted".into(),
            1,
        )));
        assert!(app.edited_files.is_empty());
    }

    #[test]
    fn submit_blocked_while_busy_keeps_input_and_shows_notice() {
        let mut app = test_app();
        app.input.set_text("hello");
        // Simulate an in-flight stream for the active session → busy.
        let sid = app.sessions.active_id();
        app.streams.push(crate::app::state::StreamHandle {
            session_id: sid,
            rx: tokio::sync::mpsc::channel(1).1,
            cold_retries: 0,
        });
        assert!(app.is_busy());
        let out = app.submit();
        assert!(out.is_none(), "must not start a new stream while busy");
        assert!(
            matches!(app.overlay, Overlay::Notice { .. }),
            "a busy notice should show"
        );
        assert_eq!(
            app.input.take(),
            "hello",
            "the composed text must be preserved"
        );
    }

    #[test]
    fn submit_when_idle_sends() {
        let mut app = test_app();
        app.input.set_text("hi there");
        assert!(!app.is_busy());
        let _ = app.submit();
        // The user message was pushed (a real stream would attach in the app; the
        // test harness has no API client so the turn finalizes immediately).
        assert!(app
            .sessions
            .active()
            .messages
            .iter()
            .any(|m| m.role == "user"));
        assert!(
            !matches!(app.overlay, Overlay::Notice { .. }),
            "idle send must not show the busy notice"
        );
    }

    #[test]
    fn copy_last_reply_queues_clipboard() {
        let mut app = test_app();
        app.sessions
            .active_mut()
            .push_message(crate::api::ChatMessage::user("q"));
        app.sessions
            .active_mut()
            .push_message(crate::api::ChatMessage::assistant("the answer"));
        app.apply(Action::CopyLastReply);
        assert_eq!(app.pending_clipboard.as_deref(), Some("the answer"));
    }

    #[test]
    fn copy_last_code_extracts_fenced_block() {
        let mut app = test_app();
        app.sessions
            .active_mut()
            .push_message(crate::api::ChatMessage::assistant(
                "here:\n```rust\nfn f() {}\n```\ndone",
            ));
        app.apply(Action::CopyLastCode);
        assert_eq!(app.pending_clipboard.as_deref(), Some("fn f() {}"));
    }

    #[test]
    fn retry_command_trims_reply_and_resends() {
        let mut app = test_app();
        app.sessions
            .active_mut()
            .push_message(crate::api::ChatMessage::user("hello"));
        app.sessions
            .active_mut()
            .push_message(crate::api::ChatMessage::assistant("old reply"));
        // `:retry` maps to a follow-up Action::RetryLast; the main loop re-applies
        // returned actions, so chain it here too.
        if let Some(follow) = app.apply(Action::RunCommand("retry".into())) {
            let _ = app.apply(follow);
        }
        // The stale reply is gone and the user message remains for the resend.
        // (A fresh turn may append a new assistant message in the API-less harness.)
        assert!(app
            .sessions
            .active()
            .messages
            .iter()
            .any(|m| m.role == "user"));
        assert_ne!(
            app.sessions.active().last_assistant_text().as_deref(),
            Some("old reply")
        );
    }

    #[test]
    fn edit_last_pulls_message_into_composer() {
        let mut app = test_app();
        app.sessions
            .active_mut()
            .push_message(crate::api::ChatMessage::user("draft text"));
        app.sessions
            .active_mut()
            .push_message(crate::api::ChatMessage::assistant("reply"));
        app.apply(Action::EditLast);
        assert_eq!(app.input.text(), "draft text");
        assert_eq!(app.vim, VimMode::Insert);
        assert!(app.sessions.active().messages.is_empty());
    }

    #[test]
    fn vim_yank_mirrors_to_system_clipboard() {
        let mut app = test_app();
        app.input.set_text("copy this line");
        app.apply(Action::YankLine);
        assert_eq!(app.yank.as_deref(), Some("copy this line"));
        assert_eq!(app.pending_clipboard.as_deref(), Some("copy this line"));
    }

    fn tool_msg_text(app: &App) -> String {
        use crate::api::models::MessageContent;
        match &app.sessions.active().messages.last().unwrap().content {
            MessageContent::Text(t) => t.clone(),
            _ => String::new(),
        }
    }

    #[test]
    fn subtask_barrier_waits_for_every_child_and_records_launch_order() {
        use crate::app::state::{Subtask, SubtaskEvent, SubtaskStatus, TaskBarrier};

        let mut app = test_app();
        let sid = app.sessions.active_id();
        app.agent_session = Some(sid);
        let first_call = tool_call(
            "workflow",
            serde_json::json!({"action": "task", "description": "first", "prompt": "one"}),
        );
        let second_call = tool_call(
            "workflow",
            serde_json::json!({"action": "task", "description": "second", "prompt": "two"}),
        );
        app.sessions
            .active_mut()
            .push_message(crate::api::ChatMessage::tool(
                "[tool-result:agent] agent 1 (ok)\n[agent-id:1]\n[running]\nfirst",
            ));
        app.sessions
            .active_mut()
            .push_message(crate::api::ChatMessage::tool(
                "[tool-result:agent] agent 2 (ok)\n[agent-id:2]\n[running]\nsecond",
            ));
        let make_task = |id, call, description: &str| Subtask {
            id,
            session_id: sid,
            parent_id: None,
            call,
            description: description.into(),
            todo_index: None,
            prompt: description.into(),
            cwd: PathBuf::from("."),
            status: SubtaskStatus::Running,
            activity: None,
            log: Vec::new(),
            transcript: Vec::new(),
            output: None,
            message_index: id as usize - 1,
            started_at: std::time::Instant::now(),
            duration_ms: None,
            abort: None,
            agent: None,
        };
        app.subtasks = vec![
            make_task(1, first_call, "first"),
            make_task(2, second_call, "second"),
        ];
        // A stale cached index must recover by the hidden agent identity marker.
        app.subtasks[1].message_index = 0;
        app.task_barrier = Some(TaskBarrier {
            session_id: sid,
            task_ids: vec![1, 2],
        });

        assert!(app
            .handle_subtask_event(SubtaskEvent::Finished {
                id: 2,
                output: Ok("second report".into()),
                duration_ms: 2,
            })
            .is_none());
        assert!(app.task_barrier.is_some());
        assert!(app.is_busy());
        let early = app
            .sessions
            .active()
            .messages
            .iter()
            .map(|message| match &message.content {
                crate::api::models::MessageContent::Text(text) => text.clone(),
                _ => String::new(),
            })
            .collect::<Vec<_>>();
        assert!(early[0].contains("running"));
        assert!(early[1].contains("second report"));

        let _ = app.handle_subtask_event(SubtaskEvent::Finished {
            id: 1,
            output: Ok("first report".into()),
            duration_ms: 1,
        });
        assert!(app.task_barrier.is_none());
        let reports: Vec<_> = app
            .sessions
            .active()
            .messages
            .iter()
            .filter(|message| message.role == "tool")
            .map(|message| match &message.content {
                crate::api::models::MessageContent::Text(text) => text.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(reports.len(), 2);
        assert!(reports[0].contains("first report"));
        assert!(reports[1].contains("second report"));
    }

    #[test]
    fn clicking_subtask_row_opens_live_detail_overlay() {
        use crate::app::state::{Subtask, SubtaskHitbox, SubtaskStatus};
        use ratatui::layout::Rect;

        let mut app = test_app();
        let sid = app.sessions.active_id();
        app.subtasks.push(Subtask {
            id: 7,
            session_id: sid,
            parent_id: None,
            call: tool_call(
                "workflow",
                serde_json::json!({"action": "task", "description": "inspect", "prompt": "check"}),
            ),
            description: "inspect".into(),
            todo_index: Some(1),
            prompt: "check".into(),
            cwd: PathBuf::from("."),
            status: SubtaskStatus::Running,
            activity: Some("searching files".into()),
            log: vec![crate::app::state::SubtaskLogEntry::Phase {
                text: "searching files".into(),
            }],
            transcript: Vec::new(),
            output: None,
            message_index: usize::MAX,
            started_at: std::time::Instant::now(),
            duration_ms: None,
            abort: None,
            agent: None,
        });
        app.layout.panel_agents = vec![SubtaskHitbox {
            task_id: 7,
            area: Rect::new(2, 12, 60, 1),
        }];

        app.apply(Action::ChatClick(5, 12));
        assert!(matches!(
            app.overlay,
            Overlay::SubtaskDetail {
                task_id: 7,
                scroll: 0
            }
        ));
    }

    #[test]
    fn registered_nested_agent_creates_a_tree_node_under_its_parent() {
        use crate::agent::ToolCall;
        use crate::app::state::{SubtaskEvent, SubtaskStatus};

        let mut app = test_app();
        let sid = app.sessions.active_id();
        app.subtasks.push(Subtask {
            id: 1,
            session_id: sid,
            parent_id: None,
            call: tool_call(
                "workflow",
                serde_json::json!({"action": "task", "description": "parent", "prompt": "check"}),
            ),
            description: "parent".into(),
            todo_index: None,
            prompt: "check".into(),
            cwd: PathBuf::from("."),
            status: SubtaskStatus::Running,
            activity: Some("waiting on child".into()),
            log: Vec::new(),
            transcript: Vec::new(),
            output: None,
            message_index: usize::MAX,
            started_at: std::time::Instant::now(),
            duration_ms: None,
            abort: None,
            agent: None,
        });
        let call = ToolCall {
            name: "agent".into(),
            args: serde_json::json!({"description": "digest docs", "prompt": "Summarize."}),
            id: None,
        };
        let event = SubtaskEvent::Registered {
            id: 5,
            parent_id: 1,
            call: call.clone(),
            description: "digest docs".into(),
            prompt: "Summarize.".into(),
            agent: None,
            cwd: PathBuf::from("."),
        };
        assert!(app.handle_subtask_event(event).is_none());
        let child = app
            .subtasks
            .iter()
            .find(|task| task.id == 5)
            .expect("nested node registered");
        assert_eq!(child.parent_id, Some(1));
        assert_eq!(child.session_id, sid);
        assert_eq!(child.status, SubtaskStatus::Running);
        assert_eq!(child.description, "digest docs");
        assert_eq!(
            child.call.args.get("agent_index").and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(app.subtasks.iter().position(|t| t.id == 5), Some(1));
    }

    #[test]
    fn round_events_append_transcript_in_order() {
        use crate::app::state::{SubtaskEvent, SubtaskRoundRole, SubtaskStatus};

        let mut app = test_app();
        let sid = app.sessions.active_id();
        app.subtasks.push(Subtask {
            id: 2,
            session_id: sid,
            parent_id: None,
            call: tool_call(
                "workflow",
                serde_json::json!({"action": "task", "description": "x", "prompt": "check"}),
            ),
            description: "x".into(),
            todo_index: None,
            prompt: "check".into(),
            cwd: PathBuf::from("."),
            status: SubtaskStatus::Running,
            activity: None,
            log: Vec::new(),
            transcript: Vec::new(),
            output: None,
            message_index: usize::MAX,
            started_at: std::time::Instant::now(),
            duration_ms: None,
            abort: None,
            agent: None,
        });
        app.handle_subtask_event(SubtaskEvent::Round {
            id: 2,
            role: SubtaskRoundRole::Assistant,
            content: "Scanning the tree.".into(),
        });
        app.handle_subtask_event(SubtaskEvent::Round {
            id: 2,
            role: SubtaskRoundRole::ToolCall,
            content: "read(src/main.rs)".into(),
        });
        app.handle_subtask_event(SubtaskEvent::Round {
            id: 2,
            role: SubtaskRoundRole::ToolResult,
            content: "ok".into(),
        });
        let transcript = &app.subtasks[0].transcript;
        assert_eq!(transcript.len(), 3);
        assert_eq!(transcript[0].role, SubtaskRoundRole::Assistant);
        assert_eq!(transcript[0].content, "Scanning the tree.");
        assert_eq!(transcript[1].role, SubtaskRoundRole::ToolCall);
        assert_eq!(transcript[2].role, SubtaskRoundRole::ToolResult);
    }

    #[test]
    fn nested_finished_marks_node_complete_without_a_barrier() {
        use crate::app::state::{SubtaskEvent, SubtaskStatus};

        let mut app = test_app();
        let sid = app.sessions.active_id();
        app.subtasks.push(Subtask {
            id: 4,
            session_id: sid,
            parent_id: Some(1),
            call: tool_call(
                "workflow",
                serde_json::json!({"action": "task", "description": "x", "prompt": "check"}),
            ),
            description: "x".into(),
            todo_index: None,
            prompt: "check".into(),
            cwd: PathBuf::from("."),
            status: SubtaskStatus::Running,
            activity: Some("working".into()),
            log: Vec::new(),
            transcript: Vec::new(),
            output: None,
            message_index: usize::MAX,
            started_at: std::time::Instant::now(),
            duration_ms: None,
            abort: None,
            agent: None,
        });
        assert!(app.task_barrier.is_none());
        assert!(app
            .handle_subtask_event(SubtaskEvent::Finished {
                id: 4,
                output: Ok("all done".into()),
                duration_ms: 500,
            })
            .is_none());
        assert!(app.task_barrier.is_none());
        let task = app.subtasks.iter().find(|t| t.id == 4).unwrap();
        assert_eq!(task.status, SubtaskStatus::Completed);
        assert_eq!(task.output.as_deref(), Some("all done"));
        assert_eq!(task.duration_ms, Some(500));
        assert_eq!(task.activity, None);
    }

    #[test]
    fn deferred_children_gate_the_handoff_until_all_complete() {
        use crate::app::state::{Subtask, SubtaskStatus, TaskBarrier};

        let mut app = test_app();
        let sid = app.sessions.active_id();
        app.sessions
            .active_mut()
            .push_message(crate::api::ChatMessage::assistant(
                "Delegated the audit; finishing my own part now.",
            ));
        app.subtasks.push(Subtask {
            id: 9,
            session_id: sid,
            parent_id: None,
            call: tool_call(
                "workflow",
                serde_json::json!({"action": "agent", "description": "x", "prompt": "p"}),
            ),
            description: "x".into(),
            todo_index: None,
            prompt: "p".into(),
            cwd: PathBuf::from("."),
            status: SubtaskStatus::Running,
            activity: None,
            log: Vec::new(),
            transcript: Vec::new(),
            output: None,
            message_index: usize::MAX,
            started_at: std::time::Instant::now(),
            duration_ms: None,
            abort: None,
            agent: None,
        });
        app.task_barrier = Some(TaskBarrier {
            session_id: sid,
            task_ids: vec![9],
        });

        // The round ended with no tool calls: the turn must NOT be handed off
        // while a delegated child is still working.
        assert!(app.maybe_start_agent_round(sid).is_none());
        assert!(app.task_barrier.is_some(), "gate holds the deferred batch");
        assert!(
            app.sessions
                .active()
                .messages
                .iter()
                .all(|m| m.role != "user"),
            "no continuation prompt injected while a child still runs"
        );
        assert!(app.status.is_some());
    }

    #[tokio::test]
    async fn children_completing_after_the_round_ends_inject_reports_and_synthesize() {
        use crate::app::state::{Subtask, SubtaskEvent, SubtaskStatus, TaskBarrier};

        let mut app = test_app();
        let sid = app.sessions.active_id();
        app.select_mock_model();
        app.subtasks.push(Subtask {
            id: 9,
            session_id: sid,
            parent_id: None,
            call: tool_call(
                "workflow",
                serde_json::json!({"action": "agent", "description": "audit", "prompt": "p"}),
            ),
            description: "audit".into(),
            todo_index: None,
            prompt: "p".into(),
            cwd: PathBuf::from("."),
            status: SubtaskStatus::Running,
            activity: None,
            log: Vec::new(),
            transcript: Vec::new(),
            output: None,
            message_index: usize::MAX,
            started_at: std::time::Instant::now(),
            duration_ms: None,
            abort: None,
            agent: None,
        });
        app.task_barrier = Some(TaskBarrier {
            session_id: sid,
            task_ids: vec![9],
        });

        // Round already ended (agent_session is None): the last child finishing
        // feeds its report back and starts the final-synthesis stream.
        let out = app.handle_subtask_event(SubtaskEvent::Finished {
            id: 9,
            output: Ok("found the bug in parser.rs".into()),
            duration_ms: 120,
        });
        assert!(out.is_some(), "final synthesis stream starts");
        assert!(app.task_barrier.is_none(), "deferred batch cleared");
        let messages = &app.sessions.active().messages;
        let injected = messages
            .iter()
            .filter(|m| m.role == "user")
            .last()
            .expect("continuation prompt injected");
        let text = match &injected.content {
            crate::api::models::MessageContent::Text(text) => text.clone(),
            _ => panic!("expected text"),
        };
        assert!(text.contains("agent (completed)"));
        assert!(text.contains("found the bug in parser.rs"));
        assert!(text.contains("Synthesize your final response now"));
    }

    #[test]
    fn children_finishing_mid_round_do_not_inject_a_premature_synthesis() {
        use crate::app::state::{Subtask, SubtaskEvent, SubtaskStatus, TaskBarrier};

        let mut app = test_app();
        let sid = app.sessions.active_id();
        app.agent_session = Some(sid);
        app.subtasks.push(Subtask {
            id: 9,
            session_id: sid,
            parent_id: None,
            call: tool_call(
                "workflow",
                serde_json::json!({"action": "agent", "description": "x", "prompt": "p"}),
            ),
            description: "x".into(),
            todo_index: None,
            prompt: "p".into(),
            cwd: PathBuf::from("."),
            status: SubtaskStatus::Running,
            activity: None,
            log: Vec::new(),
            transcript: Vec::new(),
            output: None,
            message_index: usize::MAX,
            started_at: std::time::Instant::now(),
            duration_ms: None,
            abort: None,
            agent: None,
        });
        app.task_barrier = Some(TaskBarrier {
            session_id: sid,
            task_ids: vec![9],
        });

        // The parent round is still active: the child finishing early just
        // clears the batch — no forced continuation, no premature hand-off.
        assert!(app
            .handle_subtask_event(SubtaskEvent::Finished {
                id: 9,
                output: Ok("early report".into()),
                duration_ms: 10,
            })
            .is_none());
        assert!(app.task_barrier.is_none());
        assert_eq!(app.agent_session, Some(sid));
        assert!(app
            .sessions
            .active()
            .messages
            .iter()
            .all(|m| m.role != "user"));
    }

    #[test]
    fn clicking_sidebar_agent_enters_and_exits_its_tree_view() {
        use crate::app::state::{
            Subtask, SubtaskHitbox, SubtaskRound, SubtaskRoundRole, SubtaskStatus,
        };
        use ratatui::layout::Rect;

        let mut app = test_app();
        let sid = app.sessions.active_id();
        app.subtasks.push(Subtask {
            id: 7,
            session_id: sid,
            parent_id: Some(3),
            call: tool_call(
                "workflow",
                serde_json::json!({"action": "task", "description": "inspect", "prompt": "check"}),
            ),
            description: "inspect".into(),
            todo_index: None,
            prompt: "check".into(),
            cwd: PathBuf::from("."),
            status: SubtaskStatus::Running,
            activity: Some("searching files".into()),
            log: Vec::new(),
            transcript: vec![SubtaskRound {
                role: SubtaskRoundRole::Assistant,
                content: "Child response shown in the main chat".into(),
            }],
            output: None,
            message_index: usize::MAX,
            started_at: std::time::Instant::now(),
            duration_ms: None,
            abort: None,
            agent: None,
        });
        app.subtasks.push(Subtask {
            id: 3,
            session_id: sid,
            parent_id: None,
            call: tool_call(
                "workflow",
                serde_json::json!({"action": "task", "description": "parent", "prompt": "check"}),
            ),
            description: "parent".into(),
            todo_index: None,
            prompt: "check".into(),
            cwd: PathBuf::from("."),
            status: SubtaskStatus::Completed,
            activity: None,
            log: Vec::new(),
            transcript: Vec::new(),
            output: Some("done".into()),
            message_index: usize::MAX,
            started_at: std::time::Instant::now(),
            duration_ms: Some(100),
            abort: None,
            agent: None,
        });
        app.layout.sidebar_agents = vec![SubtaskHitbox {
            task_id: 7,
            area: Rect::new(2, 12, 60, 2),
        }];

        // Click enters the agent: chat + sidebar switch to its own content.
        let follow = app.apply(Action::ChatClick(5, 12));
        if let Some(action) = follow {
            app.apply(action);
        }
        assert_eq!(app.view_node, Some(7));
        assert_eq!(app.selected_subtask, Some(7));
        app.sync_chat_doc(80, 20);
        assert!(app
            .chat
            .doc()
            .iter()
            .any(|row| row.plain.contains("Child response shown in the main chat")));
        assert!(app.layout.panel_agents.is_empty());

        // NavigateBack exits to the parent node, not the root.
        app.apply(Action::NavigateBack);
        assert_eq!(app.view_node, Some(3));

        // Another NavigateBack reaches the root chat.
        app.apply(Action::NavigateBack);
        assert_eq!(app.view_node, None);

        // Clicking the already-entered agent collapses back to the root.
        let follow = app.apply(Action::ChatClick(5, 12));
        if let Some(action) = follow {
            app.apply(action);
        }
        app.apply(Action::NavigateBack);
        app.apply(Action::NavigateBack);
        let follow = app.apply(Action::ChatClick(5, 12));
        if let Some(action) = follow {
            app.apply(action);
        }
        assert_eq!(app.view_node, Some(7));
        let follow = app.apply(Action::ChatClick(5, 12));
        if let Some(action) = follow {
            app.apply(action);
        }
        assert_eq!(app.view_node, None);
    }

    #[test]
    fn child_detail_blocks_clicks_from_reaching_panels_behind_it() {
        use crate::app::state::PromptHitbox;
        use ratatui::layout::Rect;

        let mut app = test_app();
        app.overlay = Overlay::SubtaskDetail {
            task_id: 99,
            scroll: 0,
        };
        app.layout.prompt = Some(PromptHitbox {
            area: Rect::new(0, 0, 20, 1),
            msg: None,
        });
        app.show_last_prompt = false;

        app.apply(Action::ChatClick(2, 0));
        assert!(!app.show_last_prompt);
    }

    #[tokio::test]
    async fn cancelling_child_agent_aborts_worker_and_updates_transcript() {
        use crate::app::state::{Subtask, SubtaskStatus, TaskBarrier};

        let mut app = test_app();
        let sid = app.sessions.active_id();
        app.sessions
            .active_mut()
            .push_message(crate::api::ChatMessage::tool(
                "[tool-result:agent] agent 1 (ok)\n[agent-id:21]\n[running]\nworking",
            ));
        let worker = tokio::spawn(std::future::pending::<()>());
        let abort = worker.abort_handle();
        app.subtasks.push(Subtask {
            id: 21,
            session_id: sid,
            parent_id: None,
            call: tool_call(
                "workflow",
                serde_json::json!({"action": "agent", "description": "inspect", "prompt": "check"}),
            ),
            description: "inspect".into(),
            todo_index: Some(1),
            prompt: "check".into(),
            cwd: PathBuf::from("."),
            status: SubtaskStatus::Running,
            activity: Some("working".into()),
            log: Vec::new(),
            transcript: Vec::new(),
            output: None,
            message_index: 0,
            started_at: std::time::Instant::now(),
            duration_ms: None,
            abort: Some(abort.clone()),
            agent: None,
        });
        app.task_barrier = Some(TaskBarrier {
            session_id: sid,
            task_ids: vec![21],
        });

        app.apply(Action::AgentCancel);
        tokio::task::yield_now().await;

        assert!(abort.is_finished());
        assert_eq!(app.subtasks[0].status, SubtaskStatus::Failed);
        let text = match &app.sessions.active().messages[0].content {
            crate::api::models::MessageContent::Text(text) => text,
            _ => panic!("expected text"),
        };
        assert!(text.contains("Cancelled by user"));
    }

    #[test]
    fn agent_cancel_sets_the_abort_flag_for_the_in_flight_tool() {
        let mut app = test_app();
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        app.agent_abort = Some(flag.clone());
        assert!(!flag.load(std::sync::atomic::Ordering::Relaxed));
        app.apply(Action::AgentCancel);
        assert!(flag.load(std::sync::atomic::Ordering::Relaxed));
        assert!(app.agent_abort.is_none());
        assert!(app.agent_tool_rx.is_none());
        assert!(app.agent_tool_batch_rx.is_none());
    }

    #[test]
    fn child_agent_navigation_is_one_based_and_wraps() {
        use crate::app::state::{Subtask, SubtaskStatus};

        let mut app = test_app();
        let sid = app.sessions.active_id();
        let make_agent = |id, description: &str| Subtask {
            id,
            session_id: sid,
            parent_id: None,
            call: tool_call(
                "workflow",
                serde_json::json!({"action": "agent", "description": description, "prompt": "check"}),
            ),
            description: description.into(),
            todo_index: None,
            prompt: "check".into(),
            cwd: PathBuf::from("."),
            status: SubtaskStatus::Running,
            activity: None,
            log: Vec::new(),
            transcript: Vec::new(),
            output: None,
            message_index: usize::MAX,
            started_at: std::time::Instant::now(),
            duration_ms: None,
            abort: None,
            agent: None,
        };
        app.subtasks = vec![make_agent(11, "first"), make_agent(12, "second")];
        app.selected_subtask = Some(11);

        app.apply(Action::NextSubtask);
        assert_eq!(app.selected_subtask, Some(12));
        assert!(matches!(
            app.overlay,
            Overlay::SubtaskDetail { task_id: 12, .. }
        ));

        app.apply(Action::NextSubtask);
        assert_eq!(app.selected_subtask, Some(11));
        app.apply(Action::PrevSubtask);
        assert_eq!(app.selected_subtask, Some(12));
    }

    #[test]
    fn ask_tool_opens_decision_and_records_choice() {
        let mut app = test_app();
        app.pending_tools.push_back(crate::agent::ToolCall {
            name: "ask".into(),
            args: serde_json::json!({"question": "Pick one", "options": ["A", "B", "C"]}),
            id: None,
        });
        let _ = app.process_next_tool();
        match &mut app.overlay {
            Overlay::Decision(r) => r.selected = 2, // choose "C"
            other => panic!("expected decision overlay, got {:?}", other),
        }
        let _ = app.resolve_decision();
        assert!(matches!(app.overlay, Overlay::None));
        let last = app.sessions.active().messages.last().unwrap();
        assert_eq!(last.role, "tool");
        assert!(tool_msg_text(&app).contains('C'));
    }

    #[test]
    fn edited_ask_option_becomes_recorded_choice() {
        let mut app = test_app();
        app.pending_tools.push_back(crate::agent::ToolCall {
            name: "ask".into(),
            args: serde_json::json!({"question": "Pick one", "options": ["Original", "Other"]}),
            id: None,
        });
        let _ = app.process_next_tool();
        app.apply(Action::AgentDecisionEdited("Adjusted option".into()));
        let _ = app.resolve_decision();
        assert!(tool_msg_text(&app).contains("Adjusted option"));
    }

    #[tokio::test]
    async fn propose_step_requires_multiple_paths_and_records_selection() {
        let mut app = test_app();
        app.pending_tools.push_back(crate::agent::ToolCall {
            name: "propose_step".into(),
            args: serde_json::json!({
                "title": "Storage",
                "description": "Choose persistence model",
                "alternatives": [
                    {"label": "File", "description": "Simple local JSON", "feasibility": "possible"},
                    {"label": "SQLite", "description": "Queryable local database", "feasibility": "possible"}
                ]
            }),
            id: None,
        });
        let _ = app.process_next_tool();
        match &mut app.overlay {
            Overlay::Decision(r) => {
                assert!(r.options[0].contains("Simple local JSON"));
                r.selected = 1;
            }
            other => panic!("expected path decision overlay, got {:?}", other),
        }
        app.model_idx = 2;
        let follow_up = app.resolve_decision();
        assert!(tool_msg_text(&app).contains("Queryable local database"));
        assert!(follow_up.is_some());
        assert!(app.sessions.active().is_streaming());
    }

    #[test]
    fn ask_tool_without_options_records_free_form_answer() {
        let mut app = test_app();
        app.pending_tools.push_back(crate::agent::ToolCall {
            name: "ask".into(),
            args: serde_json::json!({"question": "What name should I use?"}),
            id: None,
        });
        let _ = app.process_next_tool();
        match &mut app.overlay {
            Overlay::Decision(r) => {
                assert!(r.free_form());
                r.answer = "aitui".into();
            }
            other => panic!("expected free-form decision overlay, got {:?}", other),
        }
        let _ = app.resolve_decision();
        assert!(matches!(app.overlay, Overlay::None));
        assert!(tool_msg_text(&app).contains("aitui"));
    }

    #[test]
    fn plan_tool_writes_file_and_approval_feeds_body_back() {
        let mut app = test_app();
        let path = std::env::temp_dir().join("aitui_test_plan.md");
        let _ = std::fs::remove_file(&path);
        app.pending_tools.push_back(crate::agent::ToolCall {
            name: "plan".into(),
            args: serde_json::json!({
                "path": path.to_string_lossy(),
                "body": "step one\nstep two",
            }),
            id: None,
        });
        let _ = app.process_next_tool();
        assert!(matches!(app.overlay, Overlay::Plan(_)));
        assert!(path.exists(), "plan file should be written");
        let _ = app.resolve_plan(true);
        let out = tool_msg_text(&app);
        assert!(out.contains("APPROVED"));
        assert!(out.contains("step one"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn plan_denial_records_denied() {
        let mut app = test_app();
        let path = std::env::temp_dir().join("aitui_test_plan_deny.md");
        let _ = std::fs::remove_file(&path);
        app.pending_tools.push_back(crate::agent::ToolCall {
            name: "plan".into(),
            args: serde_json::json!({"path": path.to_string_lossy(), "body": "x"}),
            id: None,
        });
        let _ = app.process_next_tool();
        let _ = app.resolve_plan(false);
        assert!(tool_msg_text(&app).contains("DENIED"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn non_read_tool_raises_permission_popup() {
        // A write is not auto-approved → process_next_tool must raise a Permission
        // overlay (this is the popup that was rendering off-box before).
        let mut app = test_app();
        app.pending_tools.push_back(crate::agent::ToolCall {
            name: "write_file".into(),
            args: serde_json::json!({"path": "x.txt", "content": "hi"}),
            id: None,
        });
        let _ = app.process_next_tool();
        assert!(
            matches!(app.overlay, Overlay::Permission(_)),
            "write should prompt for permission"
        );
    }

    #[test]
    fn dismiss_notice_closes_it() {
        let mut app = test_app();
        app.overlay = Overlay::Notice {
            title: "t".into(),
            body: "b".into(),
        };
        app.apply(Action::DismissNotice);
        assert!(matches!(app.overlay, Overlay::None));
    }

    // ── Commands ───────────────────────────────────────────────────────────────

    #[test]
    fn command_w_submits() {
        let mut app = test_app();
        let result = app.apply(Action::RunCommand("w".into()));
        assert!(matches!(result, Some(Action::Submit)));
    }

    #[test]
    fn command_q_quits() {
        let mut app = test_app();
        let result = app.apply(Action::RunCommand("q".into()));
        assert!(matches!(result, Some(Action::Quit)));
    }

    #[test]
    fn command_new_creates_session() {
        let mut app = test_app();
        let result = app.apply(Action::RunCommand("new".into()));
        assert!(matches!(result, Some(Action::NewSession)));
    }

    #[test]
    fn command_clear_clears_messages() {
        let mut app = test_app();
        app.sessions
            .active_mut()
            .push_message(crate::api::ChatMessage::user("test"));
        app.apply(Action::RunCommand("clear".into()));
        assert!(app.sessions.active().messages.is_empty());
    }

    #[test]
    fn command_history_tracks_commands() {
        let mut app = test_app();
        app.apply(Action::RunCommand("w".into()));
        app.apply(Action::RunCommand("q".into()));
        assert_eq!(app.command_history.len(), 2);
        assert_eq!(app.command_history[0], "w");
        assert_eq!(app.command_history[1], "q");
    }

    #[test]
    fn command_history_does_not_duplicate_consecutive() {
        let mut app = test_app();
        app.apply(Action::RunCommand("w".into()));
        app.apply(Action::RunCommand("w".into()));
        assert_eq!(app.command_history.len(), 1);
    }

    #[test]
    fn unknown_command_shows_status() {
        let mut app = test_app();
        app.apply(Action::RunCommand("bogus".into()));
        assert!(app.status.as_deref().unwrap().contains("Unknown"));
    }

    #[test]
    fn command_model_selects_model() {
        let mut app = test_app();
        let result = app.apply(Action::RunCommand("model claude-sonnet-4-6".into()));
        assert!(matches!(result, Some(Action::SelectModel(_))));
    }

    #[test]
    fn command_attach_file_invalid_shows_error() {
        let mut app = test_app();
        let follow = app.apply(Action::RunCommand("attach /nonexistent/path".into()));
        if let Some(a) = follow {
            app.apply(a);
        }
        assert!(app.status.as_deref().unwrap().contains("Not found"));
    }

    // ── Agent ──────────────────────────────────────────────────────────────────

    #[test]
    fn speculative_result_is_used_without_respawning() {
        use crate::agent::{ToolCall, ToolResult};
        let mut app = test_app();
        let call = ToolCall {
            name: "read_file".into(),
            args: serde_json::json!({"path": "x"}),
            id: None,
        };
        app.permissions.remember_allow(call.kind().unwrap());
        // A result pre-run while the reply streamed is stashed under its call sig.
        app.store_spec_result(
            app.spec_epoch,
            ToolResult::success(call.clone(), "file contents".into(), 1),
        );
        app.pending_tools.push_back(call);
        app.agent_session = Some(app.sessions.active_id());

        let _ = app.process_next_tool();

        // The cached result was used directly — no async tool execution spawned.
        assert!(
            app.agent_tool_rx.is_none(),
            "must not respawn a pre-run tool"
        );
        assert!(
            app.sessions.active().messages.iter().any(|m| m.role == "tool"
                && matches!(&m.content, crate::api::models::MessageContent::Text(t) if t.contains("file contents"))),
            "the speculative result should be recorded as a tool message",
        );
    }

    #[test]
    fn stale_epoch_speculative_result_is_dropped() {
        use crate::agent::{ToolCall, ToolResult};
        let mut app = test_app();
        let call = ToolCall {
            name: "read_file".into(),
            args: serde_json::json!({"path": "x"}),
            id: None,
        };
        let stale = app.spec_epoch;
        app.spec_epoch = app.spec_epoch.wrapping_add(1); // turn moved on
        app.store_spec_result(stale, ToolResult::success(call, "old".into(), 1));
        assert!(
            app.spec_results.is_empty(),
            "a result from a past turn must be dropped"
        );
    }

    #[test]
    fn api_setup_opens_and_edits_both_fields() {
        let mut app = test_app();
        app.apply(Action::OpenApiSetup);
        assert!(matches!(app.overlay, Overlay::ApiSetup(_)));
        // Prefilled from (empty) config; the overlay consumes PickerChar.
        for c in "http://x/v1".chars() {
            app.apply(Action::PickerChar(c));
        }
        match &app.overlay {
            Overlay::ApiSetup(a) => assert_eq!(a.endpoint, "http://x/v1"),
            _ => panic!("expected ApiSetup overlay"),
        }
        app.apply(Action::PickerDown); // switch to the key field
        for c in "sk-1".chars() {
            app.apply(Action::PickerChar(c));
        }
        match &app.overlay {
            Overlay::ApiSetup(a) => {
                assert_eq!(a.field, 1);
                assert_eq!(a.api_key, "sk-1");
            }
            _ => panic!("expected ApiSetup overlay"),
        }
    }

    #[test]
    fn base_url_error_detection() {
        assert!(looks_like_base_url_error(
            "Request failed: builder error: relative url without a base"
        ));
        assert!(looks_like_base_url_error("No API client"));
        assert!(!looks_like_base_url_error("API error 500: internal"));
    }

    #[test]
    fn native_command_is_noop() {
        let mut app = test_app();
        app.apply(Action::RunCommand("native".into()));
        assert!(app.status.unwrap().contains("always on"));
    }

    // ── Models ─────────────────────────────────────────────────────────────────

    #[test]
    fn next_model_cycles_forward() {
        // test_app() has 3 models: gpt-5.5, claude-sonnet-4-6, mock.
        let mut app = test_app();
        assert_eq!(app.model_idx, 0);
        app.apply(Action::NextModel);
        assert_eq!(app.model_idx, 1);
        app.apply(Action::NextModel);
        assert_eq!(app.model_idx, 2);
        app.apply(Action::NextModel);
        assert_eq!(app.model_idx, 0); // wraps
    }

    #[test]
    fn prev_model_cycles_backward() {
        let mut app = test_app();
        app.apply(Action::PrevModel);
        assert_eq!(app.model_idx, 2); // wraps to last
    }

    #[test]
    fn select_model_finds_or_appends() {
        let mut app = test_app();
        app.apply(Action::SelectModel("gpt-5.5".into()));
        assert_eq!(app.model_idx, 0);
        app.apply(Action::SelectModel("new-model".into()));
        assert_eq!(app.model_idx, 3);
    }

    #[test]
    fn models_loaded_appends_mock_and_selects_default() {
        use crate::app::state::ModelLoad;
        let mut app = test_app();
        app.config.api.default_model = "gpt-5.5".into();
        app.apply(Action::ModelsLoaded(vec![
            "gpt-5.4".into(),
            "gpt-5.5".into(),
        ]));
        assert!(
            app.models.iter().any(|m| m == "mock"),
            "mock always present"
        );
        assert_eq!(app.current_model(), "gpt-5.5");
        assert_eq!(app.model_load, ModelLoad::Loaded);
    }

    #[test]
    fn models_loaded_falls_back_to_mock_when_default_absent() {
        let mut app = test_app();
        app.config.api.default_model = "does-not-exist".into();
        app.apply(Action::ModelsLoaded(vec!["gpt-5.4".into()]));
        assert_eq!(app.current_model(), "mock");
    }

    #[test]
    fn models_failed_uses_mock_only() {
        use crate::app::state::ModelLoad;
        let mut app = test_app();
        app.apply(Action::ModelsFailed);
        assert_eq!(app.models, vec!["mock".to_string()]);
        assert_eq!(app.model_load, ModelLoad::Failed);
        assert!(app.is_mock());
    }

    // ── Overlays ───────────────────────────────────────────────────────────────

    #[test]
    fn open_model_picker_sets_overlay() {
        let mut app = test_app();
        app.apply(Action::OpenModelPicker);
        assert!(matches!(app.overlay, Overlay::Picker(_)));
    }

    #[test]
    fn picker_cancel_clears_overlay() {
        let mut app = test_app();
        app.overlay = Overlay::Picker(Picker::models(vec![]));
        app.apply(Action::PickerCancel);
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn open_settings_sets_overlay() {
        let mut app = test_app();
        app.apply(Action::OpenSettings);
        assert!(matches!(app.overlay, Overlay::Settings(_)));
    }

    #[test]
    fn open_command_palette_sets_overlay() {
        let mut app = test_app();
        app.apply(Action::OpenCommandPalette);
        assert!(matches!(app.overlay, Overlay::Palette(_)));
    }

    // ── UI toggles ─────────────────────────────────────────────────────────────

    #[test]
    fn toggle_help_flips_flag() {
        let mut app = test_app();
        assert!(!app.show_help);
        app.apply(Action::ToggleHelp);
        assert!(app.show_help);
        app.apply(Action::ToggleHelp);
        assert!(!app.show_help);
    }

    #[test]
    fn quit_sets_flag() {
        let mut app = test_app();
        app.apply(Action::Quit);
        assert!(app.should_quit);
    }

    // ── Transcript scrolling ─────────────────────────────────────────────────────

    #[test]
    fn chat_scroll_when_no_messages_no_panic() {
        let mut app = test_app();
        app.apply(Action::ChatPageUp);
        app.apply(Action::ChatScroll(-3));
        // no crash
    }

    #[test]
    fn transcript_scrolling_does_not_auto_expand_or_collapse_last_prompt() {
        let mut app = test_app();
        assert!(!app.show_last_prompt);

        app.apply(Action::ChatScroll(3));
        assert!(!app.show_last_prompt);

        app.show_last_prompt = true;
        app.apply(Action::ChatScroll(-3));
        assert!(app.show_last_prompt);

        app.show_last_prompt = false;
        app.apply(Action::ChatPageUp);
        assert!(!app.show_last_prompt);

        app.show_last_prompt = true;
        app.apply(Action::ChatBottom);
        assert!(app.show_last_prompt);
    }

    #[test]
    fn toggle_output_flips_flag_and_touches() {
        let mut app = test_app();
        assert!(!app.show_output);
        let rev = app.content_rev;
        app.apply(Action::ToggleOutput);
        assert!(app.show_output);
        assert_ne!(app.content_rev, rev);
        app.apply(Action::ToggleOutput);
        assert!(!app.show_output);
    }

    #[test]
    fn response_suggestion_inserts_into_empty_composer() {
        let mut app = test_app();
        app.sessions.active_mut().response_suggestions = vec!["Run the tests".into()];

        app.apply(Action::AcceptResponseSuggestion(0));

        assert_eq!(app.input.text(), "Run the tests");
        assert_eq!(app.vim, VimMode::Insert);
        assert!(app.sessions.active().response_suggestions.is_empty());
    }

    #[test]
    fn chat_click_toggles_individual_block_header() {
        use ratatui::layout::Rect;
        let mut app = test_app();
        app.layout.chat = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let (rows, header_idx, key) = collapsible_tool_doc();
        app.chat.stick_bottom = false;
        app.chat.scroll = 0;
        app.chat.set_doc(rows, 1, 80, 24);
        app.chat.scroll = 0; // view the top so the header maps to its row directly

        assert!(!app.chat.toggled.contains(&key));
        app.apply(Action::ChatClick(5, header_idx as u16)); // click the header row
        assert!(
            app.chat.toggled.contains(&key),
            "click should flip the block"
        );
        // Browsing (not at bottom): the click must NOT jump/reveal — scroll stays put.
        assert_eq!(
            app.chat.focus_msg, None,
            "click while browsing must not force a scroll"
        );
        assert!(
            !app.chat.stick_bottom,
            "click while browsing must not stick to bottom"
        );
        app.apply(Action::ChatClick(5, header_idx as u16));
        assert!(!app.chat.toggled.contains(&key), "second click flips back");
    }

    #[test]
    fn chat_click_at_bottom_still_reveals() {
        use ratatui::layout::Rect;
        let mut app = test_app();
        app.layout.chat = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let (rows, header_idx, key) = collapsible_tool_doc();
        app.chat.stick_bottom = true; // already at the bottom
        app.chat.set_doc(rows, 1, 80, 24);
        app.chat.scroll = 0;
        app.apply(Action::ChatClick(5, header_idx as u16));
        assert_eq!(
            app.chat.focus_msg,
            Some(key.0),
            "at bottom, a click still reveals the block"
        );
    }

    #[test]
    fn chat_click_on_non_header_row_does_not_toggle() {
        use ratatui::layout::Rect;
        let mut app = test_app();
        app.layout.chat = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let (rows, _header_idx, _key) = collapsible_tool_doc();
        assert!(
            rows.iter().filter(|r| r.toggle.is_some()).count() == 1,
            "exactly one collapsible header exists"
        );
        app.chat.stick_bottom = false;
        app.chat.set_doc(rows, 1, 80, 24);
        app.chat.scroll = 0;
        // Click on the header toggles it (tool messages no longer have a separator
        // before the collapsible block).
        assert!(app.chat.toggled.is_empty());
        app.apply(Action::ChatClick(3, 0));
        assert!(!app.chat.toggled.is_empty(), "clicking the header toggles");
    }

    #[test]
    fn chat_click_outside_pane_is_ignored() {
        use ratatui::layout::Rect;
        let mut app = test_app();
        app.layout.chat = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let (rows, _idx, _key) = collapsible_tool_doc();
        app.chat.set_doc(rows, 1, 80, 24);
        app.apply(Action::ChatClick(5, 100)); // row 100 is below the pane
        assert!(app.chat.toggled.is_empty());
    }

    /// A document whose only collapsible header is a long (>6 line) tool result.
    /// Returns the rows, the header's row index, and its `(msg, block)` key.
    fn collapsible_tool_doc() -> (
        Vec<crate::render::document::RenderedLine>,
        usize,
        (usize, usize),
    ) {
        use crate::domain::blocks::Block;
        use crate::render::document::{build, DocMessage};
        use std::collections::HashSet;
        let output = (0..10)
            .map(|i| format!("out {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let msgs = vec![DocMessage {
            role: "tool".into(),
            blocks: vec![Block::ToolResult {
                ok: true,
                name: Some("shell".into()),
                summary: "shell(x)".into(),
                output,
            }],
            duration_ms: None,
            created_at: None,
        }];
        let rows = build(
            &msgs,
            80,
            &crate::render::theme::Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        let idx = rows
            .iter()
            .position(|r| r.toggle.is_some())
            .expect("a collapsible header");
        let key = rows[idx].toggle.unwrap();
        (rows, idx, key)
    }

    // ── Attachments ────────────────────────────────────────────────────────────

    #[test]
    fn attach_file_that_exists_sets_attachment() {
        let mut app = test_app();
        let path = std::env::current_dir().unwrap_or_default();
        app.apply(Action::AttachFile(path.clone()));
        assert!(app.attachment.is_some());
    }

    #[test]
    fn attach_missing_file_shows_error() {
        let mut app = test_app();
        app.apply(Action::AttachFile(std::path::PathBuf::from(
            "/must/not/exist/xyz",
        )));
        assert!(app.attachment.is_none());
        assert!(app.status.as_deref().unwrap().contains("Not found"));
    }

    #[test]
    fn clear_attachment_removes_it() {
        let mut app = test_app();
        app.attachment = Some(std::path::PathBuf::from("."));
        app.apply(Action::ClearAttachment);
        assert!(app.attachment.is_none());
    }

    // ── Streaming ──────────────────────────────────────────────────────────────

    fn push_active_stream(app: &mut App) {
        let sid = app.sessions.active_id();
        app.streams.push(crate::app::state::StreamHandle {
            session_id: sid,
            rx: tokio::sync::mpsc::channel(1).1,
            cold_retries: 0,
        });
    }

    #[test]
    fn stream_token_updates_session_and_touches() {
        let mut app = test_app();
        app.sessions.active_mut().begin_assistant_stream();
        let sid = app.sessions.active_id();
        let rev = app.content_rev;
        app.apply(Action::StreamToken(sid, "hello".into()));
        assert_eq!(
            app.sessions.active().streaming_display().as_deref(),
            Some("hello")
        );
        assert_ne!(app.content_rev, rev);
    }

    #[test]
    fn agent_stream_cut_on_complete_tool_call() {
        let mut app = test_app();
        app.sessions.active_mut().agent_mode = true;
        app.sessions.active_mut().begin_assistant_stream();
        let sid = app.sessions.active_id();
        push_active_stream(&mut app);
        app.apply(Action::StreamToken(
            sid,
            "```tool\n{\"name\":\"list_dir\",\"args\":{\"path\":\".\"}}\n```".into(),
        ));
        // The stream was cut: flag set for the main loop, message finalized, handle gone.
        assert_eq!(
            app.cut_stream,
            Some(sid),
            "a complete tool call must cut the stream"
        );
        assert!(!app.sessions.active().is_streaming());
        assert!(app.streams.is_empty());
        assert!(
            app.sessions
                .active()
                .messages
                .last()
                .is_some_and(|m| matches!(
                    &m.content,
                    crate::api::models::MessageContent::Text(t) if t.contains("list_dir")
                )),
            "the finalized turn keeps the tool call",
        );
    }

    /// A fence on the reasoning channel is not a commitment — the model may still
    /// discard it — so it must not cut the stream. An endpoint that routes its whole
    /// reply through reasoning still works: the call runs once the turn finalizes.
    #[test]
    fn reasoning_tool_call_does_not_cut_stream_but_stays_runnable() {
        let mut app = test_app();
        app.sessions.active_mut().agent_mode = true;
        app.sessions.active_mut().begin_assistant_stream();
        let sid = app.sessions.active_id();
        push_active_stream(&mut app);

        app.apply(Action::StreamReasoning(
            sid,
            "```tool\n{\"name\":\"list\",\"args\":{\"path\":\".\"}}\n```".into(),
        ));

        assert_eq!(app.cut_stream, None, "a reasoning fence must not cut");
        assert!(app.sessions.active().is_streaming());

        // Turn ends with nothing on the content channel: the reasoning text was the
        // reply, so its call is runnable.
        app.apply(Action::StreamDone(sid));
        let calls = app.tool_calls_in(sid);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "list");
    }

    /// A model that interleaves visible `<thinking>` tags in the content channel
    /// (Claude-Code style) emits its tool call inside the thinking block and then
    /// stops, expecting the harness to run it. The call must survive into the
    /// committed-call fallback and actually start the child-agent batch.
    #[tokio::test]
    async fn workflow_agent_call_inside_visible_thinking_block_still_runs() {
        let mut app = test_app();
        app.sessions.active_mut().agent_mode = true;
        app.sessions.active_mut().begin_assistant_stream();
        let sid = app.sessions.active_id();
        push_active_stream(&mut app);

        let call_json = r#"{"args":{"action":"agent","checks":[],"cwd":"","description":"Audit keyboard behavior","items":[],"prompt":"Audit Shift+Enter handling and Vim normal-mode A behavior","summary":"","task_index":1,"verification":"none"},"id":"call_54RfPgZ6VnIwzDCFsoe1xVb7","name":"workflow"}"#;
        let text = format!(
            "<thinking>\nI'll delegate this audit to a child agent.\n<tool>\n{}\n</tool>\n</thinking>",
            call_json
        );
        app.apply(Action::StreamToken(sid, text));

        // The block is closed: the model committed to the call and typically stops
        // here — cut so the child-agent round starts instead of hanging the turn.
        assert_eq!(
            app.cut_stream,
            Some(sid),
            "a call in a closed thinking block must cut the stream"
        );
        assert!(!app.sessions.active().is_streaming());
        assert!(app.streams.is_empty());

        app.apply(Action::StreamDone(sid));
        let calls = app.tool_calls_in(sid);
        assert_eq!(
            calls.len(),
            1,
            "committed fallback must recover a call the model stopped after emitting in thinking"
        );
        assert_eq!(calls[0].name, "workflow");
        assert_eq!(calls[0].args["action"], "agent");

        // StreamDone already started the agent round: the child is spawned and the
        // barrier waits for it, with no leftover queued tools.
        assert_eq!(app.subtasks.len(), 1, "the child agent is spawned");
        assert_eq!(app.subtasks[0].description, "Audit keyboard behavior");
        assert!(app.task_barrier.is_some(), "barrier waits for the child");
        assert!(app.pending_tools.is_empty());
    }

    /// A `workflow(agent)` call that names a configured agent resolves the
    /// registry entry: the spawned subtask carries the agent identity so the
    /// detail overlay can show the role/model/tool policy it runs with.
    #[tokio::test]
    async fn named_agent_call_resolves_the_config_registry_entry() {
        use crate::config::types::AgentDef;

        let mut app = test_app();
        app.config.agents.insert(
            "reviewer".into(),
            AgentDef {
                description: "Read-only code review".into(),
                model: Some("review-model".into()),
                role: "senior reviewer".into(),
                tools: vec!["read".into(), "search".into()],
                deny: Vec::new(),
            },
        );
        app.sessions.active_mut().agent_mode = true;
        app.sessions.active_mut().begin_assistant_stream();
        let sid = app.sessions.active_id();
        push_active_stream(&mut app);

        let call_json = r#"{"args":{"action":"agent","agent":"reviewer","description":"review diff","prompt":"Review the pending diff for correctness"},"id":"call_9","name":"workflow"}"#;
        app.apply(Action::StreamToken(
            sid,
            format!("<thinking>\n<tool>\n{}\n</tool>\n</thinking>", call_json),
        ));
        app.apply(Action::StreamDone(sid));

        assert_eq!(app.subtasks.len(), 1);
        assert_eq!(app.subtasks[0].agent.as_deref(), Some("reviewer"));
        assert_eq!(app.subtasks[0].description, "review diff");

        // Unknown names still spawn; they fall back to the inline role prompt.
        let mut app = test_app();
        app.sessions.active_mut().agent_mode = true;
        app.sessions.active_mut().begin_assistant_stream();
        let sid = app.sessions.active_id();
        push_active_stream(&mut app);
        let call_json = r#"{"args":{"action":"agent","agent":"nope","description":"scan","prompt":"scan the tree"},"id":"call_10","name":"workflow"}"#;
        app.apply(Action::StreamToken(
            sid,
            format!("<thinking>\n<tool>\n{}\n</tool>\n</thinking>", call_json),
        ));
        app.apply(Action::StreamDone(sid));
        assert_eq!(app.subtasks.len(), 1);
        assert_eq!(app.subtasks[0].agent.as_deref(), Some("nope"));
    }

    /// The dangerous case: the model sketches a call while thinking, then commits to
    /// a different one. Only the committed call may run.
    #[test]
    fn tool_call_drafted_in_reasoning_never_runs_when_content_commits() {
        let mut app = test_app();
        app.sessions.active_mut().agent_mode = true;
        app.sessions.active_mut().begin_assistant_stream();
        let sid = app.sessions.active_id();
        push_active_stream(&mut app);

        app.apply(Action::StreamReasoning(
            sid,
            "maybe ```tool\n{\"name\":\"delete\",\"args\":{\"path\":\"src\"}}\n``` — no, too risky"
                .into(),
        ));
        assert_eq!(app.cut_stream, None, "a draft must not cut the stream");

        app.apply(Action::StreamToken(
            sid,
            "```tool\n{\"name\":\"list\",\"args\":{\"path\":\".\"}}\n```".into(),
        ));
        assert_eq!(app.cut_stream, Some(sid), "the committed call cuts");

        let calls = app.tool_calls_in(sid);
        assert_eq!(calls.len(), 1, "the drafted delete must not be runnable");
        assert_eq!(calls[0].name, "list");
    }

    #[test]
    fn non_agent_stream_is_not_cut() {
        let mut app = test_app(); // agent mode off
        app.sessions.active_mut().begin_assistant_stream();
        let sid = app.sessions.active_id();
        push_active_stream(&mut app);
        app.apply(Action::StreamToken(
            sid,
            "```tool\n{\"name\":\"list_dir\",\"args\":{\"path\":\".\"}}\n```".into(),
        ));
        assert_eq!(
            app.cut_stream, None,
            "non-agent mode must keep streaming normally"
        );
        assert!(app.sessions.active().is_streaming());
    }

    #[test]
    fn stream_done_clears_rx_and_saves() {
        let mut app = test_app();
        app.sessions.active_mut().begin_assistant_stream();
        let sid = app.sessions.active_id();
        push_active_stream(&mut app);
        app.apply(Action::StreamDone(sid));
        assert!(app.streams.is_empty());
    }

    #[test]
    fn cancel_stream_clears_rx_and_finalizes() {
        let mut app = test_app();
        app.sessions.active_mut().begin_assistant_stream();
        push_active_stream(&mut app);
        app.apply(Action::CancelStream);
        assert!(app.streams.is_empty());
        assert!(!app.sessions.active().is_streaming());
    }

    #[test]
    fn fork_duplicates_active_session() {
        let mut app = test_app();
        app.sessions
            .active_mut()
            .push_message(crate::api::ChatMessage::user("hi"));
        let before = app.sessions.all().len();
        app.apply(Action::ForkSession);
        assert_eq!(app.sessions.all().len(), before + 1);
        // The fork carries the original's messages and is now active.
        assert!(app
            .sessions
            .active()
            .messages
            .iter()
            .any(|m| m.role == "user"));
        assert!(app.sessions.active().name.contains("fork"));
    }

    #[test]
    fn background_stream_targets_its_session_not_active() {
        // Start a stream for session A, switch to a new session B, then a token for
        // A must land in A — not the now-active B (this is what enables parallel).
        let mut app = test_app();
        let a = app.sessions.active_id();
        app.sessions.active_mut().begin_assistant_stream();
        app.apply(Action::NewSession);
        let b = app.sessions.active_id();
        assert_ne!(a, b);
        app.apply(Action::StreamToken(a, "from-a".into()));
        assert_eq!(
            app.sessions
                .by_id(a)
                .unwrap()
                .streaming_display()
                .as_deref(),
            Some("from-a")
        );
        assert!(app.sessions.by_id(b).unwrap().streaming_display().is_none());
    }

    // ── System prompt ──────────────────────────────────────────────────────────

    #[test]
    fn set_system_prompt_updates_session() {
        let mut app = test_app();
        app.apply(Action::SetSystemPrompt(Some("Be concise".into())));
        assert_eq!(
            app.sessions.active().system_prompt.as_deref(),
            Some("Be concise")
        );
    }

    #[test]
    fn set_system_prompt_clears_with_none() {
        let mut app = test_app();
        app.sessions.active_mut().system_prompt = Some("old".into());
        app.apply(Action::SetSystemPrompt(None));
        assert!(app.sessions.active().system_prompt.is_none());
    }
}
