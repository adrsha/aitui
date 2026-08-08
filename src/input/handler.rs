use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::app::action::{Action, Dir};
use crate::app::overlay::Overlay;
use crate::app::state::App;
use crate::input::vim::VimMode;

pub fn handle_event(app: &App, event: Event) -> Vec<Action> {
    match event {
        // With keyboard enhancement on, terminals also report key releases —
        // act on presses (and auto-repeats) only, so keys don't fire twice.
        Event::Key(k) if k.kind != KeyEventKind::Release => {
            handle_key(app, normalize_shifted_key(k))
        }
        Event::Mouse(m) => handle_mouse(app, m),
        Event::Resize(_, _) => vec![Action::Resize],
        // A bracketed paste arrives as one blob — smart-paste decides file vs chip.
        Event::Paste(s) => vec![Action::PasteText(s)],
        Event::FocusGained => vec![Action::FocusGained],
        Event::FocusLost => vec![Action::FocusLost],
        _ => vec![],
    }
}

fn normalize_shifted_key(mut key: KeyEvent) -> KeyEvent {
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        if let KeyCode::Char(c) = key.code {
            key.code = KeyCode::Char(shifted_char(c));
        }
    }
    key
}

fn shifted_char(c: char) -> char {
    match c {
        'a'..='z' => c.to_ascii_uppercase(),
        '`' => '~',
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        _ => c,
    }
}

fn handle_key(app: &App, key: KeyEvent) -> Vec<Action> {
    let km = &app.keymap;

    // ── Global shortcuts (fire in any mode, configurable) ───────────────
    if km.quit.matches(&key) {
        return if app.sessions.active().is_streaming() {
            vec![Action::CancelStream]
        } else {
            vec![Action::Quit]
        };
    }
    if km.redraw.matches(&key) {
        return vec![Action::Resize];
    }
    if km.next_session.matches(&key)
        && !(app.vim == VimMode::Insert
            && key.code == KeyCode::Tab
            && !key.modifiers.contains(KeyModifiers::SHIFT))
    {
        return vec![Action::NextSession];
    }
    if km.prev_session.matches(&key) {
        return vec![Action::PrevSession];
    }
    if km.prev_subtask.matches(&key) {
        return vec![Action::PrevSubtask];
    }
    if km.next_subtask.matches(&key) {
        return vec![Action::NextSubtask];
    }
    if km.session_picker.matches(&key) {
        return vec![Action::OpenSessionPicker];
    }
    if km.fork_session.matches(&key) {
        return vec![Action::ForkSession];
    }
    if km.open_editor.matches(&key) {
        return vec![Action::OpenEditor];
    }
    if km.open_file.matches(&key) {
        return vec![Action::OpenEditPicker];
    }
    if km.open_shell.matches(&key) {
        // While the browser is open, this key closes it too (both keys toggle).
        return if app.overlay.is_browser() {
            vec![Action::BrowserClose]
        } else {
            vec![Action::OpenShell]
        };
    }
    if km.next_model.matches(&key) {
        return vec![Action::NextModel];
    }
    if km.prev_model.matches(&key) {
        return vec![Action::PrevModel];
    }
    if km.file_picker.matches(&key) {
        return vec![Action::OpenFilePicker];
    }
    if km.model_picker.matches(&key) {
        return vec![Action::OpenModelPicker];
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        if let KeyCode::Char(c @ '1'..='3') = key.code {
            let index = c.to_digit(10).unwrap_or(1) as usize - 1;
            if app
                .sessions
                .active()
                .response_suggestions
                .get(index)
                .is_some()
            {
                return vec![Action::AcceptResponseSuggestion(index)];
            }
        }
    }

    // ── Overlays take priority over the rest ────────────────────────────
    match &app.overlay {
        Overlay::Picker(_) => return handle_picker(app, &key),
        Overlay::Browser(_) => return handle_browser(&key),
        Overlay::Palette(_) => return handle_palette(&key),
        Overlay::Settings(_) => return handle_settings(&key),
        Overlay::Permission(_) => return handle_permission(app, &key),
        Overlay::Decision(_) => return handle_decision(app, &key),
        Overlay::Plan(_) => return handle_plan(&key),
        Overlay::ToolRequest(_) => return handle_tool_request(&key),
        Overlay::ApiSetup(_) => return handle_api_setup(&key),
        Overlay::SubtaskDetail { .. } => {
            return match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                    vec![Action::DismissNotice]
                }
                KeyCode::Left | KeyCode::Char('h') => vec![Action::PrevSubtask],
                KeyCode::Right | KeyCode::Char('l') => vec![Action::NextSubtask],
                KeyCode::Up | KeyCode::Char('k') | KeyCode::PageUp if !ctrl_pressed(&key) => {
                    vec![Action::SubtaskDetailUp]
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::PageDown if !ctrl_pressed(&key) => {
                    vec![Action::SubtaskDetailDown]
                }
                KeyCode::Char('k') if ctrl_pressed(&key) => vec![Action::SubtaskDetailUp],
                KeyCode::Char('j') if ctrl_pressed(&key) => vec![Action::SubtaskDetailDown],
                _ => Vec::new(),
            }
        }
        Overlay::CommandLine(cl) => return handle_command_line(cl, &key),
        // A notice is a plain "OK" dialog: any key dismisses it.
        Overlay::Notice { .. } => return vec![Action::DismissNotice],
        Overlay::None => {}
    }

    // Esc inside an entered child agent walks back up the tree (to its parent,
    // and eventually the root chat).
    if app.view_node.is_some() && key.code == KeyCode::Esc {
        return vec![Action::NavigateBack];
    }

    // ── Tab / Shift-Tab ────────────────────────────────────────────────
    // No overlay is open here. In insert mode, plain Tab belongs to the
    // composer: it accepts the visible ghost suggestion or inserts a tab.
    // Shift-Tab and non-insert Tab keep session cycling available.
    if !(app.mention.active && !app.mention.matches.is_empty()) {
        if key.code == KeyCode::BackTab
            || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT))
        {
            return vec![Action::PrevSession];
        }
        if key.code == KeyCode::Tab {
            if app.vim == VimMode::Insert {
                if app.input.text().is_empty()
                    && !app.sessions.active().response_suggestions.is_empty()
                {
                    return vec![Action::AcceptResponseSuggestion(0)];
                }
                return vec![Action::InsertChar('\t')];
            }
            return vec![Action::NextSession];
        }
    }

    // ── Transcript scrolling (works in any input mode) ──────────────────
    if km.scroll_up.matches(&key) {
        return vec![Action::ChatPageUp];
    }
    if km.scroll_down.matches(&key) {
        return vec![Action::ChatPageDown];
    }
    if km.scroll_bottom.matches(&key) {
        return vec![Action::ChatBottom];
    }
    if km.scroll_half_down.matches(&key) {
        return vec![Action::ChatHalfDown];
    }
    if km.scroll_half_up.matches(&key) {
        return vec![Action::ChatHalfUp];
    }
    if km.scroll_top.matches(&key) {
        return vec![Action::ChatTop];
    }
    if km.toggle_output.matches(&key) {
        return vec![Action::ToggleOutput];
    }

    // ── Help overlay steals keys while open ──────────────────────────
    if app.show_help {
        return match key.code {
            KeyCode::Esc => vec![Action::HelpBack],
            KeyCode::Char('q') if !ctrl_pressed(&key) => vec![Action::HelpBack],
            KeyCode::Char('j') | KeyCode::Down if !ctrl_pressed(&key) => vec![Action::HelpDown],
            KeyCode::Char('k') | KeyCode::Up if !ctrl_pressed(&key) => vec![Action::HelpUp],
            KeyCode::PageDown => vec![Action::HelpPageDown],
            KeyCode::PageUp => vec![Action::HelpPageUp],
            KeyCode::Enter => vec![Action::HelpSelect],
            _ if km.help.matches(&key) => vec![Action::ToggleHelp],
            _ => vec![],
        };
    }

    // ── Vim modes for the input box ─────────────────────────────────────
    match app.vim {
        VimMode::Insert => handle_insert(app, &key),
        VimMode::Normal => handle_normal(app, &key),
        VimMode::Visual => handle_visual(&key),
        VimMode::Operator(op) => handle_operator(&key, op),
    }
}

