use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::app::action::{Action, Dir};
use crate::app::overlay::Overlay;
use crate::app::state::App;
use crate::input::vim::VimMode;

pub fn handle_event(app: &App, event: Event) -> Vec<Action> {
    match event {
        // With keyboard enhancement on, terminals also report key releases —
        // act on presses (and auto-repeats) only, so keys don't fire twice.
        Event::Key(k) if k.kind != KeyEventKind::Release => handle_key(app, k),
        Event::Mouse(m) => handle_mouse(m),
        Event::Resize(_, _) => vec![Action::Resize],
        // A bracketed paste arrives as one blob — smart-paste decides file vs chip.
        Event::Paste(s) => vec![Action::PasteText(s)],
        _ => vec![],
    }
}

fn handle_key(app: &App, key: KeyEvent) -> Vec<Action> {
    let km = &app.keymap;

    // ── Launch screen is fully modal (resume/new), only Ctrl-C escapes ──
    if let Overlay::Startup(_) = app.overlay {
        return handle_startup(&key, km);
    }

    // ── Global shortcuts (fire in any mode, configurable) ───────────────
    if km.quit.matches(&key) {
        return if app.sessions.active().is_streaming() {
            vec![Action::CancelStream]
        } else {
            vec![Action::Quit]
        };
    }
    if km.redraw.matches(&key) { return vec![Action::Resize]; }
    if km.next_session.matches(&key) { return vec![Action::NextSession]; }
    if km.prev_session.matches(&key) { return vec![Action::PrevSession]; }
    if km.session_picker.matches(&key) { return vec![Action::OpenSessionPicker]; }
    if km.fork_session.matches(&key) { return vec![Action::ForkSession]; }
    if km.open_editor.matches(&key) { return vec![Action::OpenEditor]; }
    if km.open_file.matches(&key) { return vec![Action::OpenEditPicker]; }
    if km.open_shell.matches(&key) {
        // While the browser is open, this key closes it too (both keys toggle).
        return if app.overlay.is_browser() { vec![Action::BrowserClose] } else { vec![Action::OpenShell] };
    }
    if km.next_model.matches(&key) { return vec![Action::NextModel]; }
    if km.prev_model.matches(&key) { return vec![Action::PrevModel]; }
    if km.file_picker.matches(&key) { return vec![Action::OpenFilePicker]; }
    if km.model_picker.matches(&key) { return vec![Action::OpenModelPicker]; }
    if km.toggle_agent.matches(&key) { return vec![Action::ToggleAgentMode]; }

    // ── Overlays take priority over the rest ────────────────────────────
    match &app.overlay {
        Overlay::Startup(_) => return handle_startup(&key, km),
        Overlay::Picker(_) => return handle_picker(&key),
        Overlay::Browser(_) => return handle_browser(&key),
        Overlay::Palette(_) => return handle_palette(&key),
        Overlay::Settings(_) => return handle_settings(&key),
        Overlay::Permission(_) => return handle_permission(&key),
        Overlay::ApiSetup(_) => return handle_api_setup(&key),
        // A notice is a plain "OK" dialog: any key dismisses it.
        Overlay::Notice { .. } => return vec![Action::DismissNotice],
        Overlay::None => {}
    }

    // ── Transcript scrolling (works in any input mode) ──────────────────
    if km.scroll_up.matches(&key) { return vec![Action::ChatPageUp]; }
    if km.scroll_down.matches(&key) { return vec![Action::ChatPageDown]; }
    if km.scroll_half_down.matches(&key) { return vec![Action::ChatHalfDown]; }
    if km.scroll_half_up.matches(&key) { return vec![Action::ChatHalfUp]; }
    if km.scroll_top.matches(&key) { return vec![Action::ChatTop]; }
    if km.scroll_bottom.matches(&key) { return vec![Action::ChatBottom]; }
    if km.toggle_output.matches(&key) { return vec![Action::ToggleOutput]; }

    // ── Vim modes for the input box ─────────────────────────────────────
    match app.vim {
        VimMode::Insert => handle_insert(app, &key),
        VimMode::Normal => handle_normal(app, &key),
        VimMode::Command => handle_command(app, &key),
        VimMode::Visual => handle_visual(&key),
        VimMode::Operator(op) => handle_operator(&key, op),
    }
}

fn ctrl_pressed(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
}

// ── Overlay handlers ──────────────────────────────────────────────────────────

