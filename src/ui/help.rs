use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::commands::HELP_ENTRIES;
use crate::app::state::App;
use crate::render::theme::Theme;

fn help_popup(area: Rect) -> Option<Rect> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let margin_x = if area.width >= 80 { 4 } else { 1 }.min(area.width / 2);
    let margin_y = if area.height >= 24 { 1 } else { 0 }.min(area.height / 2);
    let width = area.width.saturating_sub(margin_x * 2).clamp(1, 120);
    let height = area.height.saturating_sub(margin_y * 2).max(1);
    Some(Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    })
}

fn render_main(f: &mut Frame, app: &App, inner: Rect, theme: &Theme) {
    let avail = inner.height as usize;
    let scroll = app.help_scroll;
    let mut lines: Vec<Line> = Vec::new();
    let mut prev_section = "";

    for (i, entry) in HELP_ENTRIES.iter().enumerate() {
        if i < scroll {
            continue;
        }
        if lines.len() >= avail {
            break;
        }

        if entry.section != prev_section {
            if !prev_section.is_empty() {
                lines.push(Line::from(vec![]));
            }
            lines.push(Line::from(vec![Span::styled(
                format!(" {} ", entry.section),
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            )]));
            prev_section = entry.section;
        }

        let is_sel = i == app.help_selected;
        let style = if is_sel {
            theme.selection()
        } else {
            Style::default().fg(theme.text)
        };
        let icon_style = if is_sel {
            theme.selection().fg(theme.accent)
        } else {
            Style::default().fg(theme.accent)
        };
        let sel_mark = if is_sel { " ▸" } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(sel_mark.to_string(), icon_style),
            Span::styled(format!(" {} ", entry.icon), icon_style),
            Span::styled(
                if entry.key.is_empty() {
                    format!("  {}", entry.summary)
                } else {
                    format!(" {:<12}  {}", entry.key, entry.summary)
                },
                if is_sel {
                    style.add_modifier(Modifier::BOLD)
                } else {
                    style
                },
            ),
        ]));
    }

    // Footer hint
    lines.push(Line::from(vec![]));
    lines.push(Line::from(vec![Span::styled(
        " ↑↓ — navigate  ·  →/Enter — details  ·  ←/q/Esc — close  ·  PgUp/PgDn — scroll",
        Style::default().fg(theme.muted),
    )]));

    f.render_widget(Paragraph::new(lines), inner);
}

fn render_detail(
    f: &mut Frame,
    entry: &crate::app::commands::HelpEntry,
    inner: Rect,
    theme: &Theme,
    _app: &App,
) {
    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled(
                format!(" {} ", entry.icon),
                Style::default().fg(theme.accent),
            ),
            Span::styled(
                entry.key,
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![Span::styled(
            entry.summary,
            Style::default().fg(theme.text),
        )]),
        Line::from(vec![]),
        Line::from(vec![Span::styled(
            format!(" {} — Details", entry.section),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        )]),
    ];

    for line in entry.details {
        if line.is_empty() {
            lines.push(Line::from(vec![]));
        } else {
            lines.push(Line::from(vec![Span::styled(
                format!(" {}", line),
                Style::default().fg(theme.text),
            )]));
        }
    }

    lines.push(Line::from(vec![]));
    lines.push(Line::from(vec![Span::styled(
        " ← / q / Esc — back to overview  ·  ? — close help  ·  ↑↓ / PgUp/PgDn — scroll",
        Style::default().fg(theme.muted),
    )]));

    f.render_widget(Paragraph::new(lines), inner);
}

pub fn render(f: &mut Frame, app: &App, theme: &Theme) {
    let Some(popup) = help_popup(f.area()) else {
        return;
    };

    f.render_widget(Clear, popup);

    let title = match app.help_detail {
        Some(idx) => {
            let name = HELP_ENTRIES
                .get(idx)
                .map(|e| format!("{} {} — Details", e.icon, e.key))
                .unwrap_or_else(|| "Details".to_string());
            format!(" {} ", name)
        }
        None => " Help — Keybindings & Commands ".to_string(),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default())
        .title(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .style(theme.surface());
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    match app.help_detail.and_then(|i| HELP_ENTRIES.get(i)) {
        Some(entry) => render_detail(f, entry, inner, theme, app),
        None => render_main(f, app, inner, theme),
    }
}

#[cfg(test)]
mod tests {
    use super::help_popup;
    use ratatui::layout::Rect;

    #[test]
    fn help_popup_stays_within_small_and_large_frames() {
        assert_eq!(help_popup(Rect::new(0, 0, 0, 10)), None);
        assert_eq!(
            help_popup(Rect::new(3, 4, 20, 8)),
            Some(Rect::new(4, 4, 18, 8))
        );
        assert_eq!(
            help_popup(Rect::new(0, 0, 140, 40)),
            Some(Rect::new(10, 1, 120, 38))
        );
    }
}
