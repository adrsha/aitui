use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Padding, Paragraph};
use ratatui::Frame;

use crate::app::state::App;
use crate::input::vim::VimMode;
use crate::render::theme::Theme;
use crate::render::wrap::hard_chunks;

/// Wrapped visual line: text, owning logical line, and char offset within it.
struct VisualLine {
    text: String,
    logical_row: usize,
    char_offset: usize,
}

fn wrap_input_lines(lines: &[String], width: usize) -> Vec<VisualLine> {
    let mut out = Vec::new();
    for (ri, line) in lines.iter().enumerate() {
        let mut offset = 0;
        for chunk in hard_chunks(line, width) {
            let chunk_len = chunk.chars().count();
            out.push(VisualLine {
                text: chunk,
                logical_row: ri,
                char_offset: offset,
            });
            offset += chunk_len;
        }
    }
    if out.is_empty() {
        out.push(VisualLine {
            text: String::new(),
            logical_row: 0,
            char_offset: 0,
        });
    }
    out
}

fn visual_cursor(visual: &[VisualLine], logical_row: usize, logical_col: usize) -> (usize, usize) {
    for (vi, vl) in visual.iter().enumerate() {
        if vl.logical_row == logical_row {
            let chunk_end = vl.char_offset + vl.text.chars().count();
            if vl.char_offset <= logical_col && logical_col <= chunk_end {
                return (vi, logical_col - vl.char_offset);
            }
        }
    }
    let vi = visual
        .iter()
        .rposition(|vl| vl.logical_row == logical_row)
        .unwrap_or(0);
    let col = visual[vi].text.chars().count().min(logical_col);
    (vi, col)
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
    let panel = Style::default();
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
    let visual = wrap_input_lines(&app.input.lines, avail_w);
    let total_visual = visual.len();

    let (cursor_vi, cursor_vc) = visual_cursor(&visual, app.input.row, app.input.col);

    let input_h = inner_h.min(total_visual.max(1));
    let start_row = if cursor_vi >= input_h {
        cursor_vi + 1 - input_h
    } else {
        0
    };

    let mut rendered: Vec<Line<'static>> = Vec::with_capacity(inner_h);
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
    let max_h = 10usize.min(app.mention.matches.len());
    let popup_w = area.width.min(50);
    let popup_h = (max_h as u16 + 2).min(area.height.saturating_sub(2)).max(3);

    let x = area.x;
    let y = area.y.saturating_sub(popup_h);

    let popup_area = Rect {
        x,
        y,
        width: popup_w,
        height: popup_h,
    };
    let block = Block::default()
        .title(" @file ")
        .padding(Padding::horizontal(1));
    let inner = block.inner(popup_area);
    f.render_widget(ratatui::widgets::Clear, popup_area);
    f.render_widget(block, popup_area);

    for i in 0..max_h {
        if let Some(path) = app.mention.matches.get(i) {
            let style = if i == app.mention.selected {
                theme.selection()
            } else {
                Style::default().fg(theme.text)
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(path.clone(), style))),
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
