use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, Padding, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::app::commands::COMMAND_DOCS;
use crate::app::overlay::{
    ApiSetup, BrowsePurpose, DecisionRequest, FileBrowser, Overlay, Picker, PickerKind,
    PlanRequest, Settings, SettingsRow, ToolRequest,
};
use crate::app::state::App;
use crate::render::theme::Theme;
use crate::render::wrap::wrap_words;

pub fn height(overlay: &Overlay, width: u16, available: u16) -> Option<u16> {
    let preferred = match overlay {
        Overlay::None => return None,
        Overlay::Picker(_) | Overlay::Browser(_) => 18,
        Overlay::Palette(_) => 14,
        Overlay::Settings(_) => 14,
        Overlay::Permission(request) => permission_height(request, width),
        Overlay::Decision(_) => 20,
        Overlay::PromptDuringRun(_) => 14,
        Overlay::Plan(_) | Overlay::ToolRequest(_) => 12,
        Overlay::ApiSetup(_) => 11,
        Overlay::SubtaskDetail { .. } => available.min(24),
        Overlay::Notice { .. } => 11,
        Overlay::CommandLine(cl) => {
            if cl.has_completions() {
                // Input (1) + gap (1) + popup (items capped at 10 + borders = +2)
                (4 + (cl.filtered.len() as u16).min(10))
                    .min(available)
                    .min(14)
            } else {
                4
            }
        }
    };
    Some(preferred.min(available).max(1))
}

fn permission_height(request: &crate::app::overlay::PermissionRequest, width: u16) -> u16 {
    let inner_width = width.saturating_sub(5).max(1);
    let theme = Theme::default();
    let body_lines = command_lines(
        &request.calls,
        &theme,
        std::path::Path::new("."),
        inner_width as usize,
        request.horizontal_scroll,
        Some(request.operation_index),
        &request.operation_decisions,
    )
    .len()
    .max(3) as u16;
    let statement_height = access_statement_height(request, inner_width);
    let child_overhead = if request.child_agent_label.is_some() {
        2
    } else {
        0
    };
    let body_overhead = if request.deny.is_some() {
        statement_height.saturating_add(8 + child_overhead)
    } else {
        statement_height.saturating_add(6 + child_overhead)
    };
    body_lines.saturating_add(body_overhead)
}

pub fn render(f: &mut Frame, app: &App, area: Rect, theme: &Theme) -> Option<usize> {
    if area.width < 4 || area.height < 4 {
        f.render_widget(Clear, area);
        return None;
    }

    match &app.overlay {
        Overlay::None => None,
        Overlay::Picker(p) => {
            render_picker(f, app, p, area, theme);
            None
        }
        Overlay::Browser(b) => {
            render_browser(f, b, area, theme);
            None
        }
        Overlay::Palette(p) => {
            render_palette(f, p, area, theme);
            None
        }
        Overlay::Settings(s) => {
            render_settings(f, app, s, area, theme);
            None
        }
        Overlay::Permission(p) => {
            render_permission(f, p, area, theme);
            None
        }
        Overlay::Decision(r) => {
            render_decision(f, r, area, theme);
            None
        }
        Overlay::PromptDuringRun(r) => {
            render_prompt_during_run(f, r, area, theme);
            None
        }
        Overlay::Plan(r) => {
            render_plan(f, r, area, theme);
            None
        }
        Overlay::ToolRequest(r) => {
            render_tool_request(f, r, area, theme);
            None
        }
        Overlay::ApiSetup(s) => {
            render_api_setup(f, s, area, theme);
            None
        }
        Overlay::SubtaskDetail { task_id, scroll } => {
            render_subtask_detail(f, app, *task_id, *scroll, area, theme)
        }
        Overlay::Notice { title, body } => {
            render_notice(f, title, body, area, theme);
            None
        }
        Overlay::CommandLine(cl) => {
            render_command_line(f, cl, area, theme);
            None
        }
    }
}

fn render_subtask_detail(
    f: &mut Frame,
    app: &App,
    task_id: u64,
    scroll: usize,
    area: Rect,
    theme: &Theme,
) -> Option<usize> {
    let task = app.subtasks.iter().find(|task| task.id == task_id)?;
    let agent_index = app
        .subtasks
        .iter()
        .filter(|agent| agent.session_id == task.session_id)
        .position(|agent| agent.id == task.id)
        .map(|index| index + 1)
        .unwrap_or(1);
    let state = match task.status {
        crate::app::state::SubtaskStatus::Running => "RUNNING",
        crate::app::state::SubtaskStatus::Completed => "COMPLETED",
        crate::app::state::SubtaskStatus::Unresolved => "UNRESOLVED",
        crate::app::state::SubtaskStatus::Failed => "FAILED",
    };
    let duration = task
        .duration_ms
        .map(|milliseconds| format!("{} ms", milliseconds))
        .unwrap_or_else(|| format!("{} ms so far", task.started_at.elapsed().as_millis()));
    let assignment = task
        .todo_index
        .map(|index| format!("\nMain checklist task: {}", index))
        .unwrap_or_default();
    let agent_line = task
        .agent
        .as_deref()
        .map(|name| format!("Agent: {}\n", name))
        .unwrap_or_default();
    let mut text = format!(
        "{}Status: {}\nDuration: {}{}\nWorking directory: {}\n\nPrompt:\n{}",
        agent_line,
        state,
        duration,
        assignment,
        task.cwd.display(),
        task.prompt
    );
    if !task.log.is_empty() {
        text.push_str("\n\nActivity:\n");
        for entry in &task.log {
            if let Ok(line) = serde_json::to_string(entry) {
                text.push_str(&line);
                text.push('\n');
            }
        }
    }
    if let Some(output) = task.output.as_deref() {
        text.push_str("\n\nReport:\n");
        text.push_str(output);
    }
    text.push_str("\n\n↑/↓ or j/k scroll · ←/→ or h/l switch agent · Esc close");

    let inner = panel(
        f,
        area,
        &format!(" Child Agent {}: {} ", agent_index, task.description),
        theme,
    );
    let lines: Vec<Line> = text
        .split('\n')
        .flat_map(|line| {
            let indent = if line.is_empty() { "" } else { "  " };
            let avail = inner.width.max(1) as usize;
            let wrapped = wrap_words(line, avail.saturating_sub(2));
            if wrapped.is_empty() {
                vec![Line::from("")]
            } else {
                wrapped
                    .into_iter()
                    .map(|wline| {
                        Line::from(Span::styled(
                            format!("{}{}", indent, wline),
                            Style::default().fg(theme.text),
                        ))
                    })
                    .collect()
            }
        })
        .collect();
    let max_start = lines.len().saturating_sub(inner.height as usize);
    let start = scroll.min(max_start);
    let end = (start + inner.height as usize).min(lines.len());
    f.render_widget(Paragraph::new(lines[start..end].to_vec()), inner);
    Some(max_start)
}

/// A compact informational dock: title + wrapped body + a dismiss hint.
fn render_notice(f: &mut Frame, title: &str, body: &str, area: Rect, theme: &Theme) {
    let inner = panel(f, area, title, theme);
    let mut lines: Vec<Line> = body
        .split('\n')
        .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(theme.text))))
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Press any key to dismiss",
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::ITALIC),
    )));
    f.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: true }),
        inner,
    );
}

// ── File browser (vim navigation + multi-select) ──────────────────────────────

fn render_browser(f: &mut Frame, b: &FileBrowser, area: Rect, theme: &Theme) {
    let title = match b.purpose {
        BrowsePurpose::Attach => " Attach File ",
        BrowsePurpose::Edit => " Open in $EDITOR ",
    };
    let inner = panel(f, area, title, theme);

    // Current directory header.
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" dir {}", b.dir.display()),
            Style::default().fg(theme.muted),
        ))),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
    );

    let list_h = inner.height.saturating_sub(2) as usize;
    // Keep the cursor visible.
    let start = if b.cursor >= list_h {
        b.cursor + 1 - list_h
    } else {
        0
    };
    let end = (start + list_h).min(b.entries.len());

    let items: Vec<ListItem> = b.entries[start..end]
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let idx = start + i;
            let sel = b.is_selected(&e.path);
            let mark = if sel { "✓ " } else { "  " };
            let glyph = if e.is_dir { "dir " } else { "file " };
            let style = if idx == b.cursor {
                theme.selection()
            } else if sel {
                Style::default().fg(theme.success)
            } else {
                Style::default().fg(theme.text)
            };
            ListItem::new(Line::from(Span::styled(
                format!("{}{}{}", mark, glyph, e.name),
                style,
            )))
        })
        .collect();
    f.render_widget(
        List::new(items),
        Rect {
            x: inner.x,
            y: inner.y + 1,
            width: inner.width,
            height: list_h as u16,
        },
    );

    // Footer hint.
    let hint = if b.purpose == BrowsePurpose::Edit {
        let n = b.selected.len();
        if n > 0 {
            format!(" {} selected · ⏎ open all · space toggle · h up", n)
        } else {
            " l/⏎ open · space select · h up · Esc close".to_string()
        }
    } else {
        " l/⏎ attach · h up · Esc close".to_string()
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(theme.accent),
        ))),
        Rect {
            x: inner.x,
            y: inner.y + inner.height - 1,
            width: inner.width,
            height: 1,
        },
    );
}

// ── Picker (models / sessions) ────────────────────────────────────────────────

