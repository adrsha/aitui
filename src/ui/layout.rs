use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub struct AppLayout {
    pub chat: Rect,
    pub todo: Rect,
    pub input: Rect,
    pub statusbar: Rect,
}

/// Single-column layout: transcript fills the space, then an optional sticky todo
/// panel, then the input box, then a one-line status bar. The todo panel and input
/// sit *below* the scrollable transcript, so neither scrolls with it.
///
/// `input_height` is the number of text rows the input currently needs (already
/// clamped by the caller); the panel adds 2 for its border. `todo_height` is the
/// full height of the todo panel including its border, or 0 to hide it.
pub fn compute(area: Rect, input_height: u16, todo_height: u16) -> AppLayout {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(todo_height),
            Constraint::Length(input_height + 2),
            Constraint::Length(1),
        ])
        .split(area);

    AppLayout {
        chat: chunks[0],
        todo: chunks[1],
        input: chunks[2],
        statusbar: chunks[3],
    }
}
