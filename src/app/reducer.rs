//! The reducer: applies one `Action` to the `App`, optionally returning a
//! follow-up action. All mutation funnels through here so behaviour is easy to
//! trace and test.

use std::path::PathBuf;

use crate::agent::Permission;
use crate::app::action::{Action, Dir};
use crate::app::overlay::{
    list_picker_dir, Overlay, Picker, PickerKind, Settings, SettingsRow,
};
use crate::app::state::{list_dir_entries, App, Focus};
use crate::input::vim::VimMode;

impl App {
    fn chat_h(&self) -> usize {
        self.layout.chat.height.saturating_sub(2) as usize
    }

    pub fn apply(&mut self, action: Action) -> Option<Action> {
        match action {
            Action::Quit => self.should_quit = true,
            Action::Resize => {}
            Action::ClearStatus => self.status = None,
            Action::ToggleHelp => self.show_help = !self.show_help,
            Action::ToggleSidebar => self.sidebar_collapsed = !self.sidebar_collapsed,

            // ── Modes ───────────────────────────────────────────────────────
            Action::EnterInsert => {
                self.vim = VimMode::Insert;
                self.focus = Focus::Input;
                self.status = None;
                self.pending_escape = None;
            }
            Action::EnterNormal => {
                self.vim = VimMode::Normal;
                self.command.clear();
                self.input.clamp_normal();
                self.pending_escape = None;
                self.mention.reset();
            }
            Action::EnterVisual => self.vim = VimMode::Visual,
            Action::EnterCommand => {
                self.vim = VimMode::Command;
                self.command.clear();
                self.command_history_idx = None;
                self.focus = Focus::Input;
            }
            Action::EnterOperator(op) => self.vim = VimMode::Operator(op),
            Action::SetPendingEscape(c) => self.pending_escape = Some(c),

            // ── Input editing ───────────────────────────────────────────────
            Action::InsertChar(c) => {
                self.pending_escape = None;
                if self.vim == VimMode::Command {
                    self.command.push(c);
                } else {
                    self.input.insert_char(c);
                    self.update_mention();
                }
            }
            Action::Newline => {
                self.mention.reset();
                self.input.insert_newline();
            }
            Action::Backspace => {
                if self.vim == VimMode::Command {
                    self.command.pop();
                } else {
                    self.input.backspace();
                    self.update_mention();
                }
            }
            Action::DeleteAt => self.input.delete_at(),
            Action::DeleteLine => self.input.delete_line(),
            Action::DeleteWordForward => self.input.delete_word_forward(),
            Action::YankLine => {
                self.yank = Some(self.input.yank_line());
                self.set_status("Line yanked");
            }
            Action::Paste => {
                if let Some(t) = self.yank.clone() {
                    self.input.paste(&t);
                }
            }
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
            Action::AttachStream(rx) => self.stream_rx = Some(rx),
            Action::StreamToken(t) => {
                self.sessions.active_mut().append_stream_token(&t);
                self.chat.stick_bottom = true;
                self.touch();
            }
            Action::StreamReasoning(t) => {
                self.sessions.active_mut().append_reasoning(&t);
                self.chat.stick_bottom = true;
                self.touch();
            }
            Action::StreamDone => {
                self.sessions.active_mut().finalize_assistant_stream();
                self.stream_rx = None;
                self.status = None;
                self.sessions.save();
                self.touch();
                if self.sessions.active().agent_mode {
                    return self.start_agent_round();
                }
            }
            Action::StreamError(e) => {
                self.sessions.active_mut().finalize_assistant_stream();
                self.stream_rx = None;
                self.set_status(format!("Stream error: {}", e));
                self.sessions.save();
                self.touch();
            }
            Action::CancelStream => {
                self.stream_rx = None;
                self.sessions.active_mut().finalize_assistant_stream();
                self.set_status("Cancelled.");
                self.sessions.save();
                self.touch();
            }

            // ── Chat navigation ─────────────────────────────────────────────
            Action::ChatDown => self.chat.down(self.chat_h()),
            Action::ChatUp => self.chat.up(self.chat_h()),
            Action::ChatLeft => self.chat.left(self.chat_h()),
            Action::ChatRight => self.chat.right(self.chat_h()),
            Action::ChatWordForward => self.chat.word_forward(self.chat_h()),
            Action::ChatWordBackward => self.chat.word_backward(self.chat_h()),
            Action::ChatLineStart => self.chat.line_start(),
            Action::ChatLineEnd => self.chat.line_end(),
            Action::ChatTop => self.chat.top(self.chat_h()),
            Action::ChatBottom => self.chat.bottom(self.chat_h()),
            Action::ChatPageDown => self.chat.page_down(self.chat_h()),
            Action::ChatPageUp => self.chat.page_up(self.chat_h()),
            Action::ChatScroll(d) => self.chat.scroll_by(d, self.chat_h()),
            Action::ChatToggle => {
                if self.chat.toggle_current() {
                    self.touch();
                }
            }
            Action::ChatYank => {
                if let Some(line) = self.chat.yank_line() {
                    self.yank = Some(line.clone());
                    self.set_status(format!("Yanked: {}", truncate(&line, 48)));
                }
            }
            Action::ChatOpenLink => return self.open_cursor_link(),

            // ── Focus ───────────────────────────────────────────────────────
            Action::FocusChat => {
                self.focus = Focus::Chat;
            }
            Action::FocusSidebar => self.focus = Focus::Sidebar,
            Action::FocusInput => {
                self.focus = Focus::Input;
                self.vim = VimMode::Normal;
            }
            Action::CycleFocus => {
                self.focus = match self.focus {
                    Focus::Input => Focus::Sidebar,
                    Focus::Sidebar => Focus::Chat,
                    Focus::Chat => Focus::Input,
                };
                if self.focus == Focus::Input {
                    self.vim = VimMode::Normal;
                }
            }

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
            Action::SelectSession(i) => {
                self.sessions.select(i);
                self.focus = Focus::Sidebar;
                self.chat.stick_bottom = true;
                self.touch();
            }
            Action::RenameSession(name) => {
                self.sessions.active_mut().name = name.clone();
                self.set_status(format!("Renamed: {}", name));
                self.sessions.save();
            }

            // ── Models ──────────────────────────────────────────────────────
            Action::OpenModelPicker => {
                self.overlay = Overlay::Picker(Picker::models(self.models.clone()));
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
                    let current = self.current_model().to_string();
                    self.models = models;
                    self.model_idx = self.models.iter().position(|m| m == &current).unwrap_or(0);
                    self.set_status(format!("Loaded {} models", self.models.len()));
                }
            }

            // ── Files / attachment ──────────────────────────────────────────
            Action::OpenFilePicker => {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let items = list_dir_entries(&cwd);
                self.overlay = Overlay::Picker(Picker::files(cwd, items));
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
                self.mention.reset();
                self.overlay = Overlay::Palette(crate::app::overlay::Palette::new());
            }
            Action::OpenSettings => {
                let prompt = self.sessions.active().system_prompt.clone().unwrap_or_default();
                self.overlay = Overlay::Settings(Settings { selected: 0, editing_prompt: false, prompt_buf: prompt });
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

            Action::MouseClick(x, y) => return self.mouse_click(x, y),
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
            Overlay::None => {}
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
            Overlay::None => {}
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
            _ => {}
        }
    }
    fn picker_backspace(&mut self) -> Option<Action> {
        match &mut self.overlay {
            Overlay::Picker(p) => {
                if p.query.is_empty() && p.kind == PickerKind::File {
                    if let Some(parent) = p.dir.parent().map(|x| x.to_path_buf()) {
                        let items = list_dir_entries(&parent);
                        *p = Picker::files(parent, items);
                    }
                } else {
                    p.query.pop();
                    p.refilter();
                }
            }
            Overlay::Palette(p) => {
                p.query.pop();
                p.refilter();
            }
            Overlay::Settings(s) if s.editing_prompt => {
                s.prompt_buf.pop();
            }
            _ => {}
        }
        None
    }

