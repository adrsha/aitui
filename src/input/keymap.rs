//! Parsing of config key-spec strings (`"ctrl-n"`, `"esc"`, `"pageup"`, `"i"`)
//! into matchers, and a `Keymap` of every configurable binding precompiled once
//! at startup so the hot input path only compares, never parses.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::config::KeybindConfig;

/// A single parsed key binding: a key code plus required ctrl/alt modifiers.
/// Shift is folded into the character itself (e.g. `?`), so it isn't tracked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySpec {
    code: KeyCode,
    ctrl: bool,
    alt: bool,
}

impl KeySpec {
    /// Parse a spec like `"ctrl-n"`, `"alt-x"`, `"?"`, `"pageup"`. Case-insensitive.
    pub fn parse(spec: &str) -> Option<KeySpec> {
        let mut rest = spec.trim();
        if rest.is_empty() {
            return None;
        }
        let mut ctrl = false;
        let mut alt = false;
        loop {
            // Prefixes are ASCII; compare case-insensitively without allocating.
            if let Some(r) = strip_prefix_ci(rest, "ctrl-").or_else(|| strip_prefix_ci(rest, "c-")) {
                ctrl = true;
                rest = r;
            } else if let Some(r) = strip_prefix_ci(rest, "alt-").or_else(|| strip_prefix_ci(rest, "a-")) {
                alt = true;
                rest = r;
            } else if let Some(r) = strip_prefix_ci(rest, "shift-").or_else(|| strip_prefix_ci(rest, "s-")) {
                // Shift is represented by the resulting character; nothing to record.
                rest = r;
            } else {
                break;
            }
        }
        Some(KeySpec { code: parse_code(rest)?, ctrl, alt })
    }

    /// Does this binding match a key event?
    pub fn matches(&self, ev: &KeyEvent) -> bool {
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        let alt = ev.modifiers.contains(KeyModifiers::ALT);
        if ctrl != self.ctrl || alt != self.alt {
            return false;
        }
        match (self.code, ev.code) {
            (KeyCode::Char(a), KeyCode::Char(b)) => a.eq_ignore_ascii_case(&b),
            (a, b) => a == b,
        }
    }

    /// A human-readable label for the help overlay (e.g. `Ctrl-N`, `PgUp`).
    pub fn label(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("Ctrl-");
        }
        if self.alt {
            s.push_str("Alt-");
        }
        s.push_str(&code_label(self.code));
        s
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

fn parse_code(s: &str) -> Option<KeyCode> {
    let code = match s.to_ascii_lowercase().as_str() {
        "esc" | "escape" => KeyCode::Esc,
        "enter" | "return" | "cr" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Char(' '),
        "backspace" | "bs" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" | "pgdown" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        _ => {
            // Otherwise it must be a single character (use the raw, non-lowered
            // form so `?`, `]`, etc. survive).
            let mut chars = s.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                return None; // more than one char and not a known name
            }
            KeyCode::Char(ch.to_ascii_lowercase())
        }
    };
    Some(code)
}

fn code_label(code: KeyCode) -> String {
    match code {
        KeyCode::Char(' ') => "Space".to_string(),
        KeyCode::Char(c) => c.to_ascii_uppercase().to_string(),
        KeyCode::Esc => "Esc".to_string(),
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::Backspace => "Backspace".to_string(),
        KeyCode::Delete => "Del".to_string(),
        KeyCode::PageUp => "PgUp".to_string(),
        KeyCode::PageDown => "PgDn".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::Up => "Up".to_string(),
        KeyCode::Down => "Down".to_string(),
        KeyCode::Left => "Left".to_string(),
        KeyCode::Right => "Right".to_string(),
        other => format!("{:?}", other),
    }
}

