//! The sticky task panel: the agent's todo breakdown, pinned directly above the
//! input box. It sits outside the transcript, so it never scrolls away.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use crate::app::state::{App, TodoStatus};
use crate::render::theme::Theme;

/// Most task rows to show before eliding, so a long list can't swallow the screen.
const MAX_VISIBLE: usize = 8;

/// Total panel height (rows + border) for the current todo list, or 0 when there
/// are none — the layout uses this to size (or hide) the panel.
pub fn height(todo_count: usize) -> u16 {
    if todo_count == 0 {
        return 0;
    }
    let rows = todo_count.min(MAX_VISIBLE) + usize::from(todo_count > MAX_VISIBLE);
    rows as u16 + 2 // top + bottom border
}

pub fn render(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    if app.todos.is_empty() || area.height < 3 {
        return;
    }

    let done = app
        .todos
        .iter()
        .filter(|t| t.status == TodoStatus::Done)
        .count();
    let title = format!(" Tasks {}/{} ", done, app.todos.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.muted))
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    for todo in app.todos.iter().take(MAX_VISIBLE) {
        let (glyph_style, text_style) = match todo.status {
            TodoStatus::Done => (
                Style::default().fg(theme.success),
                Style::default()
                    .fg(theme.muted)
                    .add_modifier(Modifier::CROSSED_OUT),
            ),
            TodoStatus::InProgress => (
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
            TodoStatus::Pending => (
                Style::default().fg(theme.muted),
                Style::default().fg(theme.text),
            ),
        };
        // `glyph␠` + text, truncated to the inner width so a long task never wraps
        // the fixed-height panel.
        let prefix = format!("{} ", todo.status.glyph());
        let avail = width.saturating_sub(prefix.chars().count());
        let text = truncate_cols(&todo.text, avail);
        lines.push(Line::from(vec![
            Span::styled(prefix, glyph_style),
            Span::styled(text, text_style),
        ]));
    }
    if app.todos.len() > MAX_VISIBLE {
        lines.push(Line::from(Span::styled(
            format!("  … {} more", app.todos.len() - MAX_VISIBLE),
            Style::default().fg(theme.faint),
        )));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

/// Truncate `s` to at most `max` display columns, adding an ellipsis when cut.
fn truncate_cols(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let take = max.saturating_sub(1);
    let mut out: String = s.chars().take(take).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn height_is_zero_when_empty_else_rows_plus_border() {
        assert_eq!(height(0), 0);
        assert_eq!(height(1), 3); // 1 row + 2 border
        assert_eq!(height(MAX_VISIBLE), MAX_VISIBLE as u16 + 2);
        // Over the cap: capped rows + one "… N more" row + border.
        assert_eq!(height(MAX_VISIBLE + 5), MAX_VISIBLE as u16 + 1 + 2);
    }

    #[test]
    fn truncate_adds_ellipsis_only_when_cut() {
        assert_eq!(truncate_cols("hello", 10), "hello");
        assert_eq!(truncate_cols("hello", 3), "he…");
        assert_eq!(truncate_cols("hello", 0), "");
    }
}