    fn picker_confirm(&mut self) -> Option<Action> {
        match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Picker(p) => match p.kind {
                PickerKind::File => {
                    if let Some(item) = p.selected_item() {
                        let path = p.dir.join(item);
                        if path.is_dir() {
                            let items = list_dir_entries(&path);
                            self.overlay = Overlay::Picker(Picker::files(path, items));
                            return None;
                        }
                        return Some(Action::AttachFile(path));
                    }
                    None
                }
                PickerKind::Model => p.selected_item().map(|m| Action::SelectModel(m.to_string())),
            },
            Overlay::Palette(p) => p.selected_cmd().map(|c| Action::RunCommand(c.run.to_string())),
            Overlay::Permission(r) => self.resolve_permission(r.permission()),
            Overlay::Settings(s) => {
                self.overlay = Overlay::Settings(s);
                self.settings_confirm();
                None
            }
            Overlay::None => None,
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
            SettingsRow::SidebarWidth => {
                let w = self.config.ui.sidebar_width as i32 + dir * 2;
                self.config.ui.sidebar_width = w.clamp(16, 60) as u16;
            }
            SettingsRow::InputHeight => {
                let h = self.config.ui.input_height as i32 + dir;
                self.config.ui.input_height = h.clamp(2, 20) as u16;
            }
            SettingsRow::SystemPrompt => {}
        }
        let _ = self.config.save();
    }

    // ── Link opening ────────────────────────────────────────────────────────

    fn open_cursor_link(&mut self) -> Option<Action> {
        let line = self.chat.yank_line().unwrap_or_default();
        let links = crate::render::document::extract_links(&line);
        match links.into_iter().next() {
            Some(url) => {
                let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
                self.set_status(format!("Opening {}", url));
            }
            None => self.set_status("No link on this line"),
        }
        None
    }

    // ── Mouse ───────────────────────────────────────────────────────────────

    fn mouse_click(&mut self, x: u16, y: u16) -> Option<Action> {
        let hit = |r: ratatui::layout::Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;
        let l = self.layout;
        if l.toggle.width > 0 && hit(l.toggle) {
            return Some(Action::ToggleSidebar);
        }
        if hit(l.sidebar) {
            self.focus = Focus::Sidebar;
            let rel = y.saturating_sub(l.sidebar.y + 1);
            if rel >= 4 {
                let idx = (rel - 4) as usize / 2;
                if idx < self.sessions.all().len() {
                    return Some(Action::SelectSession(idx));
                }
            }
            return None;
        }
        if hit(l.chat) {
            self.focus = Focus::Chat;
            return None;
        }
        if hit(l.input) {
            self.focus = Focus::Input;
            self.vim = VimMode::Insert;
        }
        None
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
            "w" | "write" => return Some(Action::Submit),
            "wq" => {
                let r = self.submit();
                self.should_quit = true;
                return r;
            }
            "new" | "n" => return Some(Action::NewSession),
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
            "settings" | "config" | "set" => return Some(Action::OpenSettings),
            "sidebar" => return Some(Action::ToggleSidebar),
            "?" | "help" => return Some(Action::ToggleHelp),
            "nosystem" | "system" => return Some(Action::SetSystemPrompt(None)),
            other if other.starts_with("model ") => {
                return Some(Action::SelectModel(other[6..].trim().to_string()))
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

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}
