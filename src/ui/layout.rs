use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub struct AppLayout {
    pub sidebar: Rect,
    pub chat: Rect,
    pub input: Rect,
    pub statusbar: Rect,
}

pub fn compute(area: Rect, sidebar_width: u16, input_height: u16) -> AppLayout {
    let vchunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let main_area = vchunks[0];
    let statusbar = vchunks[1];

    let hchunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(sidebar_width), Constraint::Min(1)])
        .split(main_area);

    let sidebar = hchunks[0];
    let right_area = hchunks[1];

    let rchunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(input_height + 2)])
        .split(right_area);

    AppLayout {
        sidebar,
        chat: rchunks[0],
        input: rchunks[1],
        statusbar,
    }
}