/// Every configurable binding, parsed once. Built from `KeybindConfig`; an
/// invalid spec falls back to that binding's built-in default so a typo can
/// never leave an action completely unbound.
#[derive(Debug, Clone)]
pub struct Keymap {
    // Global
    pub quit: KeySpec,
    pub next_session: KeySpec,
    pub prev_session: KeySpec,
    pub session_picker: KeySpec,
    pub fork_session: KeySpec,
    pub open_editor: KeySpec,
    pub open_file: KeySpec,
    pub open_shell: KeySpec,
    pub file_picker: KeySpec,
    pub model_picker: KeySpec,
    pub next_model: KeySpec,
    pub prev_model: KeySpec,
    pub toggle_agent: KeySpec,
    pub redraw: KeySpec,
    pub scroll_up: KeySpec,
    pub scroll_down: KeySpec,
    pub scroll_top: KeySpec,
    pub scroll_bottom: KeySpec,
    pub scroll_half_down: KeySpec,
    pub scroll_half_up: KeySpec,
    pub toggle_output: KeySpec,
    // Normal mode
    pub insert: KeySpec,
    pub command: KeySpec,
    pub palette: KeySpec,
    pub help: KeySpec,
    pub submit: KeySpec,
    pub visual: KeySpec,
    // Insert mode
    pub normal: KeySpec,
    /// Optional two-key chord (e.g. `jk`) that also leaves insert mode. Set when
    /// the `normal` binding is a 2-char plain sequence; Esc still works too.
    pub normal_chord: Option<(char, char)>,
}

impl Keymap {
    pub fn from_config(k: &KeybindConfig) -> Self {
        let d = KeybindConfig::default();
        // Parse `spec`, falling back to the built-in default if it's invalid.
        let p = |spec: &str, default: &str| {
            KeySpec::parse(spec)
                .or_else(|| KeySpec::parse(default))
                .expect("built-in default key spec must parse")
        };
        let (normal, normal_chord) = parse_escape(&k.normal);
        Keymap {
            quit: p(&k.quit, &d.quit),
            next_session: p(&k.next_session, &d.next_session),
            prev_session: p(&k.prev_session, &d.prev_session),
            session_picker: p(&k.session_picker, &d.session_picker),
            fork_session: p(&k.fork_session, &d.fork_session),
            open_editor: p(&k.open_editor, &d.open_editor),
            open_file: p(&k.open_file, &d.open_file),
            open_shell: p(&k.open_shell, &d.open_shell),
            file_picker: p(&k.file_picker, &d.file_picker),
            model_picker: p(&k.model_picker, &d.model_picker),
            next_model: p(&k.next_model, &d.next_model),
            prev_model: p(&k.prev_model, &d.prev_model),
            toggle_agent: p(&k.toggle_agent, &d.toggle_agent),
            redraw: p(&k.redraw, &d.redraw),
            scroll_up: p(&k.scroll_up, &d.scroll_up),
            scroll_down: p(&k.scroll_down, &d.scroll_down),
            scroll_top: p(&k.scroll_top, &d.scroll_top),
            scroll_bottom: p(&k.scroll_bottom, &d.scroll_bottom),
            scroll_half_down: p(&k.scroll_half_down, &d.scroll_half_down),
            scroll_half_up: p(&k.scroll_half_up, &d.scroll_half_up),
            toggle_output: p(&k.toggle_output, &d.toggle_output),
            insert: p(&k.insert, &d.insert),
            command: p(&k.command, &d.command),
            palette: p(&k.palette, &d.palette),
            help: p(&k.help, &d.help),
            submit: p(&k.submit, &d.submit),
            visual: p(&k.visual, &d.visual),
            normal,
            normal_chord,
        }
    }

    /// Label for the help overlay, e.g. `jk / Esc` or just `Esc`.
    pub fn normal_label(&self) -> String {
        match self.normal_chord {
            Some((a, b)) => format!("{}{} / {}", a, b, self.normal.label()),
            None => self.normal.label(),
        }
    }
}