fn render_picker(f: &mut Frame, _app: &App, picker: &Picker, area: Rect, theme: &Theme) {
    let title = match picker.kind {
        PickerKind::Model => " Model Picker ",
        PickerKind::Session => " Sessions ",
        PickerKind::Skill => " Skills ",
        PickerKind::Access => " Access ",
    };
    let inner = panel(f, area, title, theme);

    // Search bar
    let search = if picker.kind == PickerKind::Access {
        format!(" filter {}", picker.query)
    } else {
        format!(" search {}", picker.query)
    };
    let search_style = if picker.kind == PickerKind::Access {
        Style::default().bg(theme.subtle_pill).fg(theme.text)
    } else {
        Style::default().fg(theme.text)
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(search, search_style))),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
    );

    // Items
    let list_area = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width,
        height: inner.height.saturating_sub(4),
    };
    let match_hl = Style::default()
        .fg(theme.warning)
        .add_modifier(Modifier::BOLD);
    let row_height = if picker.kind == PickerKind::Session {
        2
    } else {
        1
    };
    let capacity = (list_area.height as usize / row_height).max(1);
    let (start, end) = picker_window(picker.filtered.len(), picker.selected, capacity);
    let items: Vec<ListItem> = picker.filtered[start..end]
        .iter()
        .enumerate()
        .map(|(visible_i, &idx)| {
            let i = start + visible_i;
            let item = &picker.items[idx];
            let style = if i == picker.selected {
                theme.selection()
            } else {
                Style::default().fg(theme.text)
            };
            match picker.kind {
                PickerKind::Session => ListItem::new(session_picker_lines(
                    item,
                    list_area.width as usize,
                    i == picker.selected,
                    &picker.query,
                    match_hl,
                    theme,
                )),
                PickerKind::Access => ListItem::new(access_picker_line(
                    item,
                    i == picker.selected,
                    &picker.query,
                    match_hl,
                    theme,
                )),
                _ => {
                    let mut spans = vec![Span::raw(" ")];
                    spans.extend(highlight_query_match(item, &picker.query, style, match_hl));
                    ListItem::new(Line::from(spans))
                }
            }
        })
        .collect();
    f.render_widget(List::new(items), list_area);

    if picker.kind == PickerKind::Session && picker.filtered.len() > capacity {
        let indicator = format!(
            " {}–{} of {}{}{} ",
            start + 1,
            end,
            picker.filtered.len(),
            if start > 0 { "  ↑ more" } else { "" },
            if end < picker.filtered.len() {
                "  ↓ more"
            } else {
                ""
            }
        );
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                indicator,
                Style::default().fg(theme.muted),
            ))),
            Rect::new(
                list_area.x,
                list_area.y + list_area.height,
                list_area.width,
                1,
            ),
        );
    }

    let hint = match picker.kind {
        PickerKind::Session => {
            " ↑↓/j k move · PgUp/PgDn jump · ⏎ open · n new · d delete · r rename · Esc close "
        }
        PickerKind::Model | PickerKind::Skill => " ↑↓ move · ⏎ select · type search · Esc close ",
        PickerKind::Access => {
            " j/k move · Enter disable review/edit rule · d remove rule · type to filter · Esc close "
        }
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(theme.accent),
        ))),
        Rect {
            x: inner.x,
            y: inner.y + inner.height - 1,
            width: inner.width,
            height: 1,
        },
    );
}

fn highlight_query_match(
    text: &str,
    query: &str,
    base: Style,
    match_hl: Style,
) -> Vec<Span<'static>> {
    if query.is_empty() {
        return vec![Span::styled(text.to_string(), base)];
    }
    let lower = query.to_lowercase();
    let text_lower = text.to_lowercase();
    let mut spans = Vec::new();
    let mut start = 0;
    while let Some(pos) = text_lower[start..].find(&lower) {
        let abs_pos = start + pos;
        if abs_pos > start {
            spans.push(Span::styled(text[start..abs_pos].to_string(), base));
        }
        let end = abs_pos + query.len();
        spans.push(Span::styled(text[abs_pos..end].to_string(), match_hl));
        start = end;
    }
    if start < text.len() {
        spans.push(Span::styled(text[start..].to_string(), base));
    }
    if spans.is_empty() {
        spans.push(Span::styled(text.to_string(), base));
    }
    spans
}

fn access_picker_line(
    item: &str,
    selected: bool,
    query: &str,
    match_hl: Style,
    theme: &Theme,
) -> Line<'static> {
    let (kind, rest) = item.split_once("  ").unwrap_or((item, ""));
    let bg = match kind {
        "ALLOW" => Color::Green,
        "DENY" => Color::Red,
        "POLICY" => Color::Cyan,
        _ => Color::DarkGray,
    };
    let mut spans = vec![Span::styled(
        format!(" {} ", kind),
        Style::default()
            .bg(bg)
            .fg(if matches!(bg, Color::Red | Color::DarkGray) {
                Color::White
            } else {
                Color::Black
            })
            .add_modifier(Modifier::BOLD),
    )];
    let base = if selected {
        theme.selection()
    } else {
        Style::default().bg(theme.subtle_pill).fg(theme.text)
    };
    spans.push(Span::styled(" ", base));
    spans.extend(highlight_query_match(rest, query, base, match_hl));
    Line::from(spans)
}

fn picker_window(total: usize, selected: usize, capacity: usize) -> (usize, usize) {
    if total == 0 {
        return (0, 0);
    }
    let capacity = capacity.max(1).min(total);
    let selected = selected.min(total - 1);
    let start = selected
        .saturating_sub(capacity / 2)
        .min(total.saturating_sub(capacity));
    (start, start + capacity)
}

fn session_picker_lines(
    item: &str,
    width: usize,
    selected: bool,
    query: &str,
    match_hl: Style,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let selected_bg = if selected {
        theme.selection().bg.unwrap_or(theme.subtle_pill)
    } else {
        Color::Reset
    };
    let base = Style::default().bg(selected_bg).fg(theme.text);
    if item.starts_with('＋') {
        let (title, detail) = item.split_once("  ·  ").unwrap_or((item, ""));
        let mut first = vec![Span::styled(
            if selected { " ▸ " } else { "   " },
            base.fg(if selected { theme.accent } else { theme.muted }),
        )];
        first.extend(highlight_query_match(
            title.trim(),
            query,
            base.add_modifier(Modifier::BOLD),
            match_hl.bg(selected_bg),
        ));
        return vec![
            padded_line(first, width, selected_bg),
            padded_line(
                vec![Span::styled(
                    format!("     {}", detail),
                    base.fg(theme.muted),
                )],
                width,
                selected_bg,
            ),
        ];
    }

    let parts: Vec<&str> = item.split("  ·  ").collect();
    let heading = parts.first().copied().unwrap_or(item);
    let (marker, name) = heading.split_once("  ").unwrap_or(("○", heading));
    let state = parts.get(1).copied().unwrap_or("idle");
    let metadata = parts
        .iter()
        .skip(2)
        .copied()
        .collect::<Vec<_>>()
        .join("  ·  ");
    let state_style = if state.starts_with("RUNNING") {
        base.fg(theme.warning).add_modifier(Modifier::BOLD)
    } else {
        base.fg(theme.muted)
    };
    let mut first = vec![Span::styled(
        if selected { " ▸ " } else { "   " },
        base.fg(if selected { theme.accent } else { theme.muted }),
    )];
    first.push(Span::styled(
        format!("{} ", marker),
        base.fg(if marker == "●" {
            theme.success
        } else {
            theme.muted
        }),
    ));
    first.extend(highlight_query_match(
        name,
        query,
        base.add_modifier(Modifier::BOLD),
        match_hl.bg(selected_bg),
    ));
    first.push(Span::styled("   ", base));
    first.push(Span::styled(state.to_string(), state_style));
    vec![
        padded_line(first, width, selected_bg),
        padded_line(
            vec![Span::styled(
                format!("     {}", metadata),
                base.fg(theme.muted),
            )],
            width,
            selected_bg,
        ),
    ]
}

fn padded_line(mut spans: Vec<Span<'static>>, width: usize, bg: Color) -> Line<'static> {
    let used = spans
        .iter()
        .map(|span| span.content.as_ref().width())
        .sum::<usize>();
    if used < width {
        spans.push(Span::styled(
            " ".repeat(width - used),
            Style::default().bg(bg),
        ));
    }
    Line::from(spans)
}

// ── Command line (vim `:`) ────────────────────────────────────────────────────

/// Reusable completion popup. Can be used by command line, @mentions, etc.
fn render_completion_popup(
    f: &mut Frame,
    items: &[(&'static str, &'static str)], // (icon, label)
    selected: usize,
    area: Rect,
    theme: &Theme,
) {
    if items.is_empty() || area.width < 8 || area.height < 2 {
        return;
    }
    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let max_visible = inner.height as usize;
    let scroll = if selected >= max_visible {
        selected.saturating_add(1).saturating_sub(max_visible)
    } else {
        0
    };
    let visible_end = (scroll + max_visible).min(items.len());

    for (offset, &(icon, label)) in items[scroll..visible_end].iter().enumerate() {
        let i = scroll + offset;
        let y = inner.y + offset as u16;
        let is_sel = i == selected;
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
        let sel_mark = if is_sel { "▸" } else { " " };
        let line = Line::from(vec![
            Span::styled(format!("{} ", sel_mark), icon_style),
            Span::styled(format!("{} ", icon), icon_style),
            Span::styled(
                label,
                style.add_modifier(if is_sel {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
            ),
        ]);
        f.render_widget(
            Paragraph::new(line),
            Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 1,
            },
        );
    }

    render_scrollbar(f, scroll, visible_end, items.len(), inner, theme);
}

fn render_scrollbar(
    f: &mut Frame,
    start: usize,
    visible_end: usize,
    total: usize,
    area: Rect,
    theme: &Theme,
) {
    if total <= visible_end.saturating_sub(start) {
        return;
    }
    let bar_y = area.y + ((start as f64 / total.max(1) as f64) * area.height as f64) as u16;
    let bar_h = ((visible_end.saturating_sub(start) as f64 / total.max(1) as f64)
        * area.height as f64)
        .max(1.0) as u16;
    for dy in 0..area.height {
        let y = area.y + dy;
        if y >= area.y + area.height {
            break;
        }
        let ch = if dy >= bar_y - area.y && dy < bar_y - area.y + bar_h {
            "▐"
        } else {
            "│"
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                ch,
                Style::default().fg(theme.muted),
            ))),
            Rect {
                x: area.x + area.width - 1,
                y,
                width: 1,
                height: 1,
            },
        );
    }
}

