use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::Frame;

use crate::app::state::App;
use crate::render::theme::Theme;

/// Draw a one-column scrollbar at `area` (a single column on the right of the
/// transcript). The track shows the scroll thumb; coloured pips mark where each
/// turn starts — cyan for your messages, gray for the assistant, green for tool
/// results — so the whole conversation is glanceable at a distance.
pub fn render(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let h = area.height as usize;
    let doc = app.chat.doc();
    let total = doc.len();

    // Track background.
    let track_style = Style::default().fg(theme.faint);
    for i in 0..h {
        f.render_widget(
            ratatui::widgets::Paragraph::new(Span::styled("│", track_style)),
            Rect { x: area.x, y: area.y + i as u16, width: 1, height: 1 },
        );
    }

    if total == 0 {
        return;
    }

    // Thumb: proportional block covering the visible slice.
    let scroll = app.chat.scroll.min(total);
    let thumb_start = (scroll * h) / total.max(1);
    let thumb_len = ((h * h) / total.max(1)).max(1).min(h);
    let thumb_style = Style::default().fg(theme.muted).add_modifier(Modifier::BOLD);
    for i in thumb_start..(thumb_start + thumb_len).min(h) {
        f.render_widget(
            ratatui::widgets::Paragraph::new(Span::styled("█", thumb_style)),
            Rect { x: area.x, y: area.y + i as u16, width: 1, height: 1 },
        );
    }

    // Role markers: map each turn's start row onto the track.
    for (idx, row) in doc.iter().enumerate() {
        let Some(role) = row.role_start else { continue };
        let color = match role {
            "user" => theme.gutter_user,
            "tool" => theme.gutter_tool,
            "system" => theme.gutter_system,
            _ => Color::White,
        };
        let y = (idx * (h.saturating_sub(1))) / total.max(1);
        f.render_widget(
            ratatui::widgets::Paragraph::new(Span::styled("▐", Style::default().fg(color))),
            Rect { x: area.x, y: area.y + y as u16, width: 1, height: 1 },
        );
    }
}