/// Parse the insert→normal binding, which may be a single key (`esc`, `ctrl-[`)
/// or a two-character chord (`jk`). For a chord, Esc is kept as a fallback so
/// there is always a single-key escape.
fn parse_escape(spec: &str) -> (KeySpec, Option<(char, char)>) {
    if let Some(single) = KeySpec::parse(spec) {
        return (single, None);
    }
    let chars: Vec<char> = spec.trim().chars().collect();
    let esc = KeySpec::parse("esc").expect("esc parses");
    if chars.len() == 2 && chars.iter().all(|c| !c.is_whitespace()) {
        let chord = (chars[0].to_ascii_lowercase(), chars[1].to_ascii_lowercase());
        return (esc, Some(chord));
    }
    (esc, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent { code, modifiers: mods, kind: KeyEventKind::Press, state: crossterm::event::KeyEventState::NONE }
    }

    #[test]
    fn parses_ctrl_combo() {
        let s = KeySpec::parse("ctrl-n").unwrap();
        assert!(s.matches(&ev(KeyCode::Char('n'), KeyModifiers::CONTROL)));
        assert!(!s.matches(&ev(KeyCode::Char('n'), KeyModifiers::NONE)));
    }

    #[test]
    fn plain_char_requires_no_modifiers() {
        let s = KeySpec::parse("i").unwrap();
        assert!(s.matches(&ev(KeyCode::Char('i'), KeyModifiers::NONE)));
        assert!(!s.matches(&ev(KeyCode::Char('i'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn shift_char_matches_ignoring_shift_mod() {
        let s = KeySpec::parse("?").unwrap();
        assert!(s.matches(&ev(KeyCode::Char('?'), KeyModifiers::SHIFT)));
        assert!(s.matches(&ev(KeyCode::Char('?'), KeyModifiers::NONE)));
    }

    #[test]
    fn parses_named_keys() {
        assert!(KeySpec::parse("pageup").unwrap().matches(&ev(KeyCode::PageUp, KeyModifiers::NONE)));
        assert!(KeySpec::parse("esc").unwrap().matches(&ev(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(KeySpec::parse("ctrl-home").unwrap().matches(&ev(KeyCode::Home, KeyModifiers::CONTROL)));
        assert!(KeySpec::parse("enter").unwrap().matches(&ev(KeyCode::Enter, KeyModifiers::NONE)));
    }

    #[test]
    fn rejects_garbage() {
        assert!(KeySpec::parse("").is_none());
        assert!(KeySpec::parse("notakey").is_none());
        assert!(KeySpec::parse("ctrl-").is_none());
    }

    #[test]
    fn case_insensitive() {
        assert!(KeySpec::parse("CTRL-N").unwrap().matches(&ev(KeyCode::Char('n'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn keymap_falls_back_on_invalid_spec() {
        let mut cfg = KeybindConfig::default();
        cfg.next_session = "this is not valid".into();
        let km = Keymap::from_config(&cfg);
        // Falls back to the default ctrl-n.
        assert!(km.next_session.matches(&ev(KeyCode::Char('n'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn labels_are_readable() {
        assert_eq!(KeySpec::parse("ctrl-n").unwrap().label(), "Ctrl-N");
        assert_eq!(KeySpec::parse("pageup").unwrap().label(), "PgUp");
        assert_eq!(KeySpec::parse("?").unwrap().label(), "?");
    }

    #[test]
    fn normal_chord_parsed_with_esc_fallback() {
        let mut cfg = KeybindConfig::default();
        cfg.normal = "jk".into();
        let km = Keymap::from_config(&cfg);
        assert_eq!(km.normal_chord, Some(('j', 'k')));
        // Esc still leaves insert mode.
        assert!(km.normal.matches(&ev(KeyCode::Esc, KeyModifiers::NONE)));
        assert_eq!(km.normal_label(), "jk / Esc");
    }

    #[test]
    fn single_key_normal_has_no_chord() {
        let km = Keymap::from_config(&KeybindConfig::default());
        assert!(km.normal_chord.is_none());
        assert!(km.normal.matches(&ev(KeyCode::Esc, KeyModifiers::NONE)));
    }
}