fn handle_startup(key: &KeyEvent, km: &crate::input::keymap::Keymap) -> Vec<Action> {
    if km.quit.matches(key) {
        return vec![Action::Quit];
    }
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => vec![Action::StartupDown],
        KeyCode::Char('k') | KeyCode::Up => vec![Action::StartupUp],
        KeyCode::Char('n') => vec![Action::StartupNew],
        KeyCode::Char('l') | KeyCode::Enter | KeyCode::Right => vec![Action::StartupConfirm],
        // Esc / q dismiss the launcher, resuming the last-active session as loaded.
        KeyCode::Esc | KeyCode::Char('q') => vec![Action::PickerCancel],
        _ => vec![],
    }
}

fn handle_picker(key: &KeyEvent) -> Vec<Action> {
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

fn handle_settings(key: &KeyEvent) -> Vec<Action> {
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

fn handle_permission(key: &KeyEvent) -> Vec<Action> {
    match key.code {
        KeyCode::Esc => vec![Action::AgentCancel],
        KeyCode::Up | KeyCode::Char('k') if !ctrl_pressed(key) => vec![Action::PickerUp],
        KeyCode::Down | KeyCode::Char('j') if !ctrl_pressed(key) => vec![Action::PickerDown],
        KeyCode::Enter => vec![Action::AgentPermitOnce],
        KeyCode::Char('a') if !ctrl_pressed(key) => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                vec![Action::AgentPermitAll]
            } else {
                vec![Action::AgentPermitOnce]
            }
        }
        KeyCode::Char('d') if !ctrl_pressed(key) => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                vec![Action::AgentDenyAll]
            } else {
                vec![Action::AgentDenyOnce]
            }
        }
        _ => vec![],
    }
}

// ── Vim mode handlers (input box only) ─────────────────────────────────────────

fn handle_normal(app: &App, key: &KeyEvent) -> Vec<Action> {
    // Configurable mode-switch / action keys first.
    let km = &app.keymap;
    if km.insert.matches(key) { return vec![Action::EnterInsert]; }
    if km.command.matches(key) { return vec![Action::EnterCommand]; }
    if km.palette.matches(key) { return vec![Action::OpenCommandPalette]; }
    if km.help.matches(key) { return vec![Action::ToggleHelp]; }
    if km.submit.matches(key) { return vec![Action::Submit]; }
    if km.visual.matches(key) { return vec![Action::EnterVisual]; }

    // Fixed vim motions / edits (standard vim, not remapped).
    match key.code {
        KeyCode::Esc => vec![],
        KeyCode::Char('I') => vec![Action::EnterInsert, Action::LineStart],
        KeyCode::Char('a') => vec![Action::EnterInsert, Action::Move(Dir::Right)],
        KeyCode::Char('A') => vec![Action::EnterInsert, Action::LineEnd],
        KeyCode::Char('o') => vec![Action::EnterInsert, Action::Newline],
        KeyCode::Char('O') => vec![Action::LineStart, Action::EnterInsert, Action::Newline, Action::Move(Dir::Up), Action::LineEnd],
        KeyCode::Char('h') | KeyCode::Left => vec![Action::Move(Dir::Left)],
        KeyCode::Char('j') | KeyCode::Down => vec![Action::Move(Dir::Down)],
        KeyCode::Char('k') | KeyCode::Up => vec![Action::Move(Dir::Up)],
        KeyCode::Char('l') | KeyCode::Right => vec![Action::Move(Dir::Right)],
        KeyCode::Char('w') => vec![Action::Move(Dir::WordForward)],
        KeyCode::Char('b') => vec![Action::Move(Dir::WordBackward)],
        KeyCode::Char('0') => vec![Action::LineStart],
        KeyCode::Char('^') => vec![Action::LineStart],
        KeyCode::Char('$') => vec![Action::LineEnd],
        KeyCode::Char('x') => vec![Action::DeleteAt],
        KeyCode::Char('d') => vec![Action::EnterOperator('d')],
        KeyCode::Char('y') => vec![Action::YankLine],
        KeyCode::Char('p') => vec![Action::Paste],
        KeyCode::Char('D') => vec![Action::DeleteAt, Action::LineEnd],
        KeyCode::Char('u') => vec![Action::Backspace],
        KeyCode::Backspace => vec![Action::Backspace],
        _ => vec![],
    }
}

