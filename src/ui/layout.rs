use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub struct AppLayout {
    pub promptbar: Rect,
    pub chat: Rect,
    pub activity: Rect,
    pub input: Rect,
    pub statusbar: Rect,
    pub sidebar: Rect,
}

/// Minimum terminal width to show the side panel.
pub const SIDEBAR_SHOW_THRESHOLD: u16 = 100;
/// Width of the side panel when visible.
pub fn sidebar_width(total: u16) -> u16 {
    if total < SIDEBAR_SHOW_THRESHOLD {
        0
    } else {
        (total / 5).max(22).min(30)
    }
}

/// Header, transcript, one-line activity denoter, input box, and normal status
/// bar. When wide enough, a side panel is shown on the right with metadata,
/// tasks, and child agents.
pub fn compute(
    area: Rect,
    topbar_height: u16,
    promptbar_height: u16,
    input_panel_height: u16,
    todo_height: u16,
) -> AppLayout {
    let _ = todo_height;
    let side_w = sidebar_width(area.width);
    let main_area = if side_w > 0 {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(side_w)])
            .split(area);
        chunks[0]
    } else {
        area
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(topbar_height),
            Constraint::Length(1),
            Constraint::Length(promptbar_height),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(input_panel_height),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(main_area);

    let sidebar = if side_w > 0 {
        Rect {
            x: area.x + main_area.width,
            y: area.y,
            width: side_w,
            height: area.height,
        }
    } else {
        Rect::default()
    };

    AppLayout {
        promptbar: chunks[2],
        chat: chunks[3],
        activity: chunks[4],
        input: chunks[5],
        statusbar: chunks[7],
        sidebar,
    }
}

#[cfg(test)]
mod tests {
    use super::compute;
    use ratatui::layout::Rect;

    #[test]
    fn input_slot_uses_full_requested_panel_height() {
        let layout = compute(Rect::new(0, 0, 100, 40), 2, 0, 18, 0);
        assert_eq!(layout.input.height, 18);
        assert!(layout.chat.height > 0);
        assert_eq!(layout.input.y, layout.activity.y + layout.activity.height);
    }

    #[test]
    fn activity_sits_directly_between_chat_and_input() {
        let layout = compute(Rect::new(0, 0, 100, 40), 0, 0, 3, 0);
        assert_eq!(layout.activity.y, layout.chat.y + layout.chat.height);
        assert_eq!(layout.input.y, layout.activity.y + layout.activity.height);
    }

    #[test]
    fn sidebar_appears_on_wide_terminals() {
        let wide = compute(Rect::new(0, 0, 140, 40), 0, 0, 3, 0);
        assert!(wide.sidebar.width >= 22);
        assert_eq!(wide.chat.width + wide.sidebar.width, 140);
    }

    #[test]
    fn sidebar_hidden_on_narrow_terminals() {
        let narrow = compute(Rect::new(0, 0, 80, 40), 0, 0, 3, 0);
        assert_eq!(narrow.sidebar.width, 0);
    }
}
