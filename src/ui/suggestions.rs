use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::app::state::{App, ResponseSuggestionHitbox};
use crate::render::theme::Theme;

pub fn height(app: &App) -> u16 {
    if app.input.text().trim().is_empty() {
        app.sessions.active().response_suggestions.len().min(3) as u16
    } else {
        0
    }
}

pub fn render(
    f: &mut Frame,
    app: &App,
    area: Rect,
    theme: &Theme,
) -> Vec<ResponseSuggestionHitbox> {
    if area.height == 0 || area.width < 4 || !app.input.text().trim().is_empty() {
        return Vec::new();
    }

    app.sessions
        .active()
        .response_suggestions
        .iter()
        .take(area.height as usize)
        .enumerate()
        .map(|(index, suggestion)| {
            let row = Rect {
                x: area.x.saturating_add(2),
                y: area.y + index as u16,
                width: area.width.saturating_sub(4),
                height: 1,
            };
            let key = format!("Alt+{}", index + 1);
            let prefix_width = UnicodeWidthStr::width(key.as_str()) + 3;
            let text = truncate(suggestion, row.width as usize - prefix_width);
            let line = Line::from(vec![
                Span::styled(
                    format!("{} ", key),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("› ", Style::default().fg(theme.muted)),
                Span::styled(text, Style::default().fg(theme.text)),
            ]);
            f.render_widget(Paragraph::new(line), row);
            ResponseSuggestionHitbox {
                suggestion_idx: index,
                area: row,
            }
        })
        .collect()
}

fn truncate(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    let mut out = String::new();
    for ch in text.chars() {
        if UnicodeWidthStr::width(out.as_str())
            + unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0)
            > width - 1
        {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::truncate;

    #[test]
    fn truncates_to_available_width() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }
}
