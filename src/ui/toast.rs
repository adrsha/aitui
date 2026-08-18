use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::state::App;
use crate::app::toast::ToastLevel;
use crate::render::theme::{fg_guard, Theme};
use crate::render::wrap::wrap_words;

const MAX_WIDTH: u16 = 58;
const MAX_BODY_LINES: usize = 3;
const SCREEN_MARGIN: u16 = 1;

pub fn render(f: &mut Frame, app: &App, theme: &Theme) {
    let Some(toast) = app.toasts.back() else {
        return;
    };
    let Some((area, lines)) = geometry(f.area(), &toast.message) else {
        return;
    };
    let (title, icon, color) = match toast.level {
        ToastLevel::Warning => ("Warning", "▲", theme.warning),
        ToastLevel::Error => ("Error", "×", theme.danger),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color))
        .style(theme.surface())
        .title(Line::from(vec![
            Span::styled(
                format!(" {icon} "),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{title} "),
                Style::default()
                    .fg(fg_guard(theme.text))
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(block).style(theme.surface()),
        area,
    );
}

fn geometry(screen: Rect, message: &str) -> Option<(Rect, Vec<Line<'static>>)> {
    if screen.width < 12 || screen.height < 4 {
        return None;
    }
    let width = MAX_WIDTH
        .min(screen.width.saturating_sub(SCREEN_MARGIN * 2))
        .max(12);
    let inner_width = width.saturating_sub(2).max(1) as usize;
    let mut wrapped = wrap_words(&message.replace(['\n', '\r'], " "), inner_width);
    let truncated = wrapped.len() > MAX_BODY_LINES;
    wrapped.truncate(MAX_BODY_LINES);
    if truncated {
        if let Some(last) = wrapped.last_mut() {
            let mut chars: Vec<char> = last.chars().collect();
            if chars.len() >= inner_width {
                chars.truncate(inner_width.saturating_sub(1));
            }
            *last = chars.into_iter().collect::<String>();
            last.push('…');
        }
    }
    let height = (wrapped.len() as u16).saturating_add(2).min(screen.height);
    let area = Rect::new(
        screen
            .x
            .saturating_add(screen.width.saturating_sub(width + SCREEN_MARGIN)),
        screen.y.saturating_add(SCREEN_MARGIN),
        width,
        height,
    );
    let lines = wrapped.into_iter().map(Line::from).collect();
    Some((area, lines))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_is_bounded_and_does_not_change_layout() {
        let screen = Rect::new(0, 0, 120, 40);
        let (area, lines) = geometry(screen, &"warning ".repeat(100)).unwrap();
        assert!(area.width <= MAX_WIDTH);
        assert!(area.height <= MAX_BODY_LINES as u16 + 2);
        assert!(area.right() <= screen.right());
        assert!(area.bottom() <= screen.bottom());
        assert!(lines.len() <= MAX_BODY_LINES);
    }

    #[test]
    fn toast_hides_when_terminal_is_too_small() {
        assert!(geometry(Rect::new(0, 0, 10, 3), "error").is_none());
    }
}