fn handle_insert(app: &App, key: &KeyEvent) -> Vec<Action> {
    // While the @mention popup is open, the arrow keys / Enter drive it.
    if app.mention.active && !app.mention.matches.is_empty() {
        match key.code {
            KeyCode::Up => return vec![Action::MentionUp],
            KeyCode::Down => return vec![Action::MentionDown],
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
    if ctrl && matches!(key.code, KeyCode::Backspace) {
        return vec![Action::DeleteWordBack];
    }
    if ctrl && key.code == KeyCode::Char('w') {
        return vec![Action::DeleteWordBack];
    }
    if ctrl && matches!(key.code, KeyCode::Delete) {
        return vec![Action::DeleteWordForward];
    }

    // Enter sends the message (same as :w); Shift/Alt/Ctrl+Enter inserts a newline.
    if key.code == KeyCode::Enter {
        let newline = key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL);
        return if newline { vec![Action::Newline] } else { vec![Action::Submit] };
    }
    // Honour the `submit` binding if it's mapped to a non-Enter key too.
    if app.keymap.submit.matches(key) {
        return vec![Action::Submit];
    }

    match key.code {
        KeyCode::Backspace => vec![Action::Backspace],
        KeyCode::Delete => vec![Action::DeleteAt],
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

fn handle_command(app: &App, key: &KeyEvent) -> Vec<Action> {
    match key.code {
        KeyCode::Esc => vec![Action::EnterNormal],
        KeyCode::Enter => vec![Action::RunCommand(app.command.clone())],
        KeyCode::Backspace => vec![Action::CommandBackspace],
        KeyCode::Up => vec![Action::CommandHistoryPrev],
        KeyCode::Down => vec![Action::CommandHistoryNext],
        KeyCode::Char(c) => vec![Action::CommandChar(c)],
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
        KeyCode::Char('0') => vec![Action::LineStart],
        KeyCode::Char('$') => vec![Action::LineEnd],
        KeyCode::Char('y') => vec![Action::VisualYank],
        KeyCode::Char('d') | KeyCode::Char('x') => vec![Action::VisualDelete],
        KeyCode::Char('c') | KeyCode::Char('s') => vec![Action::VisualChange],
        _ => vec![],
    }
}

fn handle_operator(key: &KeyEvent, _op: char) -> Vec<Action> {
    match key.code {
        KeyCode::Char('d') => vec![Action::DeleteLine],
        KeyCode::Char('y') => vec![Action::YankLine],
        _ => vec![Action::EnterNormal],
    }
}

/// Whether `key` completes the insert-escape chord: it is the chord's second
/// char and the immediately preceding inserted char was the first.
fn chord_escapes(chord: Option<(char, char)>, last_insert: Option<char>, key: KeyCode) -> bool {
    match (chord, key) {
        (Some((c1, c2)), KeyCode::Char(c)) => {
            c.eq_ignore_ascii_case(&c2) && last_insert.map_or(false, |p| p.eq_ignore_ascii_case(&c1))
        }
        _ => false,
    }
}

// ── Mouse handler ─────────────────────────────────────────────────────────────

fn handle_mouse(mouse: MouseEvent) -> Vec<Action> {
    match mouse.kind {
        MouseEventKind::ScrollUp => vec![Action::ChatScroll(3)],
        MouseEventKind::ScrollDown => vec![Action::ChatScroll(-3)],
        MouseEventKind::Down(MouseButton::Left) => {
            vec![Action::ChatClick(mouse.column, mouse.row)]
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chord_fires_on_second_char_after_first() {
        // chord = jk, last typed = 'j', now pressing 'k' → escape
        assert!(chord_escapes(Some(('j', 'k')), Some('j'), KeyCode::Char('k')));
    }

    #[test]
    fn chord_ignores_when_previous_char_differs() {
        assert!(!chord_escapes(Some(('j', 'k')), Some('x'), KeyCode::Char('k')));
        assert!(!chord_escapes(Some(('j', 'k')), None, KeyCode::Char('k')));
    }

    #[test]
    fn chord_ignores_non_second_char() {
        assert!(!chord_escapes(Some(('j', 'k')), Some('j'), KeyCode::Char('z')));
    }

    #[test]
    fn no_chord_configured_never_fires() {
        assert!(!chord_escapes(None, Some('j'), KeyCode::Char('k')));
    }
}
