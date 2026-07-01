use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::state::App;
use crate::render::theme::Theme;

/// Render the transcript: a borderless, scrolling pane of pre-wrapped rows.
/// Only the visible slice is drawn (virtualization).
pub fn render(f: &mut Frame, app: &mut App, area: Rect, _theme: &Theme) {
    let vp_h = area.height as usize;
    let chat = &mut app.chat;
    let total = chat.doc().len();
    let max_scroll = total.saturating_sub(vp_h);
    if chat.scroll > max_scroll {
        chat.scroll = max_scroll;
    }
    let start = chat.scroll;
    let end = (start + vp_h).min(total);

    let lines: Vec<Line<'static>> =
        chat.doc()[start..end].iter().map(|row| row.line.clone()).collect();

    f.render_widget(Paragraph::new(lines), area);
}
