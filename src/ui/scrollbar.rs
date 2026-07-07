use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::Frame;

use crate::app::state::App;
use crate::render::theme::Theme;

/// Draw a compact two-column scroll rail: a faint rounded-looking track, a soft
/// thumb, and tiny coloured turn markers beside it.
pub fn render(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let h = area.height as usize;
    let doc = app.chat.doc();
    let total = doc.len();
    let rail_x = area.x + area.width.saturating_sub(1);
    let mark_x = area.x;

    let track_style = Style::default().fg(theme.faint);
    for i in 0..h {
        let glyph = if i == 0 {
            "╷"
        } else if i + 1 == h {
            "╵"
        } else {
            "│"
        };
        f.render_widget(
            ratatui::widgets::Paragraph::new(Span::styled(glyph, track_style)),
            Rect {
                x: rail_x,
                y: area.y + i as u16,
                width: 1,
                height: 1,
            },
        );
    }

    if total == 0 {
        return;
    }

    let viewport = area.height as usize;
    let scroll = app.chat.scroll.min(total);
    let thumb_len = ((viewport * viewport) / total.max(1)).max(2).min(h);
    let max_start = h.saturating_sub(thumb_len);
    let thumb_start = if total <= viewport {
        0
    } else {
        (scroll * max_start) / total.saturating_sub(viewport).max(1)
    };
    let thumb_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    for i in thumb_start..(thumb_start + thumb_len).min(h) {
        let glyph = if i == thumb_start {
            "╻"
        } else if i + 1 == (thumb_start + thumb_len).min(h) {
            "╹"
        } else {
            "┃"
        };
        f.render_widget(
            ratatui::widgets::Paragraph::new(Span::styled(glyph, thumb_style)),
            Rect {
                x: rail_x,
                y: area.y + i as u16,
                width: 1,
                height: 1,
            },
        );
    }

    for (idx, row) in doc.iter().enumerate() {
        let Some(role) = row.role_start else { continue };
        let color = match role {
            "user" => theme.gutter_user,
            "assistant" => theme.gutter_assistant,
            "tool" => theme.gutter_tool,
            "system" => theme.gutter_system,
            _ => Color::Reset,
        };
        let y = (idx * h.saturating_sub(1)) / total.max(1);
        let glyph = match role {
            "user" => "•",
            "assistant" => "✦",
            "tool" => "·",
            "system" => "◆",
            _ => "·",
        };
        f.render_widget(
            ratatui::widgets::Paragraph::new(Span::styled(
                glyph,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )),
            Rect {
                x: mark_x,
                y: area.y + y as u16,
                width: 1,
                height: 1,
            },
        );
    }
}