fn ctrl_pressed(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
}

fn ctrl_char(key: &KeyEvent, ch: char) -> bool {
    ctrl_pressed(key) && key.code == KeyCode::Char(ch)
}

// ── Overlay handlers ──────────────────────────────────────────────────────────

fn handle_picker(app: &App, key: &KeyEvent) -> Vec<Action> {
    if ctrl_char(key, 'j') {
        return vec![Action::PickerDown];
    }
    if ctrl_char(key, 'k') {
        return vec![Action::PickerUp];
    }
    if let Overlay::Picker(p) = &app.overlay {
        if p.kind == crate::app::overlay::PickerKind::Access {
            match key.code {
                KeyCode::Char('x') | KeyCode::Char('d') if !ctrl_pressed(key) => {
                    return p
                        .selected_index()
                        .and_then(|index| index.checked_sub(1))
                        .map(Action::RemoveAccessEntry)
                        .into_iter()
                        .collect();
                }
                KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right if !ctrl_pressed(key) => {
                    return match p.selected_index() {
                        Some(0)
                            if app.config.api.access_review_mode
                                != crate::config::AccessReviewMode::Off =>
                        {
                            vec![Action::DisableAccessReview]
                        }
                        Some(0) | None => Vec::new(),
                        Some(index) => vec![Action::EditAccessEntry(index - 1)],
                    };
                }
                KeyCode::Char('j') if !ctrl_pressed(key) => return vec![Action::PickerDown],
                KeyCode::Char('k') if !ctrl_pressed(key) => return vec![Action::PickerUp],
                _ => {}
            }
        }

        if p.kind == crate::app::overlay::PickerKind::Session {
            match key.code {
                KeyCode::Char('a') | KeyCode::Char('n') if !ctrl_pressed(key) => {
                    return vec![Action::PickerCancel, Action::NewSession]
                }
                KeyCode::Char('d') if !ctrl_pressed(key) => {
                    return p
                        .selected_index()
                        .and_then(|i| i.checked_sub(1))
                        .map(Action::DeleteSessionAt)
                        .into_iter()
                        .collect();
                }
                // Rename still uses the editable command palette/line: type the new
                // name after the inserted command and press Enter.
                KeyCode::Char('r') if !ctrl_pressed(key) && p.selected_index().unwrap_or(0) > 0 => {
                    return vec![
                        Action::PickerCancel,
                        Action::RunCommand("rename ".to_string()),
                    ];
                }
                KeyCode::Char('j') if !ctrl_pressed(key) => return vec![Action::PickerDown],
                KeyCode::Char('k') if !ctrl_pressed(key) => return vec![Action::PickerUp],
                KeyCode::Char('l') | KeyCode::Right if !ctrl_pressed(key) => {
                    return vec![Action::PickerConfirm]
                }
                _ => {}
            }
        }
    }
    match key.code {
        KeyCode::Esc => vec![Action::PickerCancel],
        KeyCode::Enter => vec![Action::PickerConfirm],
        KeyCode::Up => vec![Action::PickerUp],
        KeyCode::Down => vec![Action::PickerDown],
        KeyCode::Backspace => vec![Action::PickerBackspace],
        KeyCode::Char(c) => vec![Action::PickerChar(c)],
        _ => vec![],
    }
}

fn handle_browser(key: &KeyEvent) -> Vec<Action> {
    match key.code {
        KeyCode::Esc => vec![Action::BrowserClose],
        KeyCode::Char('j') | KeyCode::Down => vec![Action::BrowserDown],
        KeyCode::Char('k') | KeyCode::Up => vec![Action::BrowserUp],
        KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => vec![Action::BrowserParent],
        KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => vec![Action::BrowserOpen],
        KeyCode::Char(' ') => vec![Action::BrowserSelect],
        _ => vec![],
    }
}

