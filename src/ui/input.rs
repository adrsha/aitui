use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Padding, Paragraph};
use ratatui::Frame;

use crate::app::input_layout::{layout as layout_input, VisualLine};
use crate::app::state::App;
use crate::input::vim::VimMode;
use crate::render::theme::Theme;

fn visual_cursor(visual: &[VisualLine], logical_row: usize, logical_col: usize) -> (usize, usize) {
    let mut fallback = None;
    for (index, line) in visual.iter().enumerate() {
        if line.logical_row != logical_row {
            continue;
        }
        fallback = Some(index);
        if logical_col >= line.start && logical_col < line.end {
            return (index, logical_col - line.start);
        }
    }
    let index = fallback.unwrap_or(0);
    let line = &visual[index];
    (index, line.end.saturating_sub(line.start))
}

fn visual_selection_bounds(
    visual: &[VisualLine],
    anchor: (usize, usize),
    cursor: (usize, usize),
) -> ((usize, usize), (usize, usize)) {
    let (va_row, va_col) = visual_cursor(visual, anchor.0, anchor.1);
    let (vc_row, vc_col) = visual_cursor(visual, cursor.0, cursor.1);
    if (va_row, va_col) <= (vc_row, vc_col) {
        ((va_row, va_col), (vc_row, vc_col))
    } else {
        ((vc_row, vc_col), (va_row, va_col))
    }
}

pub fn render(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let panel = theme.surface();
    // Breathing room inside the input panel: 2 cols each side, 1 row top/bottom.
    // The layout allots `input_height + 2` rows, so the vertical padding consumes
    // that slack and the text area stays `input_height` tall.
    let block = Block::default()
        .padding(Padding::new(2, 2, 1, 1))
        .style(panel);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let inner_h = inner.height as usize;
    if inner_h == 0 {
        return;
    }

    // ── Multi-line input with wrapping ───────────────────────────────────
    let avail_w = inner.width.saturating_sub(1).max(1) as usize;
    let visual = layout_input(&app.input.lines, avail_w);
    let total_visual = visual.len();

    let (cursor_vi, cursor_vc) = visual_cursor(&visual, app.input.row, app.input.col);

    let input_h = inner_h.min(total_visual.max(1));
    let start_row = if cursor_vi >= input_h {
        cursor_vi + 1 - input_h
    } else {
        0
    };

    let mut rendered: Vec<Line<'static>> = Vec::with_capacity(inner_h);
    let ghost = ghost_suggestion(app);
    for vi in start_row..start_row + input_h {
        if vi >= total_visual {
            rendered.push(Line::from(""));
            continue;
        }
        let vl = &visual[vi];
        let line_text = &vl.text;
        if app.vim == VimMode::Visual && app.input.visual_anchor.is_some() {
            rendered.push(Line::from(render_visual_wrapped(
                app, &visual, vi, vl, theme,
            )));
        } else if vi == cursor_vi && ghost.is_some() {
            rendered.push(Line::from(render_ghost_line(
                ghost.unwrap_or_default(),
                theme,
            )));
        } else if vi == cursor_vi {
            rendered.push(Line::from(render_input_line(line_text, cursor_vc, theme)));
        } else {
            rendered.push(Line::from(Span::styled(
                line_text.clone(),
                Style::default().fg(theme.text),
            )));
        }
    }
    for _ in rendered.len()..inner_h {
        rendered.push(Line::from(""));
    }

    f.render_widget(Paragraph::new(rendered).style(panel), inner);

    // ── @mention popup ──────────────────────────────────────────────────
    if app.mention.active && !app.mention.matches.is_empty() {
        render_mention_popup(f, app, inner, theme);
    }
}

fn ghost_suggestion(app: &App) -> Option<&str> {
    app.input
        .text()
        .is_empty()
        .then(|| app.sessions.active().response_suggestions.first())
        .flatten()
        .map(String::as_str)
}

fn render_ghost_line(suggestion: &str, theme: &Theme) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            "Tab ↹  ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            suggestion.to_string(),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        ),
    ]
}

/// Render a wrapped visual line with visual selection highlighting.
fn render_visual_wrapped(
    app: &App,
    visual: &[VisualLine],
    vi: usize,
    vl: &VisualLine,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let base = Style::default().fg(theme.text);
    let a = app.input.visual_anchor.unwrap_or((0, 0));
    let b = (app.input.row, app.input.col);
    let ((s_row, s_col), (e_row, e_col)) = visual_selection_bounds(visual, a, b);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let chars: Vec<char> = vl.text.chars().collect();
    for (col, ch) in chars.iter().enumerate() {
        let vpos = (vi, col);
        let selected = vpos >= (s_row, s_col) && vpos <= (e_row, e_col);
        let style = if selected { theme.selection() } else { base };
        spans.push(Span::styled(ch.to_string(), style));
    }
    if spans.is_empty() {
        spans.push(Span::styled(" ", base));
    }
    spans
}

fn render_input_line(line: &str, cursor_col: usize, theme: &Theme) -> Vec<Span<'static>> {
    let base = Style::default().fg(theme.text);
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();

    if cursor_col >= len {
        let mut out = vec![Span::styled(line.to_string(), base)];
        out.push(Span::styled(" ", theme.cursor()));
        return out;
    }

    let before: String = chars[..cursor_col].iter().collect();
    let cur: String = chars[cursor_col..cursor_col + 1].iter().collect();
    let after: String = chars[cursor_col + 1..].iter().collect();

    let mut out = Vec::new();
    if !before.is_empty() {
        out.push(Span::styled(before, base));
    }
    out.push(Span::styled(cur, theme.cursor()));
    if !after.is_empty() {
        out.push(Span::styled(after, base));
    }
    out
}

fn render_mention_popup(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let available_h = area.y.saturating_sub(f.area().y) as usize;
    let max_h = 10usize
        .min(app.mention.matches.len())
        .min(available_h.saturating_sub(1));
    if max_h == 0 {
        return;
    }
    let popup_w = area.width.min(50);
    let popup_h = max_h as u16 + 2;

    let x = area.x;
    let y = area.y.saturating_sub(popup_h);

    let popup_area = Rect {
        x,
        y,
        width: popup_w,
        height: popup_h,
    };
    let padding = if popup_w >= 24 {
        Padding::new(2, 1, 1, 1)
    } else {
        Padding::new(1, 0, 0, 0)
    };
    let block = Block::default()
        .title(Span::styled(
            " @file ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .padding(padding)
        .style(Style::default().bg(Color::Black).fg(theme.text));
    let inner = block.inner(popup_area);
    f.render_widget(ratatui::widgets::Clear, popup_area);
    f.render_widget(block, popup_area);
    f.render_widget(
        Paragraph::new(
            (0..popup_area.height)
                .map(|_| Line::from(Span::styled("█", Style::default().fg(theme.accent))))
                .collect::<Vec<_>>(),
        ),
        Rect {
            width: 1,
            ..popup_area
        },
    );

    for i in 0..max_h {
        if let Some(path) = app.mention.matches.get(i) {
            let style = if i == app.mention.selected {
                theme.selection().add_modifier(Modifier::BOLD)
            } else {
                Style::default().bg(Color::Black).fg(theme.text)
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(format!("  {}", path), style)))
                    .style(Style::default().bg(Color::Black)),
                Rect {
                    x: inner.x,
                    y: inner.y + i as u16,
                    width: inner.width,
                    height: 1,
                },
            );
        }
    }
}