fn render_command_line(
    f: &mut Frame,
    cl: &crate::app::overlay::CommandLine,
    area: Rect,
    theme: &Theme,
) {
    let inner = panel(f, area, " : ", theme);

    let search = format!(" :{}", cl.input);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            search,
            Style::default().fg(theme.text),
        ))),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
    );

    if cl.has_completions() {
        let popup_y = inner.y + 2;
        let max_h = (cl.filtered.len() as u16).min(10).min(
            area.height
                .saturating_sub(popup_y.saturating_sub(area.y))
                .saturating_sub(1),
        );
        if max_h >= 2 {
            let popup_area = Rect {
                x: inner.x,
                y: popup_y,
                width: inner.width.min(50),
                height: max_h,
            };
            let items: Vec<(&'static str, &'static str)> = cl
                .filtered
                .iter()
                .map(|&idx| {
                    let doc = &COMMAND_DOCS[idx];
                    (doc.icon, doc.name)
                })
                .collect();
            render_completion_popup(f, &items, cl.selected, popup_area, theme);
        }
    }
}

// ── Command palette ───────────────────────────────────────────────────────────

fn render_palette(
    f: &mut Frame,
    palette: &crate::app::overlay::Palette,
    area: Rect,
    theme: &Theme,
) {
    let inner = panel(f, area, " Command Palette ", theme);

    let search = format!(" / {}", palette.query);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            search,
            Style::default().fg(theme.text),
        ))),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
    );

    let list_area = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width,
        height: inner.height.saturating_sub(3),
    };
    let visible = list_area.height as usize;
    if visible == 0 {
        return;
    }
    let start = palette.selected.saturating_sub(visible.saturating_sub(1));
    let end = (start + visible).min(palette.filtered.len());
    let cmds = crate::app::overlay::slash_commands();
    let items: Vec<ListItem> = palette
        .filtered
        .iter()
        .enumerate()
        .skip(start)
        .take(end.saturating_sub(start))
        .map(|(i, &idx)| {
            let cmd = &cmds[idx];
            let style = if i == palette.selected {
                theme.selection()
            } else {
                Style::default().fg(theme.text)
            };
            let text = format!(" {}  {}  — {}", cmd.icon, cmd.name, cmd.desc);
            ListItem::new(Line::from(Span::styled(text, style)))
        })
        .collect();
    f.render_widget(List::new(items), list_area);
}

// ── Settings ──────────────────────────────────────────────────────────────────

fn render_settings(f: &mut Frame, app: &App, settings: &Settings, area: Rect, theme: &Theme) {
    let inner = panel(f, area, " Settings ", theme);

    let rows = SettingsRow::all();
    for (i, row) in rows.iter().enumerate() {
        let y = inner.y + i as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let selected = i == settings.selected;
        let label = match row {
            SettingsRow::AutoApprove => format!(
                "  Auto-approve reads: {}",
                if app.config.ui.auto_approve_reads {
                    "ON"
                } else {
                    "OFF"
                }
            ),
            SettingsRow::AccessReview => format!(
                "  Permission review: {}",
                app.config.api.access_review_mode.label()
            ),
            SettingsRow::InputHeight => format!("  Input height: {}", app.config.ui.input_height),
            SettingsRow::ReasoningEffort => format!(
                "  Reasoning › Effort: {}",
                if settings.editing && selected {
                    settings.edit_buf.as_str()
                } else {
                    app.reasoning_effort.as_deref().unwrap_or("off")
                }
            ),
            SettingsRow::ReasoningMode => format!(
                "  Reasoning › Mode: {}",
                if settings.editing && selected {
                    settings.edit_buf.as_str()
                } else {
                    app.reasoning_mode.as_deref().unwrap_or("off")
                }
            ),
            SettingsRow::SystemPrompt => {
                if settings.editing && selected {
                    format!("  System prompt: {}", settings.edit_buf)
                } else {
                    "  System prompt: edit".to_string()
                }
            }
        };
        let style = if selected {
            theme.selection()
        } else {
            Style::default().fg(theme.text)
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(label, style))),
            Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 1,
            },
        );
    }
}

// ── Permission prompt ─────────────────────────────────────────────────────────

fn render_permission(
    f: &mut Frame,
    req: &crate::app::overlay::PermissionRequest,
    area: Rect,
    theme: &Theme,
) {
    let title = if req.editing_access.is_some() {
        " Edit Access "
    } else if req.child_agent_label.is_some() {
        " Subagent Access Request "
    } else {
        " Access Request "
    };
    let inner = panel(f, area, title, theme);
    let footer_h = if req.deny.is_some() { 4 } else { 2 }.min(inner.height);
    let body_bottom = inner.y + inner.height.saturating_sub(footer_h);
    let statement_y = if let Some(label) = req.child_agent_label.as_deref() {
        render_child_access_banner(f, label, inner, theme);
        inner.y.saturating_add(2)
    } else {
        inner.y
    };
    let statement_h =
        access_statement_height(req, inner.width).min(body_bottom.saturating_sub(statement_y));
    render_access_statement(
        f,
        req,
        Rect {
            x: inner.x,
            y: statement_y,
            width: inner.width,
            height: statement_h,
        },
        theme,
    );
    let list_y = (statement_y + statement_h.saturating_add(1)).min(body_bottom);
    let list_h = body_bottom.saturating_sub(list_y);
    let all_lines = command_lines(
        &req.calls,
        theme,
        &req.cwd,
        inner.width as usize,
        req.horizontal_scroll,
        req.has_multiple_operations().then_some(req.operation_index),
        &req.operation_decisions,
    );
    let total = all_lines.len();
    let max_start = total.saturating_sub(list_h as usize);
    let start = req.scroll.min(max_start);
    let end = (start + list_h as usize).min(total);
    let count = req.calls.len();
    let title = if total > list_h as usize {
        format!(
            "{} {} pending call{} · lines {}–{} of {}",
            "tool",
            count,
            if count == 1 { "" } else { "s" },
            start + 1,
            end,
            total
        )
    } else {
        format!(
            "tool {} pending call{}",
            count,
            if count == 1 { "" } else { "s" }
        )
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(title, theme.subtle()))),
        Rect {
            x: inner.x,
            y: list_y - 1,
            width: inner.width,
            height: 1,
        },
    );
    f.render_widget(
        Paragraph::new(all_lines[start..end].to_vec()),
        Rect {
            x: inner.x,
            y: list_y,
            width: inner.width,
            height: list_h,
        },
    );
    if list_h > 0 && start > 0 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("▲ PgUp", theme.subtle()))),
            Rect {
                x: inner.x + inner.width.saturating_sub(7),
                y: list_y,
                width: 7,
                height: 1,
            },
        );
    }
    if list_h > 0 && end < total {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("▼ PgDn", theme.subtle()))),
            Rect {
                x: inner.x + inner.width.saturating_sub(7),
                y: list_y + list_h.saturating_sub(1),
                width: 7,
                height: 1,
            },
        );
    }

    if let Some(draft) = req.deny.as_ref() {
        render_deny_reason(f, draft, inner, list_y, list_h, theme);
        return;
    }

    let review_style = if req.selected == 5 {
        theme.selection()
    } else {
        Style::default()
            .fg(theme.warning)
            .add_modifier(Modifier::BOLD)
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "  ◇ Use automated review model",
            review_style,
        ))),
        Rect {
            x: inner.x,
            y: inner.y + inner.height.saturating_sub(2),
            width: inner.width,
            height: 1,
        },
    );
    let operation_hint = if req.has_multiple_operations() {
        " · [/] or n/N operation · a allow current · A allow all · d deny current"
    } else {
        " · a/A allow once · d deny"
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(
                "←/→ field · Enter choose/review · Space choose{} · e edit commands · PgUp/PgDn calls",
                operation_hint
            ),
            Style::default().fg(theme.accent),
        ))),
        Rect {
            x: inner.x,
            y: inner.y + inner.height.saturating_sub(1),
            width: inner.width,
            height: 1,
        },
    );
    // Popups are intentionally rendered last so the command list and action rows
    // can never paint over them.
    let statement_area = Rect {
        x: inner.x,
        y: statement_y,
        width: inner.width,
        height: statement_h,
    };
    if req.selecting_folder() {
        render_folder_picker_popup(f, req, statement_area, theme);
    } else if req.selecting {
        render_access_selector_popup(f, req, statement_area, theme);
    }
}

fn render_child_access_banner(f: &mut Frame, label: &str, inner: Rect, theme: &Theme) {
    if inner.height == 0 {
        return;
    }
    let text = format!("  ◉ Requested by subagent: {}", label);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default()
                .fg(Color::Magenta)
                .bg(theme.subtle_pill)
                .add_modifier(Modifier::BOLD),
        ))),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
    );
}

fn access_statement_height(req: &crate::app::overlay::PermissionRequest, width: u16) -> u16 {
    access_statement_lines(req, width, &Theme::default())
        .len()
        .max(1) as u16
}