fn handle_palette(key: &KeyEvent) -> Vec<Action> {
    if ctrl_char(key, 'j') {
        return vec![Action::PickerDown];
    }
    if ctrl_char(key, 'k') {
        return vec![Action::PickerUp];
    }
    match key.code {
        KeyCode::Esc => vec![Action::PickerCancel],
        KeyCode::Enter => vec![Action::PickerConfirm],
        KeyCode::Up => vec![Action::PickerUp],
        KeyCode::Down => vec![Action::PickerDown],
        KeyCode::PageUp => (0..8).map(|_| Action::PickerUp).collect(),
        KeyCode::PageDown => (0..8).map(|_| Action::PickerDown).collect(),
        KeyCode::Backspace => vec![Action::PickerBackspace],
        KeyCode::Char(c) => vec![Action::PickerChar(c)],
        _ => vec![],
    }
}

fn handle_settings(key: &KeyEvent) -> Vec<Action> {
    if ctrl_char(key, 'j') {
        return vec![Action::PickerDown];
    }
    if ctrl_char(key, 'k') {
        return vec![Action::PickerUp];
    }
    match key.code {
        KeyCode::Esc => vec![Action::PickerCancel],
        KeyCode::Enter => vec![Action::PickerConfirm],
        KeyCode::Up => vec![Action::PickerUp],
        KeyCode::Down => vec![Action::PickerDown],
        KeyCode::Left => vec![Action::SettingsLeft],
        KeyCode::Right => vec![Action::SettingsRight],
        KeyCode::Char(c) => vec![Action::PickerChar(c)],
        KeyCode::Backspace => vec![Action::PickerBackspace],
        _ => vec![],
    }
}

fn handle_api_setup(key: &KeyEvent) -> Vec<Action> {
    if ctrl_char(key, 'j') {
        return vec![Action::PickerDown];
    }
    if ctrl_char(key, 'k') {
        return vec![Action::PickerUp];
    }
    match key.code {
        KeyCode::Esc => vec![Action::PickerCancel],
        KeyCode::Enter => vec![Action::PickerConfirm],
        // Tab / arrows switch between the URL and key fields.
        KeyCode::Tab | KeyCode::Up | KeyCode::Down => vec![Action::PickerDown],
        KeyCode::Char(c) => vec![Action::PickerChar(c)],
        KeyCode::Backspace => vec![Action::PickerBackspace],
        _ => vec![],
    }
}

fn handle_permission(app: &App, key: &KeyEvent) -> Vec<Action> {
    let selecting =
        matches!(&app.overlay, Overlay::Permission(r) if r.selecting || r.selecting_folder());
    if selecting {
        let folder = matches!(&app.overlay, Overlay::Permission(r) if r.selecting_folder());
        return match key.code {
            KeyCode::Esc => vec![Action::AgentPermissionSelectorCancel],
            KeyCode::Enter | KeyCode::Tab | KeyCode::Char(' ') => {
                vec![Action::AgentPermissionSelector]
            }
            KeyCode::Up | KeyCode::Char('k') if !ctrl_pressed(key) => {
                vec![Action::PickerUp]
            }
            KeyCode::Down | KeyCode::Char('j') if !ctrl_pressed(key) => {
                vec![Action::PickerDown]
            }
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h')
                if folder && !ctrl_pressed(key) =>
            {
                vec![Action::AgentPermissionFolderParent]
            }
            KeyCode::Right | KeyCode::Char('l') if folder && !ctrl_pressed(key) => {
                vec![Action::AgentPermissionSelector]
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l')
                if !ctrl_pressed(key) =>
            {
                if matches!(key.code, KeyCode::Left | KeyCode::Char('h')) {
                    vec![Action::PickerUp]
                } else {
                    vec![Action::PickerDown]
                }
            }
            _ => vec![],
        };
    }
    let custom_editing = matches!(&app.overlay, Overlay::Permission(r) if r.editing_custom);
    if custom_editing {
        return match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Tab => vec![Action::AgentPermissionCustom],
            KeyCode::Backspace => vec![Action::PickerBackspace],
            KeyCode::Char(c) if !ctrl_pressed(key) => vec![Action::PickerChar(c)],
            _ => vec![],
        };
    }
    // While the deny reason box is open every printable key is text, so the menu
    // shortcuts below (a/d/e/p, j/k) must not steal them.
    if matches!(&app.overlay, Overlay::Permission(r) if r.writing_reason()) {
        return match key.code {
            KeyCode::Esc => vec![Action::AgentDenyCancel],
            KeyCode::Enter => vec![Action::AgentResolvePermission],
            KeyCode::Backspace => vec![Action::PickerBackspace],
            KeyCode::Char(c) if !ctrl_pressed(key) => vec![Action::PickerChar(c)],
            _ => vec![],
        };
    }
    if ctrl_char(key, 'j') {
        return vec![Action::PickerDown];
    }
    if ctrl_char(key, 'k') {
        return vec![Action::PickerUp];
    }
    match key.code {
        KeyCode::Esc => vec![Action::AgentCancel],
        // PageUp/PageDown scroll the (possibly long) command list.
        KeyCode::PageUp => vec![Action::AgentPermScrollUp],
        KeyCode::PageDown => vec![Action::AgentPermScrollDown],
        KeyCode::Left if key.modifiers.contains(KeyModifiers::SHIFT) => {
            vec![Action::AgentPermScrollLeft]
        }
        KeyCode::Right if key.modifiers.contains(KeyModifiers::SHIFT) => {
            vec![Action::AgentPermScrollRight]
        }
        KeyCode::Up | KeyCode::Left | KeyCode::Char('k') | KeyCode::Char('h')
            if !ctrl_pressed(key) =>
        {
            vec![Action::PickerUp]
        }
        KeyCode::Down | KeyCode::Right | KeyCode::Char('j') | KeyCode::Char('l')
            if !ctrl_pressed(key) =>
        {
            vec![Action::PickerDown]
        }
        KeyCode::Char(' ') if !ctrl_pressed(key) => vec![Action::AgentPermissionSelector],
        KeyCode::Tab => vec![Action::AgentPermissionCustom],
        KeyCode::Enter if matches!(&app.overlay, Overlay::Permission(r) if r.editing_access.is_some()) =>
        {
            vec![Action::AgentResolvePermission]
        }
        KeyCode::Enter => vec![Action::AgentReviewPermission],
        // Quick shortcuts for the common once-off cases, so you don't have to
        // arrow to them: 'a' allow this call, 'd' deny this call, 'e' edit in $EDITOR.
        KeyCode::Char('a') if !ctrl_pressed(key) => vec![Action::AgentQuickAllow],
        KeyCode::Char('d') if !ctrl_pressed(key) => vec![Action::AgentQuickDeny],
        KeyCode::Char('e') if !ctrl_pressed(key) => vec![Action::AgentPermissionEdit],
        // 'p' opens $EDITOR to set the session access policy; on save this batch is
        // re-judged against it (so you can teach it once and stop being asked).
        KeyCode::Char('p') if !ctrl_pressed(key) => vec![Action::AgentEditPolicy],
        _ => vec![],
    }
}

