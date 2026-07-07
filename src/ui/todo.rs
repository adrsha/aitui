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
/// are none — the layout uses this to size (or hide) the panel. When the list
/// overflows, one extra row is reserved for the `↑/↓ N more` scroll indicator.
pub fn height(todo_count: usize) -> u16 {
    if todo_count == 0 {
        return 0;
    }
    let rows = todo_count.min(MAX_VISIBLE) + usize::from(todo_count > MAX_VISIBLE);
    rows as u16 + 2 // top + bottom border
}

/// Choose which slice of a `count`-long todo list to show in a `max`-row window so
/// that `focus` (the active task) stays visible, scrolling as the list progresses.
/// Returns the inclusive-exclusive `[start, end)` range of task indices.
///
/// Kept pure (no `App`) so the scroll math is unit-testable and can't panic.
fn visible_window(count: usize, focus: usize, max: usize) -> (usize, usize) {
    if max == 0 {
        return (0, 0);
    }
    if count <= max {
        return (0, count);
    }
    // Center the focus in the window, then clamp to the list bounds.
    let half = max / 2;
    let start = focus.saturating_sub(half).min(count - max);
    (start, start + max)
}

/// Index of the task the window should keep in view: the first in-progress task,
/// else the last done task (progress frontier), else the top.
fn focus_index(todos: &[crate::app::state::TodoItem]) -> usize {
    if let Some(i) = todos
        .iter()
        .position(|t| t.status == TodoStatus::InProgress)
    {
        return i;
    }
    todos
        .iter()
        .rposition(|t| t.status == TodoStatus::Done)
        .unwrap_or(0)
}

pub fn render(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let todos = &app.sessions.active().todos;
    if todos.is_empty() || area.height < 3 {
        return;
    }

    let done = todos
        .iter()
        .filter(|t| t.status == TodoStatus::Done)
        .count();
    let title = format!(" Tasks {}/{} ", done, todos.len());
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
    let count = todos.len();
    let budget = inner.height as usize;
    let overflow = count > budget;
    // Reserve one row for the scroll indicator when the list overflows.
    let task_rows = if overflow {
        budget.saturating_sub(1).max(1)
    } else {
        budget
    };
    let (start, end) = visible_window(count, focus_index(todos), task_rows);
    let hidden_above = start;
    let hidden_below = count - end;

    let mut lines: Vec<Line> = Vec::new();
    for todo in &todos[start..end] {
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
    if overflow {
        let mut parts: Vec<String> = Vec::new();
        if hidden_above > 0 {
            parts.push(format!("↑ {}", hidden_above));
        }
        if hidden_below > 0 {
            parts.push(format!("↓ {}", hidden_below));
        }
        lines.push(Line::from(Span::styled(
            format!("  {} more", parts.join("  ")),
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

    #[test]
    fn window_shows_all_when_it_fits() {
        assert_eq!(visible_window(3, 0, 5), (0, 3));
        assert_eq!(visible_window(5, 2, 5), (0, 5));
    }

    #[test]
    fn window_follows_focus_and_clamps() {
        // 20 items, 6-row window. Focus near the top pins to the start.
        assert_eq!(visible_window(20, 1, 6), (0, 6));
        // Focus in the middle centers the window.
        assert_eq!(visible_window(20, 10, 6), (7, 13));
        // Focus near the end clamps so the window stays full and in-bounds.
        assert_eq!(visible_window(20, 19, 6), (14, 20));
    }

    #[test]
    fn window_degenerate_inputs_dont_panic() {
        assert_eq!(visible_window(0, 0, 5), (0, 0));
        assert_eq!(visible_window(10, 3, 0), (0, 0));
        assert_eq!(visible_window(10, 99, 4), (6, 10)); // focus past end still clamps
    }

    #[test]
    fn focus_prefers_in_progress_then_last_done() {
        use crate::app::state::{TodoItem, TodoStatus};
        let mk = |s: TodoStatus| TodoItem {
            text: "t".into(),
            status: s,
        };
        let list = vec![
            mk(TodoStatus::Done),
            mk(TodoStatus::Done),
            mk(TodoStatus::InProgress),
            mk(TodoStatus::Pending),
        ];
        assert_eq!(focus_index(&list), 2); // the in-progress task
        let done_only = vec![mk(TodoStatus::Done), mk(TodoStatus::Done)];
        assert_eq!(focus_index(&done_only), 1); // last done = progress frontier
        let all_pending = vec![mk(TodoStatus::Pending)];
        assert_eq!(focus_index(&all_pending), 0);
    }
}
