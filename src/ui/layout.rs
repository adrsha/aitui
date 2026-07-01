use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub struct AppLayout {
    pub chat: Rect,
    pub input: Rect,
    pub statusbar: Rect,
}

/// Single-column layout: transcript fills the space, a fixed-height input box
/// sits at the bottom, and a one-line status bar underneath it. Panels span the
/// full width; breathing room lives *inside* the input panel (see `ui/input.rs`).
pub fn compute(area: Rect, input_height: u16) -> AppLayout {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(input_height + 2),
            Constraint::Length(1),
        ])
        .split(area);

    AppLayout {
        chat: chunks[0],
        input: chunks[1],
        statusbar: chunks[2],
    }
}