fn handle_decision(app: &App, key: &KeyEvent) -> Vec<Action> {
    let editing = matches!(&app.overlay, Overlay::Decision(r) if r.editing_answer());
    if !editing && ctrl_char(key, 'j') {
        return vec![Action::PickerDown];
    }
    if !editing && ctrl_char(key, 'k') {
        return vec![Action::PickerUp];
    }
    match key.code {
        KeyCode::Esc => vec![Action::AgentCancel],
        KeyCode::Tab => vec![Action::AgentDecisionCustom],
        KeyCode::Char('e') if !editing && !ctrl_pressed(key) => vec![Action::AgentDecisionEdit],
        KeyCode::Enter => vec![Action::AgentResolveDecision],
        KeyCode::Backspace if editing => vec![Action::PickerBackspace],
        KeyCode::Char(c) if editing && !ctrl_pressed(key) => vec![Action::PickerChar(c)],
        KeyCode::Up | KeyCode::Char('k') if !editing && !ctrl_pressed(key) => {
            vec![Action::PickerUp]
        }
        KeyCode::Down | KeyCode::Char('j') if !editing && !ctrl_pressed(key) => {
            vec![Action::PickerDown]
        }
        KeyCode::Char(' ') if !editing && !ctrl_pressed(key) => {
            vec![Action::AgentDecisionToggle]
        }
        _ => vec![],
    }
}

fn handle_plan(key: &KeyEvent) -> Vec<Action> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('d') if !ctrl_pressed(key) => vec![Action::AgentPlanDeny],
        KeyCode::Char('e') if !ctrl_pressed(key) => vec![Action::AgentPlanEdit],
        KeyCode::Char('a') | KeyCode::Enter if !ctrl_pressed(key) => vec![Action::AgentPlanAccept],
        _ => vec![],
    }
}

// ── Command line (vim `:`) ────────────────────────────────────────────────────

fn handle_command_line(cl: &crate::app::overlay::CommandLine, key: &KeyEvent) -> Vec<Action> {
    match key.code {
        KeyCode::Esc => vec![Action::PickerCancel],
        KeyCode::Enter => {
            let cmd = cl.input.trim();
            if cmd.is_empty() {
                return vec![Action::PickerCancel];
            }
            // Exact commands should run immediately. In particular, typing `:q`
            // or `:w` must not first expand the visible completion to `quit ` or
            // `send ` and require a second Enter press.
            if crate::app::commands::exact_command_action(cmd).is_some() {
                return vec![Action::PickerCancel, Action::RunCommand(cmd.to_string())];
            }
            if cl.has_completions() {
                return vec![Action::CommandLineAccept];
            }
            vec![Action::PickerCancel, Action::RunCommand(cmd.to_string())]
        }
        KeyCode::Backspace => vec![Action::PickerBackspace],
        KeyCode::Tab => {
            if cl.has_completions() && cl.filtered.len() > 1 {
                vec![Action::CommandLineNext]
            } else if cl.filtered.len() == 1 {
                vec![Action::CommandLineAccept]
            } else {
                vec![Action::PickerChar('\t')]
            }
        }
        KeyCode::BackTab => {
            if cl.has_completions() && cl.filtered.len() > 1 {
                vec![Action::CommandLinePrev]
            } else {
                vec![]
            }
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            vec![Action::PickerChar(c)]
        }
        _ => vec![],
    }
}

// ── Vim mode handlers (input box only) ─────────────────────────────────────────

fn handle_normal(app: &App, key: &KeyEvent) -> Vec<Action> {
    // Configurable mode-switch / action keys first.
    let km = &app.keymap;
    if km.insert.matches(key) {
        return vec![Action::EnterInsert];
    }
    // `:` and `/` are normal-mode commands only.
    if km.command.matches(key) {
        return vec![Action::OpenCommandLine];
    }
    if km.palette.matches(key) {
        return vec![Action::OpenCommandPalette];
    }
    if km.help.matches(key) {
        return vec![Action::ToggleHelp];
    }
    if km.submit.matches(key) {
        return vec![Action::Submit];
    }
    if km.visual.matches(key) {
        return vec![Action::EnterVisual];
    }

    // Fixed vim motions / edits (standard vim, not remapped).
    match key.code {
        KeyCode::Esc => vec![],
        KeyCode::Char('V') => vec![Action::EnterVisualLine],
        KeyCode::Char('I') => vec![Action::LineStart, Action::EnterInsert],
        // EnterInsert must precede Move(Right): in normal mode Move clamps the
        // cursor to a character, which would block appending at end of line.
        KeyCode::Char('a') => vec![Action::EnterInsert, Action::Move(Dir::Right)],
        KeyCode::Char('A') => vec![
            Action::LineEnd,
            Action::EnterInsert,
            Action::Move(Dir::Right),
        ],
        KeyCode::Char('o') => vec![Action::OpenLineBelow],
        KeyCode::Char('O') => vec![Action::OpenLineAbove],
        KeyCode::Char('h') | KeyCode::Left => vec![Action::Move(Dir::Left)],
        KeyCode::Char('j') | KeyCode::Down => vec![Action::Move(Dir::Down)],
        KeyCode::Char('k') | KeyCode::Up => vec![Action::Move(Dir::Up)],
        KeyCode::Char('l') | KeyCode::Right => vec![Action::Move(Dir::Right)],
        KeyCode::Char('w') => vec![Action::Move(Dir::WordForward)],
        KeyCode::Char('b') => vec![Action::Move(Dir::WordBackward)],
        KeyCode::Char('e') => vec![Action::Move(Dir::WordEnd)],
        KeyCode::Char('0') => vec![Action::LineStart],
        KeyCode::Char('^') => vec![Action::FirstNonBlank],
        KeyCode::Char('$') => vec![Action::LineEnd],
        KeyCode::Char('x') => vec![Action::DeleteAt],
        KeyCode::Char('s') => vec![Action::DeleteAt, Action::EnterInsert],
        KeyCode::Char('d') => vec![Action::EnterOperator('d')],
        KeyCode::Char('c') => vec![Action::EnterOperator('c')],
        KeyCode::Char('y') => vec![Action::EnterOperator('y')],
        KeyCode::Char('Y') => vec![Action::YankLine],
        KeyCode::Char('p') => vec![Action::Paste],
        KeyCode::Char('D') => vec![Action::DeleteToLineEnd],
        KeyCode::Char('C') => vec![Action::ChangeToLineEnd],
        KeyCode::Char('u') => vec![Action::UndoInput],
        KeyCode::Char('r') if ctrl_pressed(key) => vec![Action::RedoInput],
        KeyCode::Backspace => vec![Action::Backspace],
        _ => vec![],
    }
}

