//! Minimal ANSI theme. Every colour is one of the terminal's 16 ANSI colours (or
//! `Reset`, which uses the terminal's own foreground/background), so the app
//! follows whatever colour scheme the user's terminal defines. The palette is
//! deliberately flat — like Claude Code's TUI, structure comes from layout and
//! whitespace, not from boxes and bright accents. Only semantic colours
//! (success/danger for diffs and tool results) carry real hue.

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    // Text
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
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
    pub tool: Color,
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
        Theme {
            text: Color::Reset,
            muted: Color::Gray,
            faint: Color::DarkGray,
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
            tool: Color::DarkGray,
            thinking: Color::Green,
            link: Color::Reset,
            // Code syntax — ANSI hues so it still follows the terminal palette.
            hl_keyword: Color::Magenta,
            hl_function: Color::Blue,
            hl_type: Color::Yellow,
            hl_string: Color::Green,
            hl_comment: Color::DarkGray,
            hl_number: Color::Cyan,
            hl_constant: Color::Cyan,
            hl_property: Color::Blue,
            hl_variable: Color::Reset,
            hl_punct: Color::Gray,
        }
    }
}

impl Theme {
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
