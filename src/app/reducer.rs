//! The reducer: applies one `Action` to the `App`, optionally returning a
//! follow-up action. All mutation funnels through here so behaviour is easy to
//! trace and test.

use std::path::PathBuf;

use crate::agent::Permission;
use crate::app::action::{Action, Dir};
use crate::app::overlay::{
    BrowsePurpose, FileBrowser, Overlay, Picker, PickerKind, Settings, SettingsRow,
};
use crate::app::state::{App, PendingExternal};
use crate::input::vim::VimMode;

impl App {
    fn chat_h(&self) -> usize {
        self.layout.chat.height.saturating_sub(2) as usize
    }

    pub fn apply(&mut self, action: Action) -> Option<Action> {
        match action {
            Action::Quit => self.should_quit = true,
            Action::Resize => {}
            Action::ToggleHelp => self.show_help = !self.show_help,

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
                if !sel.is_empty() {
                    self.yank = Some(sel);
                }
                self.input.end_visual();
                self.vim = VimMode::Normal;
                self.input.clamp_normal();
            }
            Action::VisualDelete => {
                let sel = self.input.delete_selection();
                if !sel.is_empty() {
                    self.yank = Some(sel);
                }
                self.vim = VimMode::Normal;
                self.input.clamp_normal();
                self.update_mention();
            }
            Action::VisualChange => {
                let sel = self.input.delete_selection();
                if !sel.is_empty() {
                    self.yank = Some(sel);
                }
                self.vim = VimMode::Insert;
                self.update_mention();
            }
            Action::EnterCommand => {
                self.vim = VimMode::Command;
                self.command.clear();
                self.command_history_idx = None;
            }
            Action::EnterOperator(op) => self.vim = VimMode::Operator(op),

            // ── Input editing ───────────────────────────────────────────────
            Action::InsertChar(c) => {
                if self.vim == VimMode::Command {
                    self.command.push(c);
                } else {
                    self.input.insert_char(c);
                    self.update_mention();
                    self.last_insert = Some(c); // track for the jk-style chord
                }
            }
            Action::Newline => {
                self.mention.reset();
                self.input.insert_newline();
                self.last_insert = None;
            }
            Action::Backspace => {
                if self.vim == VimMode::Command {
                    self.command.pop();
                } else {
                    self.input.backspace();
                    self.update_mention();
                }
                self.last_insert = None;
            }
            Action::DeleteWordBack => {
                if self.vim == VimMode::Command {
                    // Delete a word from the command line too.
                    while self.command.ends_with(char::is_whitespace) { self.command.pop(); }
                    while self.command.chars().next_back().is_some_and(|c| !c.is_whitespace()) { self.command.pop(); }
                } else {
                    self.input.delete_word_back();
                    self.update_mention();
                }
                self.last_insert = None;
            }
            Action::DeleteWordForward => {
                self.input.delete_word_forward();
                self.update_mention();
            }
            Action::DeleteAt => self.input.delete_at(),
            Action::DeleteLine => self.input.delete_line(),
            Action::YankLine => {
                self.yank = Some(self.input.yank_line());
                self.set_status("Line yanked");
            }
            Action::Paste => {
                if let Some(t) = self.yank.clone() {
                    self.input.paste(&t);
                }
            }
            Action::PasteText(t) => self.smart_paste(t),
            Action::Move(dir) => match dir {
                Dir::Left => self.input.left(),
                Dir::Right => self.input.right(),
                Dir::Up => self.input.up(),
                Dir::Down => self.input.down(),
                Dir::WordForward => self.input.word_forward(),
                Dir::WordBackward => self.input.word_backward(),
            },
            Action::LineStart => self.input.line_start(),
            Action::LineEnd => self.input.line_end(),

            // ── Command line ────────────────────────────────────────────────
            Action::CommandChar(c) => self.command.push(c),
            Action::CommandBackspace => {
                self.command.pop();
            }
            Action::RunCommand(cmd) => return self.run_command(&cmd),
            Action::CommandHistoryPrev => {
                if !self.command_history.is_empty() {
                    let idx = match self.command_history_idx {
                        None => self.command_history.len() - 1,
                        Some(i) => i.saturating_sub(1),
                    };
                    self.command_history_idx = Some(idx);
                    self.command = self.command_history[idx].clone();
                }
            }
            Action::InputHistoryPrev => self.input_history_prev(),
            Action::InputHistoryNext => self.input_history_next(),
            Action::CommandHistoryNext => match self.command_history_idx {
                Some(i) if i + 1 < self.command_history.len() => {
                    self.command_history_idx = Some(i + 1);
                    self.command = self.command_history[i + 1].clone();
                }
                Some(_) => {
                    self.command_history_idx = None;
                    self.command.clear();
                }
                None => {}
            },

            // ── Submission / streaming ──────────────────────────────────────
            Action::Submit => return self.submit(),
            Action::AttachStream(sid, rx) => {
                self.streams.push(crate::app::state::StreamHandle { session_id: sid, rx });
            }
            Action::StreamToken(sid, t) => {
                if let Some(s) = self.sessions.by_id_mut(sid) {
                    s.append_stream_token(&t);
                }
                if sid == self.sessions.active_id() {
                    self.chat.stick_bottom = true;
                }
                // Pre-run any complete read-only tool block already in the reply so
                // its result is ready the instant the turn finishes.
                self.speculate_read_tools(sid);
                // In agent mode, the protocol is "emit a tool block and nothing
                // after it" — so the moment a complete tool call appears, stop the
                // model generating (it can't see the result mid-stream and would
                // otherwise spiral into redundant calls) and run the round.
                if self.cut_stream.is_none() && self.should_cut_stream(sid) {
                    if let Some(s) = self.sessions.by_id_mut(sid) {
                        s.finalize_assistant_stream();
                    }
                    self.streams.retain(|h| h.session_id != sid);
                    self.sessions.save();
                    self.cut_stream = Some(sid);
                }
                self.touch();
            }
            Action::StreamReasoning(sid, t) => {
                if let Some(s) = self.sessions.by_id_mut(sid) {
                    s.append_reasoning(&t);
                }
                if sid == self.sessions.active_id() {
                    self.chat.stick_bottom = true;
                }
                self.touch();
            }
            Action::StreamUsage(_sid, u) => self.usage = Some(u),
            Action::StreamDone(sid) => {
                if let Some(s) = self.sessions.by_id_mut(sid) {
                    s.finalize_assistant_stream();
                }
                self.streams.retain(|h| h.session_id != sid);
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
                // If the endpoint rejected the native `tools` field, fall back to
                // fenced parsing so the app keeps working (the user resends).
                if looks_like_base_url_error(&e) {
                    // No / invalid endpoint URL — prompt for the URL + key.
                    self.set_status("No valid API endpoint — enter your URL and key.");
                    let ep = self.config.api.endpoint.clone();
                    let key = self.config.api.api_key.clone();
                    self.overlay = Overlay::ApiSetup(crate::app::overlay::ApiSetup::new(ep, key));
                } else if self.config.api.native_tools && looks_like_tools_error(&e) {
                    self.config.api.native_tools = false;
                    let _ = self.config.save();
                    self.set_status("Endpoint rejected native tools — switched to fenced mode. Resend your message.");
                } else {
                    self.set_status(format!("Stream error: {}", e));
                }
                self.sessions.save();
                self.touch();
            }
            Action::CancelStream => {
                // Cancel only the active session's stream.
                let active = self.sessions.active_id();
                self.streams.retain(|h| h.session_id != active);
                self.sessions.active_mut().finalize_assistant_stream();
                self.set_status("Cancelled.");
                self.sessions.save();
                self.touch();
            }

            // ── Transcript scrolling ────────────────────────────────────────
            Action::ChatTop => self.chat.top(self.chat_h()),
            Action::ChatBottom => self.chat.bottom(self.chat_h()),
            Action::ChatPageDown => self.chat.page_down(self.chat_h()),
            Action::ChatPageUp => self.chat.page_up(self.chat_h()),
            Action::ChatHalfDown => self.chat.half_page_down(self.chat_h()),
            Action::ChatHalfUp => self.chat.half_page_up(self.chat_h()),
            Action::ChatScroll(d) => self.chat.scroll_by(d, self.chat_h()),
            Action::ToggleOutput => {
                // The status bar shows an independent `output` chip; don't clobber
                // the free-text status (e.g. "Generating…") with a redundant line.
                self.show_output = !self.show_output;
                self.chat.stick_bottom = true;
                self.touch();
            }
            Action::ChatClick(col, row) => {
                // Map the click to a transcript row and toggle the collapsible
                // tool output whose header sits there. Ignore clicks outside the
                // chat pane or on non-header rows.
                let area = self.layout.chat;
                let inside = col >= area.x
                    && col < area.x + area.width
                    && row >= area.y
                    && row < area.y + area.height;
                if inside {
                    let vp_row = (row - area.y) as usize;
                    if let Some(key) = self.chat.toggle_at_viewport_row(vp_row) {
                        self.chat.toggle_block(key);
                        self.touch();
                    }
                }
            }
            Action::DismissNotice => {
                if matches!(self.overlay, Overlay::Notice { .. }) {
                    self.overlay = Overlay::None;
                }
            }

            // ── External programs (editor / shell) ──────────────────────────
            Action::OpenEditor => {
                self.pending_external = Some(PendingExternal::EditorText(self.conversation_markdown()));
                self.set_status("Opening conversation in $EDITOR…");
            }
            Action::OpenEditPicker => {
                // Toggle: a second press closes the browser.
                if self.overlay.is_browser() {
                    self.overlay = Overlay::None;
                } else {
                    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                    let preselect = self.edited_files.iter().map(PathBuf::from).collect();
                    self.overlay = Overlay::Browser(FileBrowser::open(cwd, BrowsePurpose::Edit, preselect));
                    self.set_status("h/j/k/l navigate · space select · l/⏎ open · h up · Esc close");
                }
            }
            Action::OpenFilesInEditor(paths) => {
                if !paths.is_empty() {
                    let n = paths.len();
                    self.pending_external = Some(PendingExternal::EditorFiles(paths));
                    self.set_status(format!("Opening {} file(s) in $EDITOR…", n));
                }
            }
            Action::OpenShell => {
                self.pending_external = Some(PendingExternal::Shell);
                self.set_status("Opening shell…");
            }

            // ── File browser navigation ─────────────────────────────────────
            Action::BrowserDown => if let Overlay::Browser(b) = &mut self.overlay { b.down() },
            Action::BrowserUp => if let Overlay::Browser(b) = &mut self.overlay { b.up() },
            Action::BrowserParent => if let Overlay::Browser(b) = &mut self.overlay { b.parent() },
            Action::BrowserSelect => if let Overlay::Browser(b) = &mut self.overlay { b.toggle_select() },
            Action::BrowserClose => self.overlay = Overlay::None,
            Action::BrowserOpen => return self.browser_open(),

            // ── Startup launcher ────────────────────────────────────────────
            Action::StartupUp => self.picker_up(),
            Action::StartupDown => self.picker_down(),
            Action::StartupNew => {
                self.overlay = Overlay::None;
                return Some(Action::NewSession);
            }
            Action::StartupConfirm => return self.startup_confirm(),

            // ── Sessions ────────────────────────────────────────────────────
            Action::NewSession => {
                self.sessions.new_session();
                if self.config.ui.agent_default {
                    self.sessions.active_mut().agent_mode = true;
                }
                self.set_status(format!("New session: {}", self.sessions.active().name));
                self.sessions.save();
                self.chat.stick_bottom = true;
                self.touch();
            }
            Action::ForkSession => {
                self.sessions.fork_active();
                self.set_status(format!("Forked → {}", self.sessions.active().name));
                self.sessions.save();
                self.chat.stick_bottom = true;
                self.touch();
            }
            Action::DeleteSession => {
                let name = self.sessions.active().name.clone();
                self.sessions.remove_active();
                self.set_status(format!("Deleted: {}", name));
                self.sessions.save();
                self.touch();
            }
            Action::NextSession => {
                self.sessions.select_next();
                self.chat.stick_bottom = true;
                self.touch();
            }
            Action::PrevSession => {
                self.sessions.select_prev();
                self.chat.stick_bottom = true;
                self.touch();
            }
            Action::OpenSessionPicker => {
                if matches!(&self.overlay, Overlay::Picker(p) if p.kind == PickerKind::Session) {
                    self.overlay = Overlay::None;
                } else {
                    let names: Vec<String> = self.sessions.all().iter().map(|s| s.name.clone()).collect();
                    self.overlay = Overlay::Picker(Picker::sessions(names, self.sessions.active_idx()));
                }
            }
            Action::SelectSession(i) => {
                self.sessions.select(i);
                // Resume in the session's own folder so file tools / @-mentions
                // resolve against the right project.
                let cwd = self.sessions.active().cwd.clone();
                let mut where_ = String::new();
                if let Some(dir) = cwd {
                    if std::env::set_current_dir(&dir).is_ok() {
                        where_ = format!("  ({})", dir.display());
                    }
                }
                self.set_status(format!("Session: {}{}", self.sessions.active().name, where_));
                self.chat.stick_bottom = true;
                self.touch();
            }
            Action::RenameSession(name) => {
                self.sessions.active_mut().name = name.clone();
                self.set_status(format!("Renamed: {}", name));
                self.sessions.save();
            }

            // ── Skills ──────────────────────────────────────────────────────
            Action::OpenSkillPicker => {
                if matches!(&self.overlay, Overlay::Picker(p) if p.kind == PickerKind::Skill) {
                    self.overlay = Overlay::None;
                } else if self.skills.is_empty() {
                    self.set_status(format!("No skills. Add .md files in {}", crate::skills::skills_dir().display()));
                } else {
                    self.overlay = Overlay::Picker(Picker::skills(self.skill_items()));
                    self.set_status("⏎ toggle skill · Esc close · edit ~/.config/aitui/skills/");
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
                self.set_status(format!("Model: {}", m));
            }
            Action::NextModel => {
                if !self.models.is_empty() {
                    self.model_idx = (self.model_idx + 1) % self.models.len();
                    self.set_status(format!("Model: {}", self.current_model()));
                }
            }
            Action::PrevModel => {
                if !self.models.is_empty() {
                    self.model_idx = (self.model_idx + self.models.len() - 1) % self.models.len();
                    self.set_status(format!("Model: {}", self.current_model()));
                }
            }
            Action::ModelsLoaded(models) => {
                if !models.is_empty() {
                    // Keep the current selection if it survived the refresh;
                    // otherwise fall back to the configured default model, then 0.
                    let current = self.current_model().to_string();
                    let default = self.config.api.default_model.clone();
                    self.models = models;
                    self.model_idx = self
                        .models
                        .iter()
                        .position(|m| m == &current)
                        .or_else(|| self.models.iter().position(|m| m == &default))
                        .unwrap_or(0);
                    self.set_status(format!("Loaded {} models", self.models.len()));
                }
            }

            // ── Files / attachment ──────────────────────────────────────────
            Action::OpenFilePicker => {
                // Toggle: a second press closes the browser.
                if self.overlay.is_browser() {
                    self.overlay = Overlay::None;
                } else {
                    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                    self.overlay = Overlay::Browser(FileBrowser::open(cwd, BrowsePurpose::Attach, Vec::new()));
                    self.set_status("h/j/k/l navigate · l/⏎ attach file · h up · Esc close");
                }
            }
            Action::AttachFile(path) => {
                if path.exists() {
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string();
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
            Action::OpenSettings => {
                if matches!(&self.overlay, Overlay::Settings(_)) {
                    self.overlay = Overlay::None;
                } else {
                    let prompt = self.sessions.active().system_prompt.clone().unwrap_or_default();
                    self.overlay = Overlay::Settings(Settings { selected: 0, editing_prompt: false, prompt_buf: prompt });
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
            Action::SettingsLeft => self.settings_adjust(-1),
            Action::SettingsRight => self.settings_adjust(1),

            // ── @ mentions ──────────────────────────────────────────────────
            Action::MentionUp => self.mention.up(),
            Action::MentionDown => self.mention.down(),
            Action::MentionAccept => self.accept_mention(),
            Action::MentionCancel => self.mention.reset(),

            // ── Agent ───────────────────────────────────────────────────────
            Action::ToggleAgentMode => {
                let on = {
                    let s = self.sessions.active_mut();
                    s.agent_mode = !s.agent_mode;
                    s.agent_mode
                };
                self.set_status(if on {
                    "◇ Agent mode ON — model can read/edit/run with your approval"
                } else {
                    "Agent mode OFF"
                });
                self.sessions.save();
            }
            Action::AgentPermitOnce => return self.resolve_permission(Permission::Allow),
            Action::AgentPermitAll => return self.resolve_permission(Permission::AllowAll),
            Action::AgentDenyOnce => return self.resolve_permission(Permission::Deny),
            Action::AgentDenyAll => return self.resolve_permission(Permission::DenyAll),
            Action::AgentToolResult(result) => {
                self.agent_tool_rx = None;
                self.record_tool_result(result);
                return self.process_next_tool();
            }
            Action::AgentCancel => {
                self.overlay = Overlay::None;
                self.pending_tools.clear();
                self.agent_iterations = 0;
                self.set_status("Agent round cancelled");
            }

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
                if !s.editing_prompt {
                    s.selected = s.selected.saturating_sub(1);
                }
            }
            Overlay::Permission(r) => r.up(),
            Overlay::Startup(s) => s.up(),
            Overlay::ApiSetup(a) => a.next_field(),
            Overlay::Browser(_) | Overlay::Notice { .. } | Overlay::None => {}
        }
    }
    fn picker_down(&mut self) {
        match &mut self.overlay {
            Overlay::Picker(p) => p.down(),
            Overlay::Palette(p) => p.down(),
            Overlay::Settings(s) => {
                if !s.editing_prompt && s.selected + 1 < SettingsRow::all().len() {
                    s.selected += 1;
                }
            }
            Overlay::Permission(r) => r.down(),
            Overlay::Startup(s) => s.down(),
            Overlay::ApiSetup(a) => a.next_field(),
            Overlay::Browser(_) | Overlay::Notice { .. } | Overlay::None => {}
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
            Overlay::Settings(s) if s.editing_prompt => s.prompt_buf.push(c),
            Overlay::ApiSetup(a) => a.push(c),
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
            Overlay::Settings(s) if s.editing_prompt => {
                s.prompt_buf.pop();
            }
            Overlay::ApiSetup(a) => a.backspace(),
            _ => {}
        }
        None
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
        if !ep.is_empty() {
            self.mock = false;
        }
        self.set_status(if ep.is_empty() {
            "API endpoint cleared — mock mode".to_string()
        } else {
            format!("API endpoint set: {}", ep)
        });
    }

        // ── Input history helpers (shell-style up/down) ───────────────────

    fn input_history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        // First press: save the live draft and move to the newest entry.
        if self.input_history_idx.is_none() {
            self.input_draft = self.input.text();
            self.input_history_idx = Some(self.input_history.len() - 1);
        } else {
            let i = self.input_history_idx.unwrap();
            if i > 0 {
                self.input_history_idx = Some(i - 1);
            }
        }
        let text = self.input_history[self.input_history_idx.unwrap()].clone();
        self.input.set_text(&text);
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

    /// Open/attach the current target(s) in the file browser. Folders descend.
    fn browser_open(&mut self) -> Option<Action> {
        let Overlay::Browser(b) = &mut self.overlay else { return None };
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
                PickerKind::Model => p.selected_item().map(|m| Action::SelectModel(m.to_string())),
                PickerKind::Session => p.selected_index().map(Action::SelectSession),
                PickerKind::Skill => None,
            },
            Overlay::Palette(p) => p.selected_cmd().map(|c| Action::RunCommand(c.run.to_string())),
            Overlay::Permission(r) => self.resolve_permission(r.permission()),
            Overlay::Settings(s) => {
                self.overlay = Overlay::Settings(s);
                self.settings_confirm();
                None
            }
            Overlay::Browser(b) => {
                self.overlay = Overlay::Browser(b);
                self.browser_open()
            }
            Overlay::Startup(s) => {
                self.overlay = Overlay::Startup(s);
                self.startup_confirm()
            }
            Overlay::ApiSetup(a) => {
                self.overlay = Overlay::ApiSetup(a);
                self.apply_api_setup();
                None
            }
            Overlay::Notice { .. } | Overlay::None => None,
        }
    }

    /// Resolve a launch-screen choice: a new session, or resume the selected one
    /// (which `cd`s to that session's saved folder via `SelectSession`).
    fn startup_confirm(&mut self) -> Option<Action> {
        let sel = match &self.overlay {
            Overlay::Startup(s) => s.selected,
            _ => return None,
        };
        self.overlay = Overlay::None;
        if sel == 0 {
            Some(Action::NewSession)
        } else {
            Some(Action::SelectSession(sel - 1))
        }
    }

    fn settings_confirm(&mut self) {
        let Overlay::Settings(s) = &self.overlay else { return };
        let row = SettingsRow::all().get(s.selected).copied();
        match row {
            Some(SettingsRow::SystemPrompt) => {
                if let Overlay::Settings(s) = &mut self.overlay {
                    if s.editing_prompt {
                        let buf = s.prompt_buf.clone();
                        let prompt = if buf.trim().is_empty() { None } else { Some(buf) };
                        s.editing_prompt = false;
                        self.sessions.active_mut().system_prompt = prompt;
                        self.sessions.save();
                    } else {
                        s.editing_prompt = true;
                    }
                }
            }
            Some(SettingsRow::AgentDefault) | Some(SettingsRow::AutoApprove) => self.settings_adjust(0),
            _ => {}
        }
    }

    fn settings_adjust(&mut self, dir: i32) {
        let Overlay::Settings(s) = &self.overlay else { return };
        let Some(row) = SettingsRow::all().get(s.selected).copied() else { return };
        match row {
            SettingsRow::AgentDefault => self.config.ui.agent_default = !self.config.ui.agent_default,
            SettingsRow::AutoApprove => {
                self.config.ui.auto_approve_reads = !self.config.ui.auto_approve_reads;
                crate::app::overlay::sync_auto_approvals(&mut self.permissions, self.config.ui.auto_approve_reads);
            }
            SettingsRow::InputHeight => {
                let h = self.config.ui.input_height as i32 + dir;
                self.config.ui.input_height = h.clamp(2, 20) as u16;
            }
            SettingsRow::SystemPrompt => {}
        }
        let _ = self.config.save();
    }

    // ── : commands ──────────────────────────────────────────────────────────

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
                self.sessions.active_mut().messages.clear();
                self.chat.stick_bottom = true;
                self.touch();
                self.set_status("Chat cleared");
            }
            "models" | "model" => return Some(Action::OpenModelPicker),
            "files" | "attach" => return Some(Action::OpenFilePicker),
            "detach" | "noattach" => return Some(Action::ClearAttachment),
            "agent" | "agentmode" => return Some(Action::ToggleAgentMode),
            "mock" | "test" | "offline" => {
                self.mock = !self.mock;
                self.set_status(if self.mock {
                    "Mock mode ON — type 'help' then send to drive the agent offline"
                } else {
                    "Mock mode OFF — using the live API"
                });
            }
            "native" | "nativetools" => {
                self.config.api.native_tools = !self.config.api.native_tools;
                let on = self.config.api.native_tools;
                let _ = self.config.save();
                self.set_status(format!(
                    "Native tool-calling: {}",
                    if on { "ON (structured tool_calls)" } else { "off (```tool fences)" }
                ));
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
                self.set_status(format!("Sticky skills: {}", if on { "ON (remembered across restarts)" } else { "off" }));
            }
            "effort" | "reasoning" => {
                // Cycle none → low → medium → high → none.
                self.reasoning_effort = match self.reasoning_effort.as_deref() {
                    None => Some("low".into()),
                    Some("low") => Some("medium".into()),
                    Some("medium") => Some("high".into()),
                    _ => None,
                };
                self.set_status(format!(
                    "Reasoning effort: {}",
                    self.reasoning_effort.as_deref().unwrap_or("off")
                ));
            }
            other if other.starts_with("effort ") => {
                let lvl = other[7..].trim().to_lowercase();
                self.reasoning_effort = match lvl.as_str() {
                    "off" | "none" | "" => None,
                    "low" | "medium" | "high" => Some(lvl),
                    _ => {
                        self.set_status("Usage: :effort [low|medium|high|off]");
                        return None;
                    }
                };
                self.set_status(format!(
                    "Reasoning effort: {}",
                    self.reasoning_effort.as_deref().unwrap_or("off")
                ));
            }
            "editor" | "history" => return Some(Action::OpenEditor),
            "edit" | "e" | "edited" => return Some(Action::OpenEditPicker),
            "shell" | "term" | "terminal" | "sh" => return Some(Action::OpenShell),
            "?" | "help" => return Some(Action::ToggleHelp),
            "nosystem" | "system" => return Some(Action::SetSystemPrompt(None)),
            other if other.starts_with("model ") => {
                return Some(Action::SelectModel(other[6..].trim().to_string()))
            }
            other if other.starts_with("edit ") || other.starts_with("e ") => {
                let p = other.splitn(2, ' ').nth(1).unwrap_or("").trim();
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
fn looks_like_base_url_error(err: &str) -> bool {
    let e = err.to_lowercase();
    e.contains("relative url without a base")
        || e.contains("without a base")
        || e.contains("builder error")
        || e.contains("no api client")
}

fn looks_like_tools_error(err: &str) -> bool {
    let e = err.to_lowercase();
    let mentions_tools = e.contains("tool") || e.contains("function");
    let rejected = e.contains("400")
        || e.contains("not supported")
        || e.contains("unsupported")
        || e.contains("does not support")
        || e.contains("unknown field")
        || e.contains("unrecognized");
    mentions_tools && rejected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::input_buffer::InputBuffer;
    use crate::app::overlay::{Mention, Overlay, Picker};
    use crate::app::state::PanelLayout;
    use crate::config::Config;
    use crate::domain::session::SessionManager;
    use crate::input::vim::VimMode;
    use crate::render::chat::ChatState;
    use std::collections::VecDeque;

    fn test_app() -> App {
        let mut config = Config::default();
        config.ui.agent_default = false;
        let keymap = crate::input::keymap::Keymap::from_config(&config.keybinds);
        let (spec_tx, spec_rx) = tokio::sync::mpsc::channel(64);
        App {
            config,
            keymap,
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
            overlay: Overlay::None,
            mention: Mention::default(),
            pastes: Vec::new(),
            models: vec!["gemini-2.5-flash".into(), "claude-sonnet-4-6".into()],
            model_idx: 0,
            attachment: None,
            status: None,
            show_help: false,
            should_quit: false,
            yank: None,
            last_insert: None,
            show_output: false,
            mock: false,
            edited_files: Vec::new(),
            pending_external: None,
            usage: None,
            skills: Vec::new(),
            reasoning_effort: None,
            content_rev: 0,
            permissions: crate::agent::PermissionMemory::default(),
            pending_tools: VecDeque::new(),
            agent_iterations: 0,
            streams: Vec::new(),
            agent_session: None,
            agent_queue: VecDeque::new(),
            agent_tool_rx: None,
            models_rx: None,
            spec_results: std::collections::HashMap::new(),
            spec_dispatched: std::collections::HashSet::new(),
            spec_epoch: 0,
            cut_stream: None,
            spec_tx,
            spec_rx,
            mention_files: Vec::new(),
            mention_files_at: None,
            layout: PanelLayout::default(),
            api: None,
        }
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
    fn enter_command_sets_mode_and_clears_buffer() {
        let mut app = test_app();
        app.command = "old".into();
        app.apply(Action::EnterCommand);
        assert_eq!(app.vim, VimMode::Command);
        assert!(app.command.is_empty());
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
    fn insert_char_in_command_mode_appends_to_command() {
        let mut app = test_app();
        app.vim = VimMode::Command;
        app.apply(Action::InsertChar('w'));
        assert_eq!(app.command, "w");
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
    fn backspace_in_command_mode_pops_command() {
        let mut app = test_app();
        app.vim = VimMode::Command;
        app.command = "wr".into();
        app.apply(Action::Backspace);
        assert_eq!(app.command, "w");
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
    fn yank_line_copies_and_sets_status() {
        let mut app = test_app();
        app.input.paste("yank me");
        app.apply(Action::YankLine);
        assert_eq!(app.yank.as_deref(), Some("yank me"));
        assert!(app.status.is_some());
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
    fn medium_paste_chips_and_expands_on_send() {
        let mut app = test_app();
        app.vim = VimMode::Insert;
        let blob = (0..10).map(|i| format!("line{}", i)).collect::<Vec<_>>().join("\n");
        app.apply(Action::PasteText(blob.clone()));
        // The composer shows a compact chip, not the raw blob.
        assert!(app.input.text().contains("[PASTED#1-10lines-"));
        assert!(!app.input.text().contains("line5"));
        assert_eq!(app.pastes.len(), 1);
        // Sending expands the chip back to the full text and clears the store.
        let _ = app.submit();
        let sent = app.sessions.active().messages.iter().rev().find(|m| m.role == "user").unwrap();
        let text = match &sent.content {
            crate::api::models::MessageContent::Text(t) => t.clone(),
            _ => String::new(),
        };
        assert!(text.contains("line5"), "full pasted text must be restored on send");
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

    // ── Startup launcher ─────────────────────────────────────────────────────────

    #[test]
    fn startup_confirm_new_option_starts_new_session() {
        let mut app = test_app();
        app.overlay = Overlay::Startup(crate::app::overlay::Startup::new(1));
        let follow = app.apply(Action::StartupConfirm);
        assert!(matches!(follow, Some(Action::NewSession)));
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[test]
    fn startup_confirm_resume_selects_session() {
        let mut app = test_app();
        let mut s = crate::app::overlay::Startup::new(2);
        s.selected = 1; // first session (index 0)
        app.overlay = Overlay::Startup(s);
        let follow = app.apply(Action::StartupConfirm);
        assert!(matches!(follow, Some(Action::SelectSession(0))));
    }

    #[test]
    fn startup_new_action_closes_and_creates() {
        let mut app = test_app();
        app.overlay = Overlay::Startup(crate::app::overlay::Startup::new(1));
        let follow = app.apply(Action::StartupNew);
        assert!(matches!(follow, Some(Action::NewSession)));
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[test]
    fn startup_nav_stays_in_bounds() {
        let mut app = test_app();
        app.overlay = Overlay::Startup(crate::app::overlay::Startup::new(1)); // 2 options
        app.apply(Action::StartupUp); // clamped at 0
        if let Overlay::Startup(s) = &app.overlay {
            assert_eq!(s.selected, 0);
        }
        app.apply(Action::StartupDown);
        app.apply(Action::StartupDown); // clamp at last (index 1)
        if let Overlay::Startup(s) = &app.overlay {
            assert_eq!(s.selected, 1);
        }
    }

    // ── Skills ─────────────────────────────────────────────────────────────────

    #[test]
    fn toggle_skill_flips_active_and_refreshes_picker() {
        let mut app = test_app();
        app.config.ui.sticky_skills = false; // don't touch disk in tests
        app.skills = vec![
            crate::skills::Skill { name: "caveman".into(), desc: "terse".into(), body: "be terse".into(), active: false },
        ];
        app.apply(Action::OpenSkillPicker);
        assert!(matches!(app.overlay, Overlay::Picker(_)));
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
        let mut app = test_app();
        app.skills.clear();
        app.apply(Action::OpenSkillPicker);
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

    // ── Sessions ─────────────────────────────────────────────────────────────────

    #[test]
    fn open_session_picker_sets_overlay() {
        let mut app = test_app();
        app.apply(Action::OpenSessionPicker);
        assert!(matches!(app.overlay, Overlay::Picker(_)));
    }

    #[test]
    fn open_editor_sets_request() {
        let mut app = test_app();
        app.sessions.active_mut().push_message(crate::api::ChatMessage::user("hi there"));
        app.apply(Action::OpenEditor);
        match app.pending_external {
            Some(crate::app::state::PendingExternal::EditorText(ref t)) => assert!(t.contains("hi there")),
            _ => panic!("expected EditorText request"),
        }
    }

    #[test]
    fn open_files_in_editor_sets_external() {
        let mut app = test_app();
        app.apply(Action::OpenFilesInEditor(vec![std::path::PathBuf::from("src/main.rs")]));
        assert!(matches!(app.pending_external, Some(crate::app::state::PendingExternal::EditorFiles(_))));
    }

    #[test]
    fn open_shell_sets_external() {
        let mut app = test_app();
        app.apply(Action::OpenShell);
        assert!(matches!(app.pending_external, Some(crate::app::state::PendingExternal::Shell)));
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
        let call = ToolCall { name: "write_file".into(), args: serde_json::json!({"path": "./src/x.rs"}), id: None };
        app.apply(Action::AgentToolResult(ToolResult::success(call, "ok".into(), 1)));
        assert_eq!(app.edited_files, vec!["src/x.rs".to_string()]);
    }

    #[test]
    fn delete_removes_from_edited_files() {
        use crate::agent::{ToolCall, ToolResult};
        let mut app = test_app();
        app.edited_files = vec!["src/x.rs".into()];
        let call = ToolCall { name: "delete_file".into(), args: serde_json::json!({"path": "src/x.rs"}), id: None };
        app.apply(Action::AgentToolResult(ToolResult::success(call, "deleted".into(), 1)));
        assert!(app.edited_files.is_empty());
    }


    #[test]
    fn submit_blocked_while_busy_keeps_input_and_shows_notice() {
        let mut app = test_app();
        app.input.set_text("hello");
        // Simulate an in-flight stream for the active session → busy.
        let sid = app.sessions.active_id();
        app.streams.push(crate::app::state::StreamHandle { session_id: sid, rx: tokio::sync::mpsc::channel(1).1 });
        assert!(app.is_busy());
        let out = app.submit();
        assert!(out.is_none(), "must not start a new stream while busy");
        assert!(matches!(app.overlay, Overlay::Notice { .. }), "a busy notice should show");
        assert_eq!(app.input.take(), "hello", "the composed text must be preserved");
    }

    #[test]
    fn submit_when_idle_sends() {
        let mut app = test_app();
        app.input.set_text("hi there");
        assert!(!app.is_busy());
        let _ = app.submit();
        // The user message was pushed (a real stream would attach in the app; the
        // test harness has no API client so the turn finalizes immediately).
        assert!(app.sessions.active().messages.iter().any(|m| m.role == "user"));
        assert!(!matches!(app.overlay, Overlay::Notice { .. }), "idle send must not show the busy notice");
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
        assert!(matches!(app.overlay, Overlay::Permission(_)), "write should prompt for permission");
    }

    #[test]
    fn dismiss_notice_closes_it() {
        let mut app = test_app();
        app.overlay = Overlay::Notice { title: "t".into(), body: "b".into() };
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
        app.sessions.active_mut().push_message(crate::api::ChatMessage::user("test"));
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
    fn command_history_navigation() {
        let mut app = test_app();
        app.apply(Action::RunCommand("w".into()));
        app.apply(Action::RunCommand("q".into()));
        app.apply(Action::CommandHistoryPrev);
        assert_eq!(app.command, "q");
        app.apply(Action::CommandHistoryPrev);
        assert_eq!(app.command, "w");
        app.apply(Action::CommandHistoryNext);
        assert_eq!(app.command, "q");
        app.apply(Action::CommandHistoryNext);
        assert!(app.command.is_empty());
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
        let call = ToolCall { name: "read_file".into(), args: serde_json::json!({"path": "x"}), id: None };
        app.permissions.remember_allow(call.kind().unwrap());
        // A result pre-run while the reply streamed is stashed under its call sig.
        app.store_spec_result(app.spec_epoch, ToolResult::success(call.clone(), "file contents".into(), 1));
        app.pending_tools.push_back(call);
        app.agent_session = Some(app.sessions.active_id());

        let _ = app.process_next_tool();

        // The cached result was used directly — no async tool execution spawned.
        assert!(app.agent_tool_rx.is_none(), "must not respawn a pre-run tool");
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
        let call = ToolCall { name: "read_file".into(), args: serde_json::json!({"path": "x"}), id: None };
        let stale = app.spec_epoch;
        app.spec_epoch = app.spec_epoch.wrapping_add(1); // turn moved on
        app.store_spec_result(stale, ToolResult::success(call, "old".into(), 1));
        assert!(app.spec_results.is_empty(), "a result from a past turn must be dropped");
    }

    #[test]
    fn tools_error_detection() {
        assert!(looks_like_tools_error("API error 400: model does not support tools"));
        assert!(looks_like_tools_error("unknown field `tools`, expected one of ..."));
        assert!(looks_like_tools_error("function calling is unsupported here"));
        assert!(!looks_like_tools_error("API error 500: internal error"));
        assert!(!looks_like_tools_error("connection refused"));
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
        assert!(looks_like_base_url_error("Request failed: builder error: relative url without a base"));
        assert!(looks_like_base_url_error("No API client"));
        assert!(!looks_like_base_url_error("API error 500: internal"));
    }

    #[test]
    fn native_command_toggles_config() {
        let mut app = test_app();
        let before = app.config.api.native_tools;
        app.apply(Action::RunCommand("native".into()));
        assert_eq!(app.config.api.native_tools, !before);
    }

    #[test]
    fn toggle_agent_mode_switches_and_sets_status() {
        let mut app = test_app();
        app.apply(Action::ToggleAgentMode);
        assert!(app.sessions.active().agent_mode);
        assert!(app.status.is_some());
        app.apply(Action::ToggleAgentMode);
        assert!(!app.sessions.active().agent_mode);
    }

    // ── Models ─────────────────────────────────────────────────────────────────

    #[test]
    fn next_model_cycles_forward() {
        let mut app = test_app();
        assert_eq!(app.model_idx, 0);
        app.apply(Action::NextModel);
        assert_eq!(app.model_idx, 1);
        app.apply(Action::NextModel);
        assert_eq!(app.model_idx, 0); // wraps
    }

    #[test]
    fn prev_model_cycles_backward() {
        let mut app = test_app();
        app.apply(Action::PrevModel);
        assert_eq!(app.model_idx, 1);
    }

    #[test]
    fn select_model_finds_or_appends() {
        let mut app = test_app();
        app.apply(Action::SelectModel("gemini-2.5-flash".into()));
        assert_eq!(app.model_idx, 0);
        app.apply(Action::SelectModel("new-model".into()));
        assert_eq!(app.model_idx, 2);
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
    fn chat_click_toggles_individual_block_header() {
        use ratatui::layout::Rect;
        let mut app = test_app();
        app.layout.chat = Rect { x: 0, y: 0, width: 80, height: 24 };
        let (rows, header_idx, key) = collapsible_tool_doc();
        app.chat.stick_bottom = false;
        app.chat.scroll = 0;
        app.chat.set_doc(rows, 1, 80, 24);
        app.chat.scroll = 0; // view the top so the header maps to its row directly

        assert!(!app.chat.toggled.contains(&key));
        app.apply(Action::ChatClick(5, header_idx as u16)); // click the header row
        assert!(app.chat.toggled.contains(&key), "click should flip the block");
        assert_eq!(app.chat.focus_msg, Some(key.0), "click should focus that message");
        app.apply(Action::ChatClick(5, header_idx as u16));
        assert!(!app.chat.toggled.contains(&key), "second click flips back");
    }

    #[test]
    fn chat_click_on_non_header_row_does_not_toggle() {
        use ratatui::layout::Rect;
        let mut app = test_app();
        app.layout.chat = Rect { x: 0, y: 0, width: 80, height: 24 };
        let (rows, header_idx, _key) = collapsible_tool_doc();
        assert!(header_idx > 0, "there should be a role-label row before the header");
        assert!(rows[0].toggle.is_none());
        app.chat.stick_bottom = false;
        app.chat.set_doc(rows, 1, 80, 24);
        app.chat.scroll = 0;
        app.apply(Action::ChatClick(3, 0));
        assert!(app.chat.toggled.is_empty(), "clicking a non-header row does nothing");
    }

    #[test]
    fn chat_click_outside_pane_is_ignored() {
        use ratatui::layout::Rect;
        let mut app = test_app();
        app.layout.chat = Rect { x: 0, y: 0, width: 80, height: 24 };
        let (rows, _idx, _key) = collapsible_tool_doc();
        app.chat.set_doc(rows, 1, 80, 24);
        app.apply(Action::ChatClick(5, 100)); // row 100 is below the pane
        assert!(app.chat.toggled.is_empty());
    }

    /// A document whose only collapsible header is a long (>6 line) tool result.
    /// Returns the rows, the header's row index, and its `(msg, block)` key.
    fn collapsible_tool_doc() -> (Vec<crate::render::document::RenderedLine>, usize, (usize, usize)) {
        use crate::domain::blocks::Block;
        use crate::render::document::{build, DocMessage};
        use std::collections::HashSet;
        let output = (0..10).map(|i| format!("out {}", i)).collect::<Vec<_>>().join("\n");
        let msgs = vec![DocMessage {
            role: "tool".into(),
            blocks: vec![Block::ToolResult { ok: true, summary: "Shell x".into(), output }],
        }];
        let rows = build(&msgs, 80, &crate::render::theme::Theme::default(), &HashSet::new(), false, false);
        let idx = rows.iter().position(|r| r.toggle.is_some()).expect("a collapsible header");
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
        app.apply(Action::AttachFile(std::path::PathBuf::from("/must/not/exist/xyz")));
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
        app.streams.push(crate::app::state::StreamHandle { session_id: sid, rx: tokio::sync::mpsc::channel(1).1 });
    }

    #[test]
    fn stream_token_updates_session_and_touches() {
        let mut app = test_app();
        app.sessions.active_mut().begin_assistant_stream();
        let sid = app.sessions.active_id();
        let rev = app.content_rev;
        app.apply(Action::StreamToken(sid, "hello".into()));
        assert_eq!(app.sessions.active().streaming_display().as_deref(), Some("hello"));
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
        assert_eq!(app.cut_stream, Some(sid), "a complete tool call must cut the stream");
        assert!(!app.sessions.active().is_streaming());
        assert!(app.streams.is_empty());
        assert!(
            app.sessions.active().messages.last().is_some_and(|m| matches!(
                &m.content,
                crate::api::models::MessageContent::Text(t) if t.contains("list_dir")
            )),
            "the finalized turn keeps the tool call",
        );
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
        assert_eq!(app.cut_stream, None, "non-agent mode must keep streaming normally");
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
        app.sessions.active_mut().push_message(crate::api::ChatMessage::user("hi"));
        let before = app.sessions.all().len();
        app.apply(Action::ForkSession);
        assert_eq!(app.sessions.all().len(), before + 1);
        // The fork carries the original's messages and is now active.
        assert!(app.sessions.active().messages.iter().any(|m| m.role == "user"));
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
        assert_eq!(app.sessions.by_id(a).unwrap().streaming_display().as_deref(), Some("from-a"));
        assert!(app.sessions.by_id(b).unwrap().streaming_display().is_none());
    }

    // ── System prompt ──────────────────────────────────────────────────────────

    #[test]
    fn set_system_prompt_updates_session() {
        let mut app = test_app();
        app.apply(Action::SetSystemPrompt(Some("Be concise".into())));
        assert_eq!(app.sessions.active().system_prompt.as_deref(), Some("Be concise"));
    }

    #[test]
    fn set_system_prompt_clears_with_none() {
        let mut app = test_app();
        app.sessions.active_mut().system_prompt = Some("old".into());
        app.apply(Action::SetSystemPrompt(None));
        assert!(app.sessions.active().system_prompt.is_none());
    }

}