fn single_visual_input_row(app: &App) -> bool {
    crate::app::input_layout::layout(&app.input.lines, app.input.width).len() <= 1
}

fn handle_insert(app: &App, key: &KeyEvent) -> Vec<Action> {
    // Newline chords must win even when popups/keymaps also handle Enter.
    // Some terminals report Shift+Enter as Enter+SHIFT; others send a literal
    // \n or \r (depending on their terminal/kitty-protocol configuration).
    if key.code == KeyCode::Enter
        && key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL)
    {
        return vec![Action::Newline];
    }
    if matches!(key.code, KeyCode::Char('\n')) {
        return vec![Action::Newline];
    }

    // While the @mention popup is open, the arrow keys / Enter drive it.
    if app.mention.active && !app.mention.matches.is_empty() {
        match key.code {
            KeyCode::Up => return vec![Action::MentionUp],
            KeyCode::Down => return vec![Action::MentionDown],
            KeyCode::Char('k') if ctrl_char(key, 'k') => return vec![Action::MentionUp],
            KeyCode::Char('j') if ctrl_char(key, 'j') => return vec![Action::MentionDown],
            KeyCode::Tab | KeyCode::Enter => return vec![Action::MentionAccept],
            KeyCode::Esc => return vec![Action::MentionCancel],
            _ => {}
        }
    }
    // `jk`-style chord: if the previous inserted char was the chord's first key
    // and this is its second, delete that char and leave insert mode.
    if chord_escapes(app.keymap.normal_chord, app.last_insert, key.code) {
        return vec![Action::Backspace, Action::EnterNormal];
    }

    if app.keymap.normal.matches(key) {
        return vec![Action::EnterNormal];
    }

    // Word delete: Ctrl-W / Ctrl-Backspace (back), Ctrl-Delete (forward).
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // Ctrl-K/Ctrl-J mirror Up/Down wherever the composer supports vertical
    // movement: history in a single-line draft, cursor movement in multiline text.
    if ctrl && key.code == KeyCode::Char('k') {
        return if single_visual_input_row(app) {
            vec![Action::InputHistoryPrev]
        } else {
            vec![Action::Move(Dir::Up)]
        };
    }
    if ctrl && key.code == KeyCode::Char('j') {
        return if single_visual_input_row(app) {
            vec![Action::InputHistoryNext]
        } else {
            vec![Action::Move(Dir::Down)]
        };
    }
    if ctrl && key.code == KeyCode::Char('r') {
        return vec![Action::RedoInput];
    }
    if ctrl && matches!(key.code, KeyCode::Backspace) {
        return vec![Action::DeleteWordBack];
    }
    if ctrl && key.code == KeyCode::Char('w') {
        return vec![Action::DeleteWordBack];
    }
    if ctrl && matches!(key.code, KeyCode::Delete) {
        return vec![Action::DeleteWordForward];
    }

    // Enter sends the message (same as :w); Shift/Alt/Ctrl+Enter inserts a newline
    // on terminals that report modified Enter distinctly.
    if key.code == KeyCode::Enter {
        let newline = key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL);
        return if newline {
            vec![Action::Newline]
        } else {
            vec![Action::Submit]
        };
    }
    // Honour the `submit` binding if it's mapped to a non-Enter key too.
    if app.keymap.submit.matches(key) {
        return vec![Action::Submit];
    }

    match key.code {
        KeyCode::Backspace => vec![Action::Backspace],
        KeyCode::Delete => vec![Action::DeleteAt],
        // Single-line composer: Up/Down recall sent-message history (shell style).
        // Multi-line: they move the cursor between lines.
        KeyCode::Up if single_visual_input_row(app) => vec![Action::InputHistoryPrev],
        KeyCode::Down if single_visual_input_row(app) => vec![Action::InputHistoryNext],
        KeyCode::Up => vec![Action::Move(Dir::Up)],
        KeyCode::Down => vec![Action::Move(Dir::Down)],
        KeyCode::Left => vec![Action::Move(Dir::Left)],
        KeyCode::Right => vec![Action::Move(Dir::Right)],
        // Ignore control-modified chars so a stray Ctrl-key doesn't type a letter
        // (e.g. Ctrl-Enter reported oddly by some terminals).
        KeyCode::Char(c) if !ctrl => vec![Action::InsertChar(c)],
        _ => vec![],
    }
}

fn handle_tool_request(key: &KeyEvent) -> Vec<Action> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => vec![Action::AgentEnableTools],
        KeyCode::Char('n') | KeyCode::Esc => vec![Action::AgentDeclineTools],
        _ => vec![],
    }
}

