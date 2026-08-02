use ratatui::layout::Rect;
use ratatui::style::Style;
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

    let sel_range = app.mouse_select.and_then(|sel| {
        if !sel.active {
            return None;
        }
        let r0 = (sel.anchor_row.saturating_sub(area.y)) as usize;
        let r1 = (sel.drag_row.saturating_sub(area.y)) as usize;
        if r0 >= vp_h && r1 >= vp_h {
            return None;
        }
        let r0 = r0.min(vp_h - 1);
        let r1 = r1.min(vp_h - 1);
        Some(r0.min(r1)..=r1.max(r0))
    });

    let lines: Vec<Line<'static>> = chat.doc()[start..end]
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let mut line = row.line.clone();
            if let Some(ref range) = sel_range {
                if range.contains(&i) {
                    line = line.style(Style::new().bg(ratatui::style::Color::DarkGray));
                }
            }
            line
        })
        .collect();

    f.render_widget(Paragraph::new(lines), area);
}