fn access_statement_lines(
    req: &crate::app::overlay::PermissionRequest,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let line_bg = Color::DarkGray;
    let value_bg = Color::Black;
    let value_style = |index: usize| {
        if req.selected == index {
            theme.selection().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .bg(value_bg)
                .fg(theme.text)
                .add_modifier(Modifier::BOLD)
        }
    };
    let text_style = Style::default().bg(line_bg).fg(theme.text);
    let decision = if req.decision == crate::agent::PermissionDecision::Allow {
        "✓ Allow"
    } else {
        "✗ Deny"
    };
    let location_icon = match req.location_index {
        0 => "◎",
        1 => "▶",
        2 | 3 => "▶",
        _ => "▣",
    };
    let lifetime_icon = match req.lifetime_index {
        0 => "◷",
        1 => "◉",
        2 => "⏱",
        3 => "⇄",
        _ => "⊞",
    };
    let tool = req
        .calls
        .first()
        .and_then(crate::agent::ToolCall::kind)
        .filter(|_| req.tool_index > 0)
        .and_then(|_| {
            crate::agent::ToolKind::all()
                .get(req.tool_index - 1)
                .copied()
        })
        .map(|kind| format!("{} {}", kind.icon(), req.tool_label()))
        .unwrap_or_else(|| format!("tool {}", req.tool_label()));
    let scope = if req.location_index == 0 {
        "no directory limit"
    } else if req.location_index == 3 || req.include_children {
        "include subdirectories"
    } else {
        "this directory only"
    };
    let parts = vec![
        Span::styled(format!(" {} ", decision), value_style(0)),
        Span::styled("  ", text_style),
        Span::styled(format!(" {} ", tool), value_style(1)),
        Span::styled(" at ", text_style),
        Span::styled(
            format!(" {} {} ", location_icon, req.location_label()),
            value_style(3),
        ),
        Span::styled(" (", text_style),
        Span::styled(format!(" {} ", scope), value_style(2)),
        Span::styled(") for ", text_style),
        Span::styled(
            format!(" {} {} ", lifetime_icon, req.lifetime_label()),
            value_style(4),
        ),
    ];
    let max_width = width.max(1) as usize;
    let mut lines = Vec::new();
    let mut current = Vec::new();
    let mut used = 0usize;
    for part in parts {
        let part_width = part.content.as_ref().width();
        if !current.is_empty() && used + part_width > max_width {
            lines.push(access_statement_line(current, used, max_width, text_style));
            current = Vec::new();
            used = 0;
        }
        used += part_width;
        current.push(part);
    }
    if !current.is_empty() {
        lines.push(access_statement_line(current, used, max_width, text_style));
    }
    lines
}

fn access_statement_line(
    mut spans: Vec<Span<'static>>,
    used: usize,
    width: usize,
    style: Style,
) -> Line<'static> {
    if used < width {
        spans.push(Span::styled(" ".repeat(width - used), style));
    }
    Line::from(spans)
}

fn render_access_statement(
    f: &mut Frame,
    req: &crate::app::overlay::PermissionRequest,
    area: Rect,
    theme: &Theme,
) {
    f.render_widget(
        Paragraph::new(access_statement_lines(req, area.width, theme))
            .style(Style::default().bg(Color::DarkGray)),
        area,
    );
}

/// The directory browser behind the "custom directory" location: subdirectories
/// to traverse, with the `*` and `**` glob choices for the current directory.
fn render_folder_picker_popup(
    f: &mut Frame,
    req: &crate::app::overlay::PermissionRequest,
    anchor: Rect,
    theme: &Theme,
) {
    let Some(picker) = req.folder_picker.as_ref() else {
        return;
    };
    let frame = f.area();
    let count = picker.option_count();
    let popup_w = anchor.width.min(frame.width).clamp(1, 56);
    let popup_h = ((count as u16).saturating_add(3)).min(frame.height).max(3);
    let x = anchor.x.min(frame.x + frame.width.saturating_sub(popup_w));
    let y = anchor
        .y
        .saturating_sub(popup_h)
        .max(frame.y)
        .min(frame.y + frame.height.saturating_sub(popup_h));
    let popup_area = Rect {
        x,
        y,
        width: popup_w,
        height: popup_h,
    };
    let title = crate::render::path::display_path(&picker.dir);
    let padding = if popup_w >= 30 && popup_h >= 6 {
        Padding::new(2, 1, 1, 1)
    } else {
        Padding::new(1, 0, 0, 0)
    };
    let block = Block::default()
        .title(Span::styled(
            format!(" {}", title),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .padding(padding)
        .style(Style::default().bg(Color::Black).fg(theme.text));
    let inner = block.inner(popup_area);
    f.render_widget(Clear, popup_area);
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

    let visible = inner.height as usize;
    let start = picker.cursor.saturating_sub(visible.saturating_sub(1));
    let end = (start + visible).min(count);
    for (offset, index) in (start..end).enumerate() {
        let (glyph, text, color) = match picker.row(index) {
            Some(crate::app::overlay::FolderRow::Parent) => {
                ("↑", "parent directory".to_string(), theme.muted)
            }
            Some(crate::app::overlay::FolderRow::Directory(entry)) => {
                ("▸", format!("{}/", entry.name), theme.text)
            }
            Some(crate::app::overlay::FolderRow::Glob) => (
                "*",
                format!("everything in {}", picker.dir.display()),
                theme.accent,
            ),
            Some(crate::app::overlay::FolderRow::GlobRecursive) => (
                "**",
                format!("everything in {} and children", picker.dir.display()),
                theme.accent,
            ),
            None => continue,
        };
        let selected = index == picker.cursor;
        let mut style = if selected {
            theme.selection().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .bg(Color::Black)
                .fg(crate::render::theme::fg_guard(color))
        };
        if !selected {
            style = style.add_modifier(
                if matches!(
                    picker.row(index),
                    Some(crate::app::overlay::FolderRow::Glob)
                        | Some(crate::app::overlay::FolderRow::GlobRecursive)
                ) {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                },
            );
        }
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {} {}", glyph, text),
                style,
            )))
            .style(Style::default().bg(Color::Black)),
            Rect {
                x: inner.x,
                y: inner.y + offset as u16,
                width: inner.width,
                height: 1,
            },
        );
    }
    let hint = "↑/↓ move · Enter open folder · Space or Enter choose current scope · Esc cancel";
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(theme.accent),
        )))
        .style(Style::default().bg(Color::Black)),
        Rect {
            x: inner.x,
            y: inner.y + inner.height.saturating_sub(1),
            width: inner.width,
            height: 1,
        },
    );
}

fn render_access_selector_popup(
    f: &mut Frame,
    req: &crate::app::overlay::PermissionRequest,
    anchor: Rect,
    theme: &Theme,
) {
    let options = access_selector_options(req);
    if options.is_empty() {
        return;
    }
    let selected = access_selector_index(req).min(options.len().saturating_sub(1));
    let frame = f.area();
    let popup_w = anchor.width.min(frame.width).clamp(1, 52);
    let popup_h = (options.len() as u16)
        .saturating_add(2)
        .min(frame.height)
        .max(1);
    let x = anchor.x.min(frame.x + frame.width.saturating_sub(popup_w));
    let y = anchor
        .y
        .saturating_sub(popup_h)
        .max(frame.y)
        .min(frame.y + frame.height.saturating_sub(popup_h));
    let popup_area = Rect {
        x,
        y,
        width: popup_w,
        height: popup_h,
    };
    let padding = if popup_w >= 24 && popup_h >= 5 {
        Padding::new(2, 1, 1, 1)
    } else {
        Padding::new(1, 0, 0, 0)
    };
    let block = Block::default()
        .title(Span::styled(
            " choose ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .padding(padding)
        .style(Style::default().bg(Color::Black).fg(theme.text));
    let inner = block.inner(popup_area);
    f.render_widget(Clear, popup_area);
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
    let visible = inner.height as usize;
    let start = selected.saturating_sub(visible.saturating_sub(1));
    let end = (start + visible).min(options.len());
    for (row, option) in options[start..end].iter().enumerate() {
        let index = start + row;
        let style = if index == selected {
            theme.selection().add_modifier(Modifier::BOLD)
        } else {
            Style::default().bg(Color::Black).fg(theme.text)
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(format!("  {}", option), style)))
                .style(Style::default().bg(Color::Black)),
            Rect {
                x: inner.x,
                y: inner.y + row as u16,
                width: inner.width,
                height: 1,
            },
        );
    }
}

fn access_selector_options(req: &crate::app::overlay::PermissionRequest) -> Vec<String> {
    match req.selected {
        0 => vec!["✓ Allow".into(), "✗ Deny".into()],
        1 => std::iter::once("◎ all access types".to_string())
            .chain(
                crate::agent::ToolKind::all()
                    .iter()
                    .map(|kind| format!("▸ {}", kind.name())),
            )
            .collect(),
        2 => vec![
            "○ only this directory".into(),
            "◎ this directory and all subdirectories".into(),
        ],
        3 => vec![
            "◎ anywhere (no directory limit)".into(),
            "▶ directory requested by this tool".into(),
            "▶ current working directory".into(),
            "▶ current working directory and all subdirectories".into(),
            "▣ choose another directory…".into(),
        ],
        4 => vec![
            "◷ this request only".into(),
            "◉ current session".into(),
            "⏱ minutes…".into(),
            "⇄ matching requests".into(),
            "⊞ total requests".into(),
        ],
        _ => Vec::new(),
    }
}

fn access_selector_index(req: &crate::app::overlay::PermissionRequest) -> usize {
    match req.selected {
        0 => usize::from(req.decision == crate::agent::PermissionDecision::Deny),
        1 => req.tool_index,
        2 => usize::from(req.include_children),
        3 => req.location_index,
        4 => req.lifetime_index,
        _ => 0,
    }
}

