//! Minimal ANSI theme. Every colour is one of the terminal's 16 ANSI colours (or
//! `Reset`, which uses the terminal's own foreground/background), so the app
//! follows whatever colour scheme the user's terminal defines. The palette is
//! deliberately flat — like Claude Code's TUI, structure comes from layout and
//! whitespace, not from boxes and bright accents. Only semantic colours
//! (success/danger for diffs and tool results) carry real hue.

use ratatui::style::{Color, Modifier, Style};

/// Guard: ANSI 8 (`DarkGray`) is a **background-only** colour in this app — as a
/// foreground it's near-invisible on the dark pill backgrounds we now paint with
/// it. Any colour routed into a foreground slot passes through here first, so
/// ANSI 8 can never end up as text. Everything else is returned untouched.
pub fn fg_guard(c: Color) -> Color {
    match c {
        Color::DarkGray => Color::Reset,
        other => other,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    // Text
    pub text: Color,
    pub muted: Color,
    pub subtle_pill: Color,
    // Accents (kept minimal)
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    // Per-role gutter accents (fg only — the terminal's own palette, so the app
    // follows whatever colour scheme / light-or-dark theme the terminal defines).
    pub gutter_user: Color,
    pub gutter_assistant: Color,
    pub gutter_tool: Color,
    pub gutter_system: Color,
    // Roles / inline
    pub thinking: Color,
    pub link: Color,
    // Syntax highlighting (tree-sitter code previews)
    pub hl_keyword: Color,
    pub hl_function: Color,
    pub hl_type: Color,
    pub hl_string: Color,
    pub hl_comment: Color,
    pub hl_number: Color,
    pub hl_constant: Color,
    pub hl_property: Color,
    pub hl_variable: Color,
    pub hl_punct: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Theme::midnight()
    }
}

impl Theme {
    pub fn named(name: &str) -> Self {
        match name.trim().to_lowercase().as_str() {
            "midnight" | "default" | "terminal" | "" => Self::midnight(),
            "mono" | "monochrome" => Self::mono(),
            _ => Self::midnight(),
        }
    }

    pub fn midnight() -> Self {
        Theme {
            text: Color::Reset,
            muted: Color::Reset,
            // ANSI 8 (DarkGray) as a background pill instead of ANSI 4 (Blue).
            // Background-only — see `fg_guard`; the fg here is White.
            subtle_pill: Color::DarkGray,
            accent: Color::Cyan,
            success: Color::Green,
            warning: Color::Yellow,
            danger: Color::Red,
            // Gutter accents from the ANSI palette (terminal-defined, adapt to
            // light/dark). No custom RGB — always follow the terminal colours.
            gutter_user: Color::Blue,
            gutter_assistant: Color::Red,
            gutter_tool: Color::Green,
            gutter_system: Color::Yellow,
            thinking: Color::Green,
            link: Color::Reset,
            // Code syntax — ANSI hues so it still follows the terminal palette.
            hl_keyword: Color::Magenta,
            hl_function: Color::Blue,
            hl_type: Color::Yellow,
            hl_string: Color::Green,
            hl_comment: Color::Green,
            hl_number: Color::Cyan,
            hl_constant: Color::Cyan,
            hl_property: Color::Blue,
            hl_variable: Color::Reset,
            hl_punct: Color::Reset,
        }
    }

    pub fn mono() -> Self {
        Theme {
            accent: Color::Reset,
            success: Color::Reset,
            warning: Color::Reset,
            danger: Color::Reset,
            gutter_user: Color::Reset,
            gutter_assistant: Color::Reset,
            gutter_tool: Color::Reset,
            gutter_system: Color::Reset,
            thinking: Color::Reset,
            link: Color::Reset,
            hl_keyword: Color::Reset,
            hl_function: Color::Reset,
            hl_type: Color::Reset,
            hl_string: Color::Reset,
            hl_comment: Color::Reset,
            hl_number: Color::Reset,
            hl_constant: Color::Reset,
            hl_property: Color::Reset,
            hl_variable: Color::Reset,
            hl_punct: Color::Reset,
            ..Self::midnight()
        }
    }

    pub fn subtle(&self) -> Style {
        Style::default().fg(fg_guard(self.muted))
    }

    pub fn subtle_pill(&self) -> Style {
        Style::default()
            .bg(self.subtle_pill)
            .fg(fg_guard(Color::White))
            .add_modifier(Modifier::BOLD)
    }

    /// Block cursor cell — reverse video against the terminal default.
    pub fn cursor(&self) -> Style {
        Style::default().add_modifier(Modifier::REVERSED)
    }

    /// A selected list row in a picker overlay — reverse video, so it inverts the
    /// terminal's own fg/bg and adapts to any colour scheme.
    pub fn selection(&self) -> Style {
        Style::default().add_modifier(Modifier::REVERSED)
    }
}
