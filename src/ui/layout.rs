use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub struct AppLayout {
    pub chat: Rect,
    pub activity: Rect,
    pub todo: Rect,
    pub input: Rect,
    pub statusbar: Rect,
}

/// Single-column layout: transcript fills the space, then a one-line activity
/// denoter, optional sticky todo panel, the input box, and the normal status bar.
/// The activity/todo/input cluster sits below the scrollable transcript, while
/// the status bar remains at the bottom.
///
/// `input_height` is the number of text rows the input currently needs (already
/// clamped by the caller); the panel adds 2 for its border. `todo_height` is the
/// full height of the todo panel including its border, or 0 to hide it.
pub fn compute(area: Rect, input_height: u16, todo_height: u16) -> AppLayout {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(todo_height),
            Constraint::Length(input_height + 2),
            Constraint::Length(1),
        ])
        .split(area);

    AppLayout {
        chat: chunks[0],
        activity: chunks[1],
        todo: chunks[2],
        input: chunks[3],
        statusbar: chunks[4],
    }
}