/// The deny reason prompt: what is being denied, a text field, and the keys. The
/// reason is optional — Enter on an empty field denies with no explanation.
fn render_deny_reason(
    f: &mut Frame,
    draft: &crate::app::overlay::DenyDraft,
    inner: Rect,
    list_y: u16,
    list_h: u16,
    theme: &Theme,
) {
    let scope = match draft.perm {
        crate::agent::Permission::Deny => "Denying these call(s)",
        crate::agent::Permission::DenyKind => "Denying this tool for the session",
        crate::agent::Permission::DenyDirectory => "Denying this directory for the session",
        _ => "Denying everything for the session",
    };
    let start_y = list_y + list_h;
    let rows = [
        Line::from(Span::styled(
            format!("  {} — tell the model why (optional):", scope),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("  > ", Style::default().fg(theme.accent)),
            Span::styled(draft.reason.clone(), Style::default().fg(theme.text)),
            Span::styled("█", Style::default().fg(theme.accent)),
        ]),
        Line::from(Span::styled(
            "  A reason stops it retrying the same call — e.g. \"don't touch the lockfile\".",
            theme.subtle(),
        )),
    ];
    for (i, line) in rows.into_iter().enumerate() {
        let y = start_y + i as u16;
        if y >= inner.y + inner.height.saturating_sub(1) {
            break;
        }
        f.render_widget(
            Paragraph::new(line),
            Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 1,
            },
        );
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "⏎ deny · Esc back to options",
            Style::default().fg(theme.accent),
        ))),
        Rect {
            x: inner.x,
            y: inner.y + inner.height.saturating_sub(1),
            width: inner.width,
            height: 1,
        },
    );
}

/// Build the scrollable access-request body from semantic, tool-aware cards.
/// Each operation shows only the arguments that define what it will do.
fn command_lines(
    calls: &[crate::agent::ToolCall],
    theme: &Theme,
    cwd: &std::path::Path,
    width: usize,
    horizontal_scroll: usize,
    selected_operation: Option<usize>,
    operation_decisions: &[Option<crate::agent::PermissionDecision>],
) -> Vec<Line<'static>> {
    let mut concrete = Vec::new();
    for call in calls {
        match call.expanded_calls() {
            Ok(Some(items)) if !items.is_empty() => concrete.extend(items),
            _ => concrete.push(call.clone()),
        }
    }

    let mut out = Vec::new();
    for (index, call) in concrete.iter().enumerate() {
        let kind = call.kind();
        let icon = kind.map(|kind| kind.icon()).unwrap_or("tool");
        let risk = kind.map(|kind| kind.risk().label()).unwrap_or("UNKNOWN");
        let name = kind.map(|kind| kind.name()).unwrap_or(call.name.as_str());
        let decision = operation_decisions.get(index).copied().flatten();
        let marker = match decision {
            Some(crate::agent::PermissionDecision::Allow) => "✓",
            Some(crate::agent::PermissionDecision::Deny) => "✗",
            None if selected_operation == Some(index) => "▶",
            None => "○",
        };
        let header_style = match decision {
            Some(crate::agent::PermissionDecision::Allow) => Style::default()
                .fg(theme.success)
                .add_modifier(Modifier::BOLD),
            Some(crate::agent::PermissionDecision::Deny) => Style::default()
                .fg(theme.danger)
                .add_modifier(Modifier::BOLD),
            None if selected_operation == Some(index) => theme.selection(),
            None => Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        };
        out.push(Line::from(vec![
            Span::styled(
                format!("{} {} {}. {}", marker, icon, index + 1, name),
                header_style,
            ),
            Span::styled(format!("   {} risk", risk), theme.subtle()),
        ]));
        for group in permission_card_groups(call, theme, cwd) {
            out.extend(permission_card_group(
                &group,
                2,
                width,
                horizontal_scroll,
                theme,
            ));
            out.push(Line::from(String::new()));
        }
    }
    out
}

#[derive(Debug)]
struct PermissionCard<'a> {
    label: &'static str,
    value: &'a str,
    language: &'a str,
    color: Color,
    start_line: usize,
}

fn permission_card_groups<'a>(
    call: &'a crate::agent::ToolCall,
    theme: &Theme,
    cwd: &std::path::Path,
) -> Vec<Vec<PermissionCard<'a>>> {
    use crate::agent::ToolKind;

    let card = |label, key, language, color| {
        call.get_arg(key).map(|value| PermissionCard {
            label,
            value,
            language,
            color,
            start_line: 1,
        })
    };
    let optional_card = |label, key, language, color| {
        call.get_arg(key)
            .filter(|value| !value.is_empty())
            .map(|value| PermissionCard {
                label,
                value,
                language,
                color,
                start_line: 1,
            })
    };
    let path = call.get_arg("path").unwrap_or("");
    let mut groups = Vec::new();
    match call.kind() {
        Some(ToolKind::Edit) => {
            if let Some(path) = card("PATH", "path", "", theme.accent) {
                groups.push(vec![path]);
            }
            let start_line = permission_edit_start_line(call, cwd);
            if let Some(mut old) = card("OLD", "old", path, theme.danger) {
                old.start_line = start_line;
                groups.push(vec![old]);
            }
            if let Some(mut new) = card("NEW", "new", path, theme.success) {
                new.start_line = start_line;
                groups.push(vec![new]);
            }
        }
        Some(ToolKind::Write) => {
            if let Some(path) = card("PATH", "path", "", theme.accent) {
                groups.push(vec![path]);
            }
            if let Some(content) = card("CONTENT", "content", path, theme.success) {
                groups.push(vec![content]);
            }
        }
        Some(ToolKind::Shell) => {
            if let Some(command) = card("COMMAND", "command", "bash", theme.warning) {
                groups.push(vec![command]);
            }
        }
        Some(ToolKind::Move) | Some(ToolKind::Copy) => {
            let locations: Vec<_> = [
                card("FROM", "from", "", theme.danger),
                card("TO", "to", "", theme.success),
            ]
            .into_iter()
            .flatten()
            .collect();
            if !locations.is_empty() {
                groups.push(locations);
            }
        }
        Some(ToolKind::Search) => {
            let search: Vec<_> = [
                card("PATTERN", "pattern", "", theme.warning),
                optional_card("PATH", "path", "", theme.accent),
                optional_card("FILES", "glob", "", theme.success),
            ]
            .into_iter()
            .flatten()
            .collect();
            if !search.is_empty() {
                groups.push(search);
            }
        }
        Some(ToolKind::ReverseImage) => {
            let sources: Vec<_> = [
                optional_card("URL", "url", "", theme.link),
                optional_card("PATH", "path", "", theme.accent),
            ]
            .into_iter()
            .flatten()
            .collect();
            if !sources.is_empty() {
                groups.push(sources);
            }
        }
        Some(ToolKind::Download) => {
            let transfer: Vec<_> = [
                card("URL", "url", "", theme.link),
                card("PATH", "path", "", theme.accent),
            ]
            .into_iter()
            .flatten()
            .collect();
            if !transfer.is_empty() {
                groups.push(transfer);
            }
        }
        Some(ToolKind::PowerPoint) => {
            let details: Vec<_> = [
                card("OPERATION", "operation", "", theme.link),
                card("INPUT", "input_path", "", theme.muted),
                card("OUTPUT", "output_path", "", theme.accent),
            ]
            .into_iter()
            .flatten()
            .collect();
            if !details.is_empty() {
                groups.push(details);
            }
        }
        Some(ToolKind::WebSearch) | Some(ToolKind::WebImages) => {
            if let Some(query) = card("QUERY", "query", "", theme.link) {
                groups.push(vec![query]);
            }
        }
        Some(ToolKind::WebFetch) => {
            if let Some(url) = card("URL", "url", "", theme.link) {
                groups.push(vec![url]);
            }
        }
        Some(ToolKind::Read)
        | Some(ToolKind::List)
        | Some(ToolKind::MakeDir)
        | Some(ToolKind::Delete) => {
            if let Some(path) = card("PATH", "path", "", theme.accent) {
                groups.push(vec![path]);
            }
        }
        _ => {
            let generic: Vec<_> = call
                .editable_arg_keys()
                .iter()
                .filter_map(|key| card(argument_label(key), key, "", theme.accent))
                .collect();
            if !generic.is_empty() {
                groups.push(generic);
            }
        }
    }
    groups
}

fn permission_edit_start_line(call: &crate::agent::ToolCall, cwd: &std::path::Path) -> usize {
    let Some(path) = call.get_arg("path") else {
        return 1;
    };
    let Some(old) = call.get_arg("old").or_else(|| call.get_arg("old_string")) else {
        return 1;
    };
    let path = std::path::Path::new(path);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let Ok(content) = std::fs::read_to_string(resolved) else {
        return 1;
    };
    let Some(byte_index) = content.find(old) else {
        return 1;
    };
    content[..byte_index]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

fn argument_label(key: &str) -> &'static str {
    match key {
        "path" => "PATH",
        "from" => "FROM",
        "to" => "TO",
        "url" => "URL",
        "query" => "QUERY",
        "pattern" => "PATTERN",
        "command" => "COMMAND",
        "content" => "CONTENT",
        "old" => "OLD",
        "new" => "NEW",
        _ => "VALUE",
    }
}