fn handle_visual(key: &KeyEvent) -> Vec<Action> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('v') => vec![Action::EnterNormal],
        KeyCode::Char('h') | KeyCode::Left => vec![Action::Move(Dir::Left)],
        KeyCode::Char('j') | KeyCode::Down => vec![Action::Move(Dir::Down)],
        KeyCode::Char('k') | KeyCode::Up => vec![Action::Move(Dir::Up)],
        KeyCode::Char('l') | KeyCode::Right => vec![Action::Move(Dir::Right)],
        KeyCode::Char('w') => vec![Action::Move(Dir::WordForward)],
        KeyCode::Char('b') => vec![Action::Move(Dir::WordBackward)],
        KeyCode::Char('e') => vec![Action::Move(Dir::WordEnd)],
        KeyCode::Char('0') => vec![Action::LineStart],
        KeyCode::Char('^') => vec![Action::FirstNonBlank],
        KeyCode::Char('$') => vec![Action::LineEnd],
        KeyCode::Char('y') => vec![Action::VisualYank],
        KeyCode::Char('d') | KeyCode::Char('x') => vec![Action::VisualDelete],
        KeyCode::Char('c') | KeyCode::Char('s') => vec![Action::VisualChange],
        _ => vec![],
    }
}

fn handle_operator(key: &KeyEvent, op: char) -> Vec<Action> {
    let motion = match key.code {
        KeyCode::Char('h') | KeyCode::Left => Some(Dir::Left),
        KeyCode::Char('l') | KeyCode::Right => Some(Dir::Right),
        KeyCode::Char('j') | KeyCode::Down => Some(Dir::Down),
        KeyCode::Char('k') | KeyCode::Up => Some(Dir::Up),
        KeyCode::Char('w') if op == 'c' => Some(Dir::WordEnd),
        KeyCode::Char('w') => Some(Dir::WordForward),
        KeyCode::Char('b') => Some(Dir::WordBackward),
        KeyCode::Char('e') => Some(Dir::WordEnd),
        _ => None,
    };
    if let Some(dir) = motion {
        return match op {
            'd' => vec![Action::DeleteTo(dir)],
            'c' => vec![Action::ChangeTo(dir)],
            'y' => vec![Action::YankTo(dir)],
            _ => vec![Action::EnterNormal],
        };
    }

    match (op, key.code) {
        ('d', KeyCode::Char('d')) => vec![Action::DeleteLine, Action::EnterNormal],
        ('c', KeyCode::Char('c')) => vec![Action::ChangeLine],
        ('y', KeyCode::Char('y')) => vec![Action::YankLine, Action::EnterNormal],
        ('d', KeyCode::Char('$')) => vec![Action::DeleteToLineEnd, Action::EnterNormal],
        ('c', KeyCode::Char('$')) => vec![Action::ChangeToLineEnd],
        ('y', KeyCode::Char('$')) => vec![Action::YankToLineEnd, Action::EnterNormal],
        (_, KeyCode::Esc) => vec![Action::EnterNormal],
        _ => vec![Action::EnterNormal],
    }
}

/// Whether `key` completes the insert-escape chord: it is the chord's second
/// char and the immediately preceding inserted char was the first.
fn chord_escapes(chord: Option<(char, char)>, last_insert: Option<char>, key: KeyCode) -> bool {
    match (chord, key) {
        (Some((c1, c2)), KeyCode::Char(c)) => {
            c.eq_ignore_ascii_case(&c2) && last_insert.is_some_and(|p| p.eq_ignore_ascii_case(&c1))
        }
        _ => false,
    }
}

// ── Mouse handler ─────────────────────────────────────────────────────────────

