//! ANSI-only theme. Every colour is one of the terminal's 16 ANSI colours (or
//! `Reset`, which uses the terminal's own foreground/background). This means the
//! app automatically follows whatever colour scheme the user's terminal defines
//! — there are no hard-coded RGB values.

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    // Text
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    // Accents
    pub primary: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub info: Color,
    // Structure
    pub border: Color,
    pub border_focus: Color,
    pub sel_bg: Color,
    pub sel_fg: Color,
    // Roles
    pub user: Color,
    pub assistant: Color,
    pub system: Color,
    pub tool: Color,
    pub thinking: Color,
    pub link: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            text: Color::Reset,
            muted: Color::Gray,
            faint: Color::DarkGray,
            primary: Color::Blue,
            accent: Color::Cyan,
            success: Color::Green,
            warning: Color::Yellow,
            danger: Color::Red,
            info: Color::LightBlue,
            border: Color::DarkGray,
            border_focus: Color::Cyan,
            sel_bg: Color::Blue,
            sel_fg: Color::Black,
            user: Color::Green,
            assistant: Color::Cyan,
            system: Color::Yellow,
            tool: Color::Magenta,
            thinking: Color::DarkGray,
            link: Color::LightBlue,
        }
    }
}

impl Theme {
    pub fn border_style(&self, focused: bool) -> Style {
        Style::default().fg(if focused { self.border_focus } else { self.border })
    }

    /// Block cursor cell — reverse video against the terminal default.
    pub fn cursor(&self) -> Style {
        Style::default().add_modifier(Modifier::REVERSED)
    }

    /// A selected list row.
    pub fn selection(&self) -> Style {
        Style::default().fg(self.sel_fg).bg(self.sel_bg).add_modifier(Modifier::BOLD)
    }

    pub fn title(&self) -> Style {
        Style::default().fg(self.primary).add_modifier(Modifier::BOLD)
    }

    pub fn pill(&self, fg: Color, bg: Color) -> Style {
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD)
    }
}