fn permission_card_group(
    cards: &[PermissionCard<'_>],
    indent: usize,
    width: usize,
    horizontal_scroll: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    if cards.is_empty() {
        return Vec::new();
    }
    let indent = indent.min(width.saturating_sub(1));
    let content_width = width.saturating_sub(indent).max(1);
    let gap = 2;
    let horizontal = cards.len() > 1
        && content_width >= cards.len().saturating_mul(22) + gap * (cards.len() - 1);
    let card_width = if horizontal {
        content_width.saturating_sub(gap * (cards.len() - 1)) / cards.len()
    } else {
        content_width
    }
    .max(1);
    let rendered: Vec<_> = cards
        .iter()
        .map(|card| permission_code_card(card, card_width, horizontal_scroll, theme))
        .collect();
    let pad = Span::raw(" ".repeat(indent));

    if horizontal {
        let row_count = rendered.iter().map(Vec::len).max().unwrap_or(0);
        (0..row_count)
            .map(|row| {
                let mut spans = vec![pad.clone()];
                for (index, card) in rendered.iter().enumerate() {
                    if index > 0 {
                        spans.push(Span::raw(" ".repeat(gap)));
                    }
                    spans.extend(
                        card.get(row)
                            .cloned()
                            .unwrap_or_else(|| terminal_fill(card_width)),
                    );
                }
                Line::from(spans)
            })
            .collect()
    } else {
        let mut out = Vec::new();
        for (index, card) in rendered.into_iter().enumerate() {
            if index > 0 {
                out.push(Line::from(String::new()));
            }
            out.extend(card.into_iter().map(|mut spans| {
                spans.insert(0, pad.clone());
                Line::from(spans)
            }));
        }
        out
    }
}

fn permission_code_card(
    card: &PermissionCard<'_>,
    width: usize,
    horizontal_scroll: usize,
    theme: &Theme,
) -> Vec<Vec<Span<'static>>> {
    let display_value = if matches!(card.label, "PATH" | "FROM" | "TO") {
        crate::render::path::abbreviate_home(card.value)
    } else {
        card.value.to_string()
    };
    let source = expand_tabs(&display_value, 4);
    let is_code = !card.language.is_empty();
    let highlighted = if is_code {
        crate::render::highlight::highlight(&source, card.language, theme)
    } else {
        None
    };
    let source_lines: Vec<_> = source.split('\n').collect();
    let last_line_number = card
        .start_line
        .saturating_add(source_lines.len().saturating_sub(1));
    let line_number_width = last_line_number.max(1).to_string().len();
    let numbered_gutter = is_code && width >= line_number_width + 7;
    let gutter_width = if numbered_gutter {
        line_number_width + 5
    } else {
        2
    };
    let viewport_width = width.saturating_sub(gutter_width).max(1);
    let max_line_width = source_lines
        .iter()
        .map(|line| line.width())
        .max()
        .unwrap_or(0);
    let max_scroll = max_line_width.saturating_sub(viewport_width);
    let scroll = horizontal_scroll.min(max_scroll);
    let mut rows = vec![card_row(
        vec![
            Span::styled("█ ", Style::default().fg(card.color).bg(Color::Reset)),
            Span::styled(
                card.label.to_string(),
                Style::default()
                    .fg(card.color)
                    .bg(Color::Reset)
                    .add_modifier(Modifier::BOLD),
            ),
        ],
        width,
    )];

    for (index, src) in source_lines.iter().enumerate() {
        let segments = highlighted
            .as_ref()
            .and_then(|lines| lines.get(index))
            .cloned()
            .unwrap_or_else(|| vec![(src.to_string(), Style::default().fg(theme.text))]);
        let mut spans = if numbered_gutter {
            vec![
                Span::styled("█ ", Style::default().fg(card.color).bg(Color::Reset)),
                Span::styled(
                    format!("{:>line_number_width$}", card.start_line + index),
                    Style::default().fg(theme.muted).bg(Color::Reset),
                ),
                Span::styled(" │ ", Style::default().fg(Color::DarkGray).bg(Color::Reset)),
            ]
        } else {
            vec![Span::styled(
                "█ ",
                Style::default().fg(card.color).bg(Color::Reset),
            )]
        };
        spans.extend(slice_segments(segments, scroll, viewport_width));
        rows.push(card_row(spans, width));
    }

    if is_code && max_scroll > 0 {
        let (thumb_start, thumb_len) =
            horizontal_thumb(max_line_width, viewport_width, scroll, viewport_width);
        let track = (0..viewport_width)
            .map(|index| {
                if index >= thumb_start && index < thumb_start + thumb_len {
                    '━'
                } else {
                    '─'
                }
            })
            .collect::<String>();
        let mut scrollbar = vec![Span::styled(
            "█ ",
            Style::default().fg(card.color).bg(Color::Reset),
        )];
        if numbered_gutter {
            scrollbar.push(Span::styled(
                " ".repeat(line_number_width),
                Style::default().bg(Color::Reset),
            ));
            scrollbar.push(Span::styled(
                " │ ",
                Style::default().fg(Color::DarkGray).bg(Color::Reset),
            ));
        }
        scrollbar.push(Span::styled(
            track,
            Style::default().fg(theme.muted).bg(Color::Reset),
        ));
        rows.push(card_row(scrollbar, width));
    }
    rows
}

fn slice_segments(
    segments: Vec<crate::render::highlight::Segment>,
    start: usize,
    width: usize,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut skipped = 0usize;
    let mut used = 0usize;
    for (text, mut style) in segments {
        let mut visible = String::new();
        for ch in text.chars() {
            let char_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if skipped + char_width <= start {
                skipped += char_width;
                continue;
            }
            if skipped < start {
                skipped += char_width;
                continue;
            }
            if used + char_width > width {
                break;
            }
            visible.push(ch);
            used += char_width;
        }
        if !visible.is_empty() {
            style.bg = Some(Color::Reset);
            spans.push(Span::styled(visible, style));
        }
        if used >= width {
            break;
        }
    }
    spans
}

fn expand_tabs(text: &str, tab_width: usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut column = 0usize;
    for ch in text.chars() {
        match ch {
            '\n' => {
                out.push(ch);
                column = 0;
            }
            '\t' => {
                let spaces = tab_width - column % tab_width;
                out.push_str(&" ".repeat(spaces));
                column += spaces;
            }
            _ => {
                out.push(ch);
                column += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            }
        }
    }
    out
}

fn horizontal_thumb(
    content_width: usize,
    viewport_width: usize,
    scroll: usize,
    track_width: usize,
) -> (usize, usize) {
    if track_width == 0 || content_width <= viewport_width {
        return (0, track_width);
    }
    let thumb_len = ((viewport_width * track_width) / content_width).clamp(1, track_width);
    let max_scroll = content_width - viewport_width;
    let max_start = track_width - thumb_len;
    let thumb_start = scroll.min(max_scroll) * max_start / max_scroll;
    (thumb_start, thumb_len)
}

fn card_row(mut spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let used = spans
        .iter()
        .map(|span| span.content.as_ref().width())
        .sum::<usize>();
    if used > width {
        return slice_segments(
            spans
                .into_iter()
                .map(|span| (span.content.into_owned(), span.style))
                .collect(),
            0,
            width,
        );
    }
    if used < width {
        spans.push(Span::styled(
            " ".repeat(width - used),
            Style::default().bg(Color::Reset),
        ));
    }
    spans
}

fn terminal_fill(width: usize) -> Vec<Span<'static>> {
    vec![Span::styled(
        " ".repeat(width),
        Style::default().bg(Color::Reset),
    )]
}

// ── Decision / plan prompts ──────────────────────────────────────────────────

fn render_decision(f: &mut Frame, req: &DecisionRequest, area: Rect, theme: &Theme) {
    let inner = panel(
        f,
        area,
        if req.free_form() {
            " Question Request "
        } else {
            " Decision Request "
        },
        theme,
    );
    if inner.height < 4 || inner.width == 0 {
        return;
    }

    let question_height = (inner.height / 4).clamp(2, 6);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            req.question.clone(),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        )))
        .wrap(ratatui::widgets::Wrap { trim: true }),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: question_height,
        },
    );

    let body_y = inner.y + question_height;
    let body_height = inner.height.saturating_sub(question_height + 1);
    if req.free_form() {
        let answer = if req.answer.is_empty() {
            "Type answer…".to_string()
        } else {
            req.answer.clone()
        };
        let style = if req.answer.is_empty() {
            theme.subtle()
        } else {
            Style::default().fg(theme.text)
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(answer, style)))
                .block(
                    Block::default()
                        .padding(Padding::uniform(1))
                        .style(theme.surface()),
                )
                .wrap(ratatui::widgets::Wrap { trim: false }),
            Rect {
                x: inner.x,
                y: body_y,
                width: inner.width,
                height: body_height,
            },
        );
    } else {
        let detail_height = (body_height / 3).clamp(3, 7).min(body_height);
        let list_height = body_height.saturating_sub(detail_height);
        render_decision_options(
            f,
            req,
            theme,
            Rect {
                x: inner.x,
                y: body_y,
                width: inner.width,
                height: list_height,
            },
        );
        render_decision_detail(
            f,
            req,
            theme,
            Rect {
                x: inner.x,
                y: body_y + list_height,
                width: inner.width,
                height: detail_height,
            },
        );
    }

    let hint = if req.free_form() {
        " type answer · ⏎ submit · Esc cancel"
    } else if req.custom_editing {
        " type custom response · Tab options · ⏎ submit · Esc cancel"
    } else if req.multi {
        " ↑↓ move · space toggle · e edit · Tab custom · ⏎ confirm · Esc cancel"
    } else {
        " ↑↓ move · e edit · Tab custom · ⏎ choose · Esc cancel"
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(theme.accent),
        ))),
        Rect {
            x: inner.x,
            y: inner.y + inner.height - 1,
            width: inner.width,
            height: 1,
        },
    );
}

fn render_prompt_during_run(
    f: &mut Frame,
    req: &crate::app::overlay::PromptDuringRun,
    area: Rect,
    theme: &Theme,
) {
    let inner = panel(f, area, " Assistant is working ", theme);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let mut lines = vec![
        Line::from(Span::styled(
            "Send this prompt while the current main-agent turn is still running?",
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for (index, option) in crate::app::overlay::PromptDuringRun::OPTIONS
        .iter()
        .enumerate()
    {
        let selected = index == req.selected;
        lines.push(Line::from(Span::styled(
            format!(
                "{} {}. {}",
                if selected { "›" } else { " " },
                index + 1,
                option
            ),
            if selected {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text)
            },
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "↑↓ choose · 1/2/3 direct · Enter confirm · Esc wait",
        Style::default().fg(theme.muted),
    )));
    f.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: true }),
        inner,
    );
}