fn handle_mouse(app: &App, mouse: MouseEvent) -> Vec<Action> {
    // While a scrollable overlay is open the wheel belongs to that overlay.
    let perm_open = matches!(app.overlay, Overlay::Permission(_));
    let subtask_open = matches!(app.overlay, Overlay::SubtaskDetail { .. });
    let over_sidebar_tasks = app.layout.sidebar_tasks.is_some_and(|area| {
        mouse.column >= area.x
            && mouse.column < area.x + area.width
            && mouse.row >= area.y
            && mouse.row < area.y + area.height
    });
    match mouse.kind {
        MouseEventKind::ScrollUp if perm_open => vec![Action::AgentPermScrollUp],
        MouseEventKind::ScrollDown if perm_open => vec![Action::AgentPermScrollDown],
        MouseEventKind::ScrollUp if subtask_open => vec![Action::SubtaskDetailUp],
        MouseEventKind::ScrollDown if subtask_open => vec![Action::SubtaskDetailDown],
        MouseEventKind::ScrollUp if over_sidebar_tasks => vec![Action::SidebarTaskScroll(-3)],
        MouseEventKind::ScrollDown if over_sidebar_tasks => vec![Action::SidebarTaskScroll(3)],
        MouseEventKind::ScrollUp => vec![Action::ChatScroll(3)],
        MouseEventKind::ScrollDown => vec![Action::ChatScroll(-3)],
        MouseEventKind::Down(MouseButton::Left) => {
            vec![Action::ChatClick(mouse.column, mouse.row)]
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            vec![Action::ChatDrag(mouse.column, mouse.row)]
        }
        MouseEventKind::Up(MouseButton::Left) => {
            vec![Action::ChatRelease]
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crossterm::event::KeyEventKind;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    fn modified_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            modifiers,
            ..key(code)
        }
    }

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        modified_key(code, KeyModifiers::CONTROL)
    }

    fn test_app() -> App {
        let mut app = App::new(Config::default()).unwrap();
        app.overlay = Overlay::None;
        app.vim = VimMode::Insert;
        app
    }

    #[test]
    fn shifted_printable_keys_are_normalized_before_insert() {
        let app = test_app();

        let capital = handle_event(
            &app,
            Event::Key(modified_key(KeyCode::Char('a'), KeyModifiers::SHIFT)),
        );
        let colon = handle_event(
            &app,
            Event::Key(modified_key(KeyCode::Char(';'), KeyModifiers::SHIFT)),
        );
        let symbol = handle_event(
            &app,
            Event::Key(modified_key(KeyCode::Char('1'), KeyModifiers::SHIFT)),
        );

        assert!(matches!(capital.as_slice(), [Action::InsertChar('A')]));
        assert!(matches!(colon.as_slice(), [Action::InsertChar(':')]));
        assert!(matches!(symbol.as_slice(), [Action::InsertChar('!')]));
    }

    #[test]
    fn already_shifted_characters_are_left_unchanged() {
        let app = test_app();

        let actions = handle_event(
            &app,
            Event::Key(modified_key(KeyCode::Char('A'), KeyModifiers::SHIFT)),
        );

        assert!(matches!(actions.as_slice(), [Action::InsertChar('A')]));
    }

    #[test]
    fn normal_mode_a_and_capital_a_enter_insert_then_move() {
        let mut app = test_app();
        app.vim = VimMode::Normal;

        let small = handle_event(&app, Event::Key(key(KeyCode::Char('a'))));
        assert!(matches!(
            small.as_slice(),
            [Action::EnterInsert, Action::Move(Dir::Right)]
        ));

        let capital = handle_event(&app, Event::Key(key(KeyCode::Char('A'))));
        assert!(matches!(
            capital.as_slice(),
            [
                Action::LineEnd,
                Action::EnterInsert,
                Action::Move(Dir::Right)
            ]
        ));
    }

    #[test]
    fn ctrl_shift_d_jumps_to_bottom_while_ctrl_d_keeps_half_scroll() {
        let app = test_app();
        assert!(matches!(
            handle_key(
                &app,
                modified_key(
                    KeyCode::Char('d'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT
                ),
            )
            .as_slice(),
            [Action::ChatBottom]
        ));
        assert!(matches!(
            handle_key(&app, ctrl_key(KeyCode::Char('d'))).as_slice(),
            [Action::ChatHalfDown]
        ));
        assert!(!matches!(
            handle_key(&app, ctrl_key(KeyCode::End)).as_slice(),
            [Action::ChatBottom]
        ));
    }

    #[test]
    fn slash_and_colon_commands_only_open_in_normal_mode() {
        let mut app = test_app();

        assert!(matches!(
            handle_key(&app, key(KeyCode::Char('/'))).as_slice(),
            [Action::InsertChar('/')]
        ));
        assert!(matches!(
            handle_key(&app, key(KeyCode::Char(':'))).as_slice(),
            [Action::InsertChar(':')]
        ));

        app.vim = VimMode::Normal;
        assert!(matches!(
            handle_key(&app, key(KeyCode::Char('/'))).as_slice(),
            [Action::OpenCommandPalette]
        ));
        assert!(matches!(
            handle_key(&app, key(KeyCode::Char(':'))).as_slice(),
            [Action::OpenCommandLine]
        ));
    }

    #[test]
    fn exact_vim_commands_execute_on_first_enter() {
        for command in ["q", "w"] {
            let mut line = crate::app::overlay::CommandLine::new();
            for c in command.chars() {
                line.push(c);
            }
            assert!(line.has_completions());
            assert!(matches!(
                handle_command_line(&line, &key(KeyCode::Enter)).as_slice(),
                [Action::PickerCancel, Action::RunCommand(cmd)] if cmd == command
            ));
        }
    }

    #[test]
    fn partial_command_still_accepts_completion() {
        let mut line = crate::app::overlay::CommandLine::new();
        line.push('q');
        line.push('u');
        assert!(line.has_completions());
        assert!(matches!(
            handle_command_line(&line, &key(KeyCode::Enter)).as_slice(),
            [Action::CommandLineAccept]
        ));
    }

    #[test]
    fn shift_enter_event_inserts_newline_in_insert_mode() {
        let app = test_app();

        let actions = handle_event(
            &app,
            Event::Key(modified_key(KeyCode::Enter, KeyModifiers::SHIFT)),
        );

        assert!(matches!(actions.as_slice(), [Action::Newline]));
    }

    #[test]
    fn shift_enter_inserts_newline_in_insert_mode() {
        let app = test_app();

        let actions = handle_key(&app, modified_key(KeyCode::Enter, KeyModifiers::SHIFT));

        assert!(matches!(actions.as_slice(), [Action::Newline]));
    }

    #[test]
    fn arrows_move_within_a_wrapped_single_line_instead_of_recalling_history() {
        let mut app = test_app();
        app.input.set_width(4);
        app.input.set_text("abcdefgh");
        app.input.col = 6;

        assert!(matches!(
            handle_insert(&app, &key(KeyCode::Up)).as_slice(),
            [Action::Move(Dir::Up)]
        ));
        assert!(matches!(
            handle_insert(&app, &key(KeyCode::Down)).as_slice(),
            [Action::Move(Dir::Down)]
        ));
    }

    #[test]
    fn plain_enter_submits_in_insert_mode() {
        let app = test_app();

        let actions = handle_key(&app, key(KeyCode::Enter));

        assert!(matches!(actions.as_slice(), [Action::Submit]));
    }

    #[test]
    fn tab_accepts_first_suggestion_in_empty_insert_composer() {
        let mut app = test_app();
        app.sessions.active_mut().response_suggestions = vec!["Run the tests".into()];

        let actions = handle_key(&app, key(KeyCode::Tab));

        assert!(matches!(
            actions.as_slice(),
            [Action::AcceptResponseSuggestion(0)]
        ));
    }

    #[test]
    fn tab_inserts_tab_without_a_visible_suggestion() {
        let app = test_app();

        let actions = handle_key(&app, key(KeyCode::Tab));

        assert!(matches!(actions.as_slice(), [Action::InsertChar('\t')]));
    }

    #[test]
    fn ctrl_j_and_ctrl_k_navigate_command_palette() {
        assert!(matches!(
            handle_palette(&ctrl_key(KeyCode::Char('j'))).as_slice(),
            [Action::PickerDown]
        ));
        assert!(matches!(
            handle_palette(&ctrl_key(KeyCode::Char('k'))).as_slice(),
            [Action::PickerUp]
        ));
    }

    #[test]
    fn plain_j_remains_search_text_in_command_palette() {
        assert!(matches!(
            handle_palette(&key(KeyCode::Char('j'))).as_slice(),
            [Action::PickerChar('j')]
        ));
    }

    #[test]
    fn default_jk_chord_backspaces_j_and_enters_normal() {
        let mut app = test_app();
        app.last_insert = Some('j');

        let actions = handle_insert(&app, &key(KeyCode::Char('k')));

        assert!(matches!(
            actions.as_slice(),
            [Action::Backspace, Action::EnterNormal]
        ));
    }

    #[test]
    fn custom_two_char_chord_backspaces_first_char_and_enters_normal() {
        let mut config = Config::default();
        config.keybinds.normal = "fd".into();
        let mut app = App::new(config).unwrap();
        app.vim = VimMode::Insert;
        app.last_insert = Some('f');

        let actions = handle_insert(&app, &key(KeyCode::Char('d')));

        assert!(matches!(
            actions.as_slice(),
            [Action::Backspace, Action::EnterNormal]
        ));
    }

    #[test]
    fn chord_fires_on_second_char_after_first() {
        // chord = jk, last typed = 'j', now pressing 'k' → escape
        assert!(chord_escapes(
            Some(('j', 'k')),
            Some('j'),
            KeyCode::Char('k')
        ));
    }

    #[test]
    fn chord_ignores_when_previous_char_differs() {
        assert!(!chord_escapes(
            Some(('j', 'k')),
            Some('x'),
            KeyCode::Char('k')
        ));
        assert!(!chord_escapes(Some(('j', 'k')), None, KeyCode::Char('k')));
    }

    #[test]
    fn permission_enter_uses_automated_review_while_deny_reason_enter_resolves() {
        let mut app = test_app();
        app.overlay = Overlay::Permission(crate::app::overlay::PermissionRequest::single(
            crate::agent::ToolCall {
                name: "read".into(),
                args: serde_json::json!({ "path": "README.md" }),
                id: None,
            },
        ));
        assert!(matches!(
            handle_permission(&app, &key(KeyCode::Enter)).as_slice(),
            [Action::AgentReviewPermission]
        ));

        if let Overlay::Permission(request) = &mut app.overlay {
            request.begin_deny(crate::agent::Permission::Deny);
        }
        assert!(matches!(
            handle_permission(&app, &key(KeyCode::Enter)).as_slice(),
            [Action::AgentResolvePermission]
        ));
    }

    #[test]
    fn editing_access_entry_enter_saves_without_model_review() {
        let mut app = test_app();
        app.overlay = Overlay::Permission(crate::app::overlay::PermissionRequest::single(
            crate::agent::ToolCall {
                name: "read".into(),
                args: serde_json::json!({ "path": "README.md" }),
                id: None,
            },
        ));
        if let Overlay::Permission(request) = &mut app.overlay {
            request.editing_access = Some(0);
        }

        assert!(matches!(
            handle_permission(&app, &key(KeyCode::Enter)).as_slice(),
            [Action::AgentResolvePermission]
        ));
    }

    #[test]
    fn folder_picker_h_and_left_navigate_to_parent() {
        let mut app = test_app();
        app.overlay = Overlay::Permission(crate::app::overlay::PermissionRequest::single(
            crate::agent::ToolCall {
                name: "read".into(),
                args: serde_json::json!({ "path": "README.md" }),
                id: None,
            },
        ));
        if let Overlay::Permission(request) = &mut app.overlay {
            request.folder_picker = Some(crate::app::overlay::FolderPicker::open(
                std::env::current_dir().unwrap(),
            ));
        }

        for code in [KeyCode::Char('h'), KeyCode::Left, KeyCode::Backspace] {
            assert!(matches!(
                handle_permission(&app, &key(code)).as_slice(),
                [Action::AgentPermissionFolderParent]
            ));
        }
    }

    #[test]
    fn permission_quick_actions_remain_independent() {
        let mut app = test_app();
        app.overlay = Overlay::Permission(crate::app::overlay::PermissionRequest::single(
            crate::agent::ToolCall {
                name: "shell".into(),
                args: serde_json::json!({ "command": "cargo test" }),
                id: None,
            },
        ));
        assert!(matches!(
            handle_permission(&app, &key(KeyCode::Char('a'))).as_slice(),
            [Action::AgentQuickAllow]
        ));
        assert!(matches!(
            handle_permission(&app, &key(KeyCode::Char('d'))).as_slice(),
            [Action::AgentQuickDeny]
        ));
        assert!(matches!(
            handle_permission(&app, &key(KeyCode::Char('e'))).as_slice(),
            [Action::AgentPermissionEdit]
        ));
        assert!(matches!(
            handle_permission(&app, &key(KeyCode::Char('p'))).as_slice(),
            [Action::AgentEditPolicy]
        ));
    }

    #[test]
    fn permission_shift_arrows_scroll_code_horizontally() {
        let mut app = test_app();
        app.overlay = Overlay::Permission(crate::app::overlay::PermissionRequest::single(
            crate::agent::ToolCall {
                name: "edit".into(),
                args: serde_json::json!({
                    "path": "src/main.rs",
                    "old": "fn old() {}",
                    "new": "fn new() {}",
                }),
                id: None,
            },
        ));
        assert!(matches!(
            handle_permission(&app, &modified_key(KeyCode::Left, KeyModifiers::SHIFT)).as_slice(),
            [Action::AgentPermScrollLeft]
        ));
        assert!(matches!(
            handle_permission(&app, &modified_key(KeyCode::Right, KeyModifiers::SHIFT)).as_slice(),
            [Action::AgentPermScrollRight]
        ));
    }

    #[test]
    fn chord_ignores_non_second_char() {
        assert!(!chord_escapes(
            Some(('j', 'k')),
            Some('j'),
            KeyCode::Char('z')
        ));
    }

    #[test]
    fn no_chord_configured_never_fires() {
        assert!(!chord_escapes(None, Some('j'), KeyCode::Char('k')));
    }
}