fn render_decision_options(f: &mut Frame, req: &DecisionRequest, theme: &Theme, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let mut labels = req.options.clone();
    labels.push("Custom response…".to_string());
    let width = area.width.saturating_sub(4).max(1) as usize;
    let rows: Vec<Vec<String>> = labels
        .iter()
        .map(|option| wrap_words(option, width))
        .collect();
    let start = decision_scroll_start(&rows, req.selected, area.height as usize);
    let mut y = area.y;
    for (i, wrapped) in rows.iter().enumerate().skip(start) {
        let row_height = wrapped.len().max(1) as u16;
        if y + row_height > area.y + area.height {
            break;
        }
        let mark = if i == req.options.len() {
            if i == req.selected {
                "›"
            } else {
                " "
            }
        } else if req.multi {
            if req.chosen.contains(&i) {
                "☑"
            } else {
                "☐"
            }
        } else if i == req.selected {
            "›"
        } else {
            " "
        };
        let style = if i == req.selected {
            theme.selection()
        } else {
            Style::default().fg(theme.text)
        };
        let lines: Vec<Line> = wrapped
            .iter()
            .enumerate()
            .map(|(line, text)| {
                let prefix = if line == 0 {
                    format!(" {} ", mark)
                } else {
                    "   ".to_string()
                };
                Line::from(Span::styled(format!("{}{}", prefix, text), style))
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines),
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: row_height,
            },
        );
        y += row_height;
    }
}

fn render_decision_detail(f: &mut Frame, req: &DecisionRequest, theme: &Theme, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let selected = if req.custom_selected() {
        if req.answer.is_empty() {
            "Press Tab, then type custom response.".to_string()
        } else {
            req.answer.clone()
        }
    } else {
        req.options.get(req.selected).cloned().unwrap_or_default()
    };
    f.render_widget(
        Paragraph::new(selected)
            .block(
                Block::default()
                    .title(" Selected option ")
                    .padding(Padding::new(1, 1, 1, 0))
                    .style(theme.surface()),
            )
            .style(Style::default().fg(theme.text))
            .wrap(ratatui::widgets::Wrap { trim: true }),
        area,
    );
}

fn decision_scroll_start(rows: &[Vec<String>], selected: usize, height: usize) -> usize {
    if rows.is_empty() || height == 0 {
        return 0;
    }
    let selected = selected.min(rows.len() - 1);
    let mut start = selected;
    let mut used = rows[selected].len().max(1);
    while start > 0 {
        let previous = rows[start - 1].len().max(1);
        if used + previous > height {
            break;
        }
        start -= 1;
        used += previous;
    }
    start
}

fn render_plan(f: &mut Frame, req: &PlanRequest, area: Rect, theme: &Theme) {
    let inner = panel(f, area, " Plan Review ", theme);
    let lines = vec![
        Line::from(Span::styled(
            "The assistant wrote a plan for your approval.",
            Style::default().fg(theme.text),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Path: ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                crate::render::path::display_path(&req.path),
                Style::default().fg(theme.text),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "e",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" edit in $EDITOR", Style::default().fg(theme.text)),
        ]),
        Line::from(vec![
            Span::styled(
                "a / Enter",
                Style::default()
                    .fg(theme.success)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" accept edited file", Style::default().fg(theme.text)),
        ]),
        Line::from(vec![
            Span::styled(
                "d / Esc",
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" deny", Style::default().fg(theme.text)),
        ]),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

// ── Tool request prompt ───────────────────────────────────────────────────────

fn render_tool_request(f: &mut Frame, req: &ToolRequest, area: Rect, theme: &Theme) {
    let inner = panel(f, area, " Model Requested Tools ", theme);

    let lines = vec![
        Line::from(Span::styled(
            "The assistant asked to use tools, but agent mode is OFF.",
            Style::default().fg(theme.text),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("Pending tool call(s): {}", req.count),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "y / Enter",
                Style::default()
                    .fg(theme.success)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  enable agent mode and run tools",
                Style::default().fg(theme.text),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "n / Esc",
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "    continue without tools",
                Style::default().fg(theme.text),
            ),
        ]),
    ];

    f.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: true }),
        inner,
    );
}

// ── API setup prompt ──────────────────────────────────────────────────────────

fn render_api_setup(f: &mut Frame, s: &ApiSetup, area: Rect, theme: &Theme) {
    let inner = panel(f, area, " API Setup ", theme);

    let field = |focused: bool, label: &str, value: String| {
        // Focused field shows a block cursor; the key is masked.
        let marker = if focused { "▸ " } else { "  " };
        let val_style = if focused {
            Style::default()
                .fg(theme.text)
                .add_modifier(Modifier::UNDERLINED)
        } else {
            Style::default().fg(theme.muted)
        };
        let shown = if value.is_empty() {
            "—".to_string()
        } else {
            value
        };
        vec![
            Span::styled(
                format!("{}{}: ", marker, label),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(shown, val_style),
            if focused {
                Span::styled("▌", Style::default().fg(theme.accent))
            } else {
                Span::raw("")
            },
        ]
    };
    let masked_key: String = if s.api_key.is_empty() {
        String::new()
    } else {
        "•".repeat(s.api_key.chars().count().min(24))
    };

    let lines = vec![
        Line::from(field(s.field == 0, "URL", s.endpoint.clone())),
        Line::from(""),
        Line::from(field(s.field == 1, "Key", masked_key)),
        Line::from(""),
        Line::from(Span::styled(
            "Tab switch field · ⏎ save · Esc cancel",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::ITALIC),
        )),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

// ── Helper ────────────────────────────────────────────────────────────────────

/// A padded, opaque overlay panel with `title`. Returns the inner content rect
/// after rendering the clear + solid surface.
fn panel(f: &mut Frame, area: Rect, title: &str, theme: &Theme) -> Rect {
    let bar_area = Rect {
        x: area.x,
        y: area.y,
        width: 1,
        height: area.height,
    };
    let block_area = Rect {
        x: area.x + 1,
        y: area.y,
        width: area.width.saturating_sub(1),
        height: area.height,
    };
    f.render_widget(Clear, block_area);
    let accent = overlay_accent(title, theme);
    let padding = panel_padding(block_area);
    let block = Block::default()
        .title(Span::styled(
            format!(" {} ", title.trim()),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ))
        .padding(padding)
        .style(theme.surface());
    let inner = block.inner(block_area);
    f.render_widget(block, block_area);
    if area.width > 0 {
        let rail: Vec<Line> = (0..area.height)
            .map(|_| Line::from(Span::styled("│", Style::default().fg(accent))))
            .collect();
        f.render_widget(Paragraph::new(rail), bar_area);
    }
    inner
}

fn panel_padding(area: Rect) -> Padding {
    let horizontal = if area.width >= 72 {
        4
    } else if area.width >= 40 {
        2
    } else {
        1
    };
    let vertical = u16::from(area.height >= 8);
    Padding::new(horizontal, 1, vertical, vertical)
}

fn overlay_accent(title: &str, theme: &Theme) -> Color {
    if title.contains("Subagent Access") {
        Color::Magenta
    } else if title.contains("Access") {
        theme.warning
    } else if title.contains("Decision") || title.contains("Question") {
        theme.link
    } else if title.contains("Plan") {
        theme.success
    } else if title.contains("API") {
        Color::Magenta
    } else if title.contains("Settings") {
        Color::Cyan
    } else if title.contains("Model") || title.contains("Session") || title.contains("Skill") {
        Color::Blue
    } else if title.contains("File") || title.contains("EDITOR") {
        Color::Green
    } else if title.contains("Command") {
        Color::Yellow
    } else if title.contains("Tools") {
        Color::Red
    } else {
        theme.accent
    }
}

#[cfg(test)]
mod tests {
    use super::{
        access_statement_height, command_lines, decision_scroll_start, height, horizontal_thumb,
        panel_padding, permission_code_card, permission_edit_start_line, picker_window,
        session_picker_lines, PermissionCard,
    };
    use crate::app::overlay::Overlay;
    use crate::render::theme::Theme;
    use ratatui::layout::Rect;
    use ratatui::style::Color;
    use ratatui::widgets::Padding;
    use std::path::Path;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn dock_height_is_absent_without_overlay_and_clamped_when_open() {
        assert_eq!(height(&Overlay::None, 80, 30), None);
        let notice = Overlay::Notice {
            title: "Notice".into(),
            body: "Body".into(),
        };
        assert_eq!(height(&notice, 80, 30), Some(11));
        assert_eq!(height(&notice, 80, 6), Some(6));
    }

    #[test]
    fn subagent_access_prompt_names_requester_and_has_distinct_height() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut root = crate::app::overlay::PermissionRequest::single(crate::agent::ToolCall {
            name: "write".into(),
            args: serde_json::json!({"path": "src/main.rs", "content": "fn main() {}"}),
            id: None,
        });
        let root_height = height(&Overlay::Permission(root.clone()), 80, 30).unwrap();
        root.child_request_id = Some(42);
        root.child_agent_label = Some("security-reviewer — audit auth flow".into());
        let child_overlay = Overlay::Permission(root);
        assert_eq!(
            height(&child_overlay, 80, 30).unwrap(),
            root_height.saturating_add(2)
        );

        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::default();
        terminal
            .draw(|frame| {
                super::render_permission(
                    frame,
                    match &child_overlay {
                        Overlay::Permission(request) => request,
                        _ => unreachable!(),
                    },
                    frame.area(),
                    &theme,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let screen = (0..30)
            .map(|y| (0..80).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("Subagent Access Request"), "{screen}");
        assert!(
            screen.contains("Requested by subagent: security-reviewer — audit auth flow"),
            "{screen}"
        );
    }

    #[test]
    fn access_statement_and_panel_padding_reflow_for_narrow_widths() {
        let request = crate::app::overlay::PermissionRequest::single(crate::agent::ToolCall {
            name: "edit".into(),
            args: serde_json::json!({
                "path": "src/main.rs",
                "old": "fn old() {}",
                "new": "fn new() {}",
            }),
            id: None,
        });
        assert_eq!(access_statement_height(&request, 120), 1);
        assert!(access_statement_height(&request, 30) > 1);
        assert_eq!(
            panel_padding(Rect::new(0, 0, 100, 20)),
            Padding::new(4, 1, 1, 1)
        );
        assert_eq!(
            panel_padding(Rect::new(0, 0, 30, 5)),
            Padding::new(1, 1, 0, 0)
        );
    }

    #[test]
    fn access_height_tracks_rendered_content_and_available_space() {
        let list = Overlay::Permission(crate::app::overlay::PermissionRequest::single(
            crate::agent::ToolCall {
                name: "list".into(),
                args: serde_json::json!({ "path": "src" }),
                id: None,
            },
        ));
        let edit = Overlay::Permission(crate::app::overlay::PermissionRequest::single(
            crate::agent::ToolCall {
                name: "edit".into(),
                args: serde_json::json!({
                    "path": "src/main.rs",
                    "old": "fn old() {}\n",
                    "new": "fn new() {\n    println!(\"new\");\n}\n",
                }),
                id: None,
            },
        ));
        let list_height = height(&list, 100, 40).unwrap();
        let edit_height = height(&edit, 100, 40).unwrap();
        assert!(list_height < 24);
        assert!(edit_height > list_height);
        assert_eq!(height(&edit, 100, 12), Some(12));
    }

    #[test]
    fn picker_window_keeps_selection_visible_and_centers_when_possible() {
        assert_eq!(picker_window(20, 0, 5), (0, 5));
        assert_eq!(picker_window(20, 10, 5), (8, 13));
        assert_eq!(picker_window(20, 19, 5), (15, 20));
        assert_eq!(picker_window(3, 2, 8), (0, 3));
        assert_eq!(picker_window(0, 0, 5), (0, 0));
    }

    #[test]
    fn selected_session_card_has_two_readable_lines_and_clear_cursor() {
        let theme = Theme::default();
        let lines = session_picker_lines(
            "●  Fix picker  ·  RUNNING here  ·  last just now  ·  cwd ~/src  ·  12 msg",
            80,
            true,
            "picker",
            ratatui::style::Style::default().fg(Color::Yellow),
            &theme,
        );
        assert_eq!(lines.len(), 2);
        assert!(lines[0].to_string().contains("▸ ● Fix picker"));
        assert!(lines[0].to_string().contains("RUNNING here"));
        assert!(lines[1].to_string().contains("last just now"));
        assert!(lines[1].to_string().contains("cwd ~/src"));
        assert!(lines[1].to_string().contains("12 msg"));
        let selected_bg = theme.selection().bg.unwrap_or(theme.subtle_pill);
        assert!(lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .all(|span| span.style.bg == Some(selected_bg)));
    }

    #[test]
    fn decision_scroll_keeps_selected_wrapped_option_visible() {
        let rows = vec![
            vec!["a".into()],
            vec!["b1".into(), "b2".into()],
            vec!["c".into()],
            vec!["d1".into(), "d2".into(), "d3".into()],
        ];
        assert_eq!(decision_scroll_start(&rows, 3, 4), 2);
        assert_eq!(decision_scroll_start(&rows, 1, 4), 0);
    }

    #[test]
    fn decision_scroll_handles_empty_or_zero_height() {
        assert_eq!(decision_scroll_start(&[], 0, 4), 0);
        assert_eq!(decision_scroll_start(&[vec!["a".into()]], 0, 0), 0);
    }

    #[test]
    fn horizontal_scrollbar_tracks_viewport() {
        assert_eq!(horizontal_thumb(20, 10, 0, 10), (0, 5));
        assert_eq!(horizontal_thumb(20, 10, 10, 10), (5, 5));
        assert_eq!(horizontal_thumb(8, 10, 0, 10), (0, 10));
    }

    #[test]
    fn access_review_edit_uses_actual_source_line_numbers() {
        let root = std::env::temp_dir().join(format!(
            "aitui-access-lines-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("sample.rs"),
            "fn first() {}\n\nfn target() {\n    false\n}\n",
        )
        .unwrap();
        let call = crate::agent::ToolCall {
            name: "edit".into(),
            args: serde_json::json!({
                "path": "sample.rs",
                "old": "fn target() {\n    false\n}",
                "new": "fn target() {\n    true\n}"
            }),
            id: None,
        };
        assert_eq!(permission_edit_start_line(&call, &root), 3);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn code_cards_have_line_numbers_gutter_and_horizontal_scroll() {
        let theme = Theme::default();
        let card = PermissionCard {
            label: "NEW",
            value: "fn main() { let value = 123456789; }",
            language: "main.rs",
            color: theme.success,
            start_line: 1,
        };
        let left = permission_code_card(&card, 20, 0, &theme);
        let right = permission_code_card(&card, 20, 12, &theme);
        let row_text = |rows: &[Vec<ratatui::text::Span<'static>>], index: usize| {
            rows[index]
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };

        assert!(row_text(&left, 1).contains("1 │ fn main"));
        assert!(left
            .last()
            .is_some_and(|row| row.iter().any(|span| span.content.contains('━'))));
        assert_ne!(row_text(&left, 1), row_text(&right, 1));

        let tiny = permission_code_card(&card, 4, 0, &theme);
        assert!(tiny.iter().all(|row| {
            row.iter()
                .map(|span| span.content.as_ref().width())
                .sum::<usize>()
                <= 4
        }));
    }

    #[test]
    fn access_edit_request_includes_responsive_terminal_diff_cards() {
        let call = crate::agent::ToolCall {
            name: "edit".into(),
            args: serde_json::json!({
                "path": "src/main.rs",
                "old": "fn old() -> i32 { 1 }",
                "new": "fn new() -> i32 { 2 }",
            }),
            id: None,
        };
        let theme = Theme::default();
        let lines = command_lines(&[call], &theme, Path::new("."), 80, 0, None, &[]);
        assert!(lines.iter().any(|line| line
            .spans
            .iter()
            .any(|span| span.content.as_ref() == "PATH")));
        assert!(!lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| { matches!(span.content.as_ref(), "path:" | "old:" | "new:" | "diff:") }));
        assert!(lines
            .iter()
            .any(|line| line.spans.iter().any(|span| span.content.as_ref() == "OLD")));
        assert!(lines
            .iter()
            .any(|line| { line.spans.iter().any(|span| span.content.as_ref() == "NEW") }));
        assert_eq!(
            lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .filter(|span| {
                    span.content.as_ref() == "fn"
                        && span.style.fg == Some(theme.hl_keyword)
                        && span.style.bg == Some(Color::Reset)
                })
                .count(),
            2
        );
        assert!(lines.iter().flat_map(|line| line.spans.iter()).any(|span| {
            span.content.as_ref() == "i32"
                && span.style.fg == Some(theme.hl_type)
                && span.style.bg == Some(Color::Reset)
        }));
        for function in ["old", "new"] {
            assert!(lines.iter().flat_map(|line| line.spans.iter()).any(|span| {
                span.content.as_ref() == function
                    && span.style.fg == Some(theme.hl_function)
                    && span.style.bg == Some(Color::Reset)
            }));
        }
    }

    #[test]
    fn access_batch_requests_show_each_concrete_command_and_edit() {
        let calls = vec![
            crate::agent::ToolCall {
                name: "shell".into(),
                args: serde_json::json!({"commands": ["cargo test", "cargo clippy"]}),
                id: None,
            },
            crate::agent::ToolCall {
                name: "file_management".into(),
                args: serde_json::json!({
                    "action": "edit",
                    "batch": [
                        {"path": "a.rs", "old": "old a", "new": "new a"},
                        {"path": "b.rs", "old": "old b", "new": "new b"}
                    ]
                }),
                id: None,
            },
        ];
        let lines = command_lines(&calls, &Theme::default(), Path::new("."), 80, 0, None, &[]);
        let text = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        for expected in [
            "cargo test",
            "cargo clippy",
            "a.rs",
            "old a",
            "new a",
            "b.rs",
            "old b",
            "new b",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in {text:?}");
        }
        assert!(!text.contains("COMMAND│  │"));
    }

    #[test]
    fn access_shell_request_shows_only_a_command_card() {
        let call = crate::agent::ToolCall {
            name: "shell".into(),
            args: serde_json::json!({ "command": "cargo test" }),
            id: None,
        };
        let theme = Theme::default();
        let lines = command_lines(&[call], &theme, Path::new("."), 80, 0, None, &[]);
        let text: Vec<_> = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains(&"COMMAND"));
        assert!(!text.contains(&"PATH"));
        assert!(!text.contains(&"OLD"));
        assert!(!text.contains(&"NEW"));
        assert!(lines.iter().flat_map(|line| line.spans.iter()).any(|span| {
            span.content.as_ref() == "cargo"
                && span.style.fg != Some(theme.text)
                && span.style.bg == Some(Color::Reset)
        }));
    }

    #[test]
    fn access_list_request_shows_only_its_path_card() {
        let call = crate::agent::ToolCall {
            name: "list".into(),
            args: serde_json::json!({ "path": "src", "depth": "3" }),
            id: None,
        };
        let theme = Theme::default();
        let lines = command_lines(&[call], &theme, Path::new("."), 80, 0, None, &[]);
        let text: Vec<_> = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains(&"PATH"));
        assert!(text.contains(&"src"));
        assert!(!text.contains(&"OLD"));
        assert!(!text.contains(&"NEW"));
        assert!(!text.contains(&"DEPTH"));
    }

    #[test]
    fn access_move_request_uses_from_and_to_cards() {
        let call = crate::agent::ToolCall {
            name: "move".into(),
            args: serde_json::json!({ "from": "old.txt", "to": "new.txt" }),
            id: None,
        };
        let theme = Theme::default();
        let lines = command_lines(&[call], &theme, Path::new("."), 80, 0, None, &[]);
        let text: Vec<_> = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains(&"FROM"));
        assert!(text.contains(&"TO"));
        assert!(!text.contains(&"PATH"));
        assert!(!text.contains(&"OLD"));
        assert!(!text.contains(&"NEW"));
    }
}
