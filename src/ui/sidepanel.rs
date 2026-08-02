use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::app::state::{AccessHitbox, App, SubtaskHitbox, TodoStatus};
use crate::render::path::display_path;
use crate::render::theme::{fg_guard, Theme};
use crate::render::wrap::wrap_words;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const SIDEBAR_MIN_WIDTH: u16 = 20;

/// Click targets produced by the sidebar: child-agent rows and access rules.
#[derive(Debug, Default)]
pub struct SidebarHitboxes {
    pub agents: Vec<SubtaskHitbox>,
    pub access: Vec<AccessHitbox>,
    pub tasks: Option<Rect>,
}

/// Render the sidebar. Returns click targets for the child-agent rows.
pub fn render(f: &mut Frame, app: &App, area: Rect, theme: &Theme) -> SidebarHitboxes {
    if area.width < SIDEBAR_MIN_WIDTH || area.height < 4 {
        return SidebarHitboxes::default();
    }

    let session = app.sessions.active();
    let surface = theme.surface();
    f.render_widget(Clear, area);
    f.render_widget(Block::default().style(surface), area);

    let inner_x = area.x.saturating_add(2);
    let inner_w = area.width.saturating_sub(4) as usize;
    let bottom = area.y.saturating_add(area.height);
    let cwd = session
        .cwd
        .as_ref()
        .map(|path| display_path(path))
        .unwrap_or_default();
    let directory_lines = directory_lines(&cwd, inner_w, theme);
    let directory_height = directory_lines.len().min(area.height as usize) as u16;
    let directory_y = bottom.saturating_sub(directory_height);
    render_lines(
        f,
        inner_x,
        directory_y,
        inner_w,
        bottom,
        directory_lines,
        surface,
    );
    let content_bottom = directory_y.saturating_sub(1);

    let mut y = area.y.saturating_add(1);

    y = render_lines(
        f,
        inner_x,
        y,
        inner_w,
        content_bottom,
        brand_lines(inner_w, app.focused, theme),
        surface,
    );
    y = y.saturating_add(1);

    y = render_section(f, inner_x, y, inner_w, content_bottom, "Workspace", theme);
    y = y.saturating_add(1);

    y = render_labeled(
        f,
        inner_x,
        y,
        inner_w,
        content_bottom,
        "Session",
        &session.name,
        theme,
    );

    y = y.saturating_add(1);

    let usage = app.session_usage.get(&session.id);
    if usage.is_some() {
        y = render_section(f, inner_x, y, inner_w, content_bottom, "Usage", theme);
        y = y.saturating_add(1);
        y = render_context(
            f,
            inner_x,
            y,
            inner_w,
            content_bottom,
            usage,
            app.config.ui.context_window,
            theme,
        );
        y = y.saturating_add(1);
    }

    let active_skills = app
        .skills
        .iter()
        .filter(|skill| skill.active)
        .map(|skill| skill.name.as_str())
        .collect::<Vec<_>>();
    if !active_skills.is_empty() && y < content_bottom {
        y = y.saturating_add(1);
        y = render_labeled(
            f,
            inner_x,
            y,
            inner_w,
            content_bottom,
            "Skills",
            &active_skills.join(", "),
            theme,
        );
    }

    let access_count = app.access_entries().len();
    let mut access_hitboxes: Vec<AccessHitbox> = Vec::new();
    if y < content_bottom {
        y = y.saturating_add(1);
        y = render_section(f, inner_x, y, inner_w, content_bottom, "Access", theme);
        y = y.saturating_add(1);
        if y < content_bottom {
            let mode = app.config.api.access_review_mode;
            let (mark, status, color) = if mode == crate::config::AccessReviewMode::Off {
                ("○", "Review model off".to_string(), theme.muted)
            } else if app.judging.is_some() {
                (
                    "◉",
                    format!("Review model {} · working", mode.label()),
                    theme.warning,
                )
            } else {
                (
                    "●",
                    format!("Review model {} · on", mode.label()),
                    theme.success,
                )
            };
            y = render_lines(
                f,
                inner_x,
                y,
                inner_w,
                content_bottom,
                vec![Line::from(vec![
                    Span::styled(format!("{} ", mark), Style::default().fg(color)),
                    Span::styled(status, Style::default().fg(theme.text)),
                ])],
                surface,
            );
            y = y.saturating_add(1);
        }
        if y < content_bottom {
            let label = if access_count == 1 {
                "Manage 1 access rule".to_string()
            } else {
                format!("Manage {} access rules", access_count)
            };
            let rows = Rect {
                x: inner_x,
                y,
                width: inner_w as u16,
                height: 1,
            };
            access_hitboxes.push(AccessHitbox {
                index: usize::MAX,
                area: rows,
            });
            y = render_lines(
                f,
                inner_x,
                y,
                inner_w,
                content_bottom,
                vec![Line::from(vec![
                    Span::styled(
                        " ◈ ",
                        Style::default()
                            .bg(theme.accent)
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(" {} ", label),
                        Style::default()
                            .bg(theme.subtle_pill)
                            .fg(fg_guard(theme.text))
                            .add_modifier(Modifier::BOLD),
                    ),
                ])],
                surface,
            );
        }
    }

    let sid_agents: Vec<&crate::app::state::Subtask> = app
        .subtasks
        .iter()
        .filter(|task| task.session_id == session.id)
        .collect();
    if !sid_agents.is_empty() && y < content_bottom {
        use crate::app::state::SubtaskStatus;
        let running = sid_agents
            .iter()
            .filter(|task| task.status == SubtaskStatus::Running)
            .count();
        let done = sid_agents.len().saturating_sub(running);
        y = y.saturating_add(1);
        y = render_section(
            f,
            inner_x,
            y,
            inner_w,
            content_bottom,
            &format!("Agents  {} running · {} done", running, done),
            theme,
        );
        y = y.saturating_add(1);
        let ms = crate::ui::statusbar::now_ms();
        let mut hitboxes: Vec<SubtaskHitbox> = Vec::new();
        for task in &sid_agents {
            if y >= content_bottom {
                break;
            }
            let depth = tree_depth(app, task);
            let entered = app.view_node == Some(task.id);
            let lines = agent_lines(task, ms, inner_w, theme);
            let lines = indent_lines(lines, depth, entered);
            let remaining = content_bottom.saturating_sub(y) as usize;
            if lines.len() > remaining {
                break;
            }
            let rows = Rect {
                x: inner_x,
                y,
                width: inner_w as u16,
                height: lines.len() as u16,
            };
            hitboxes.push(SubtaskHitbox {
                task_id: task.id,
                area: rows,
            });
            y = render_lines(
                f,
                inner_x,
                y,
                inner_w,
                content_bottom,
                lines,
                theme.surface(),
            );
        }
        return SidebarHitboxes {
            agents: hitboxes,
            access: access_hitboxes,
            tasks: None,
        };
    }

    if session.todos.is_empty() || y >= content_bottom {
        return SidebarHitboxes {
            access: access_hitboxes,
            ..SidebarHitboxes::default()
        };
    }

    y = y.saturating_add(1);
    let done = session
        .todos
        .iter()
        .filter(|todo| todo.status == TodoStatus::Done)
        .count();
    let overall = session.todo_overall_percent;
    let header = match overall {
        Some(percent) => format!("Tasks  {}/{} · {}%", done, session.todos.len(), percent),
        None => format!("Tasks  {}/{}", done, session.todos.len()),
    };
    y = render_section(f, inner_x, y, inner_w, content_bottom, &header, theme);
    y = y.saturating_add(1);
    y = render_task_progress(
        f,
        inner_x,
        y,
        inner_w,
        content_bottom,
        overall.unwrap_or_else(|| {
            if session.todos.is_empty() {
                0
            } else {
                (done * 100 / session.todos.len()) as u8
            }
        }),
        theme,
    );
    y = y.saturating_add(1);

    let task_top = y;
    let task_area = Rect {
        x: inner_x,
        y: task_top,
        width: inner_w as u16,
        height: content_bottom.saturating_sub(task_top),
    };
    let mut task_lines = Vec::new();
    for todo in &session.todos {
        let (glyph, color) = match todo.status {
            TodoStatus::Done => ("◉", Color::Green),
            TodoStatus::InProgress => ("◐", Color::Yellow),
            TodoStatus::Pending => ("○", theme.text),
        };
        task_lines.extend(task_item_lines(
            glyph,
            todo.percent,
            &todo.text,
            inner_w,
            fg_guard(color),
            theme,
        ));
    }
    let visible_h = task_area.height as usize;
    let max_scroll = task_lines.len().saturating_sub(visible_h);
    let start = app.sidebar_task_scroll.min(max_scroll);
    let end = (start + visible_h).min(task_lines.len());
    if task_area.height > 0 {
        f.render_widget(
            Paragraph::new(task_lines[start..end].to_vec()).style(surface),
            task_area,
        );
        if start > 0 {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled("▲", theme.subtle()))),
                Rect {
                    x: task_area.x + task_area.width.saturating_sub(1),
                    y: task_area.y,
                    width: 1,
                    height: 1,
                },
            );
        }
        if end < task_lines.len() {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled("▼", theme.subtle()))),
                Rect {
                    x: task_area.x + task_area.width.saturating_sub(1),
                    y: task_area.y + task_area.height.saturating_sub(1),
                    width: 1,
                    height: 1,
                },
            );
        }
    }
    SidebarHitboxes {
        access: access_hitboxes,
        tasks: Some(task_area),
        ..SidebarHitboxes::default()
    }
}

/// Display name for a child agent: the named `[agents]` entry when the call
/// referenced one, otherwise its launch index (`agent 2`).
pub(crate) fn agent_display_name(task: &crate::app::state::Subtask) -> String {
    if let Some(name) = task.agent.as_deref() {
        return name.to_string();
    }
    match task
        .call
        .args
        .get("agent_index")
        .and_then(|value| value.as_u64())
    {
        Some(index) => format!("agent {}", index),
        None => "agent".to_string(),
    }
}

/// Two rows per child agent: status glyph + name + right-aligned elapsed time,
/// then the current activity (wrapped, indented).
pub(crate) fn agent_lines(
    task: &crate::app::state::Subtask,
    ms: u128,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    use crate::app::state::SubtaskStatus;
    let (glyph, color) = match task.status {
        SubtaskStatus::Running => (
            crate::ui::statusbar::frame(&crate::ui::statusbar::BUFFER, ms, 110 + task.id as u128),
            theme.warning,
        ),
        SubtaskStatus::Completed => ("●", theme.success),
        SubtaskStatus::Failed => ("×", theme.danger),
    };
    let name = agent_display_name(task);
    let elapsed_ms = task
        .duration_ms
        .unwrap_or_else(|| task.started_at.elapsed().as_millis() as u64);
    let elapsed = crate::render::document::fmt_duration_ms(elapsed_ms);
    let head = format!("{} {}", glyph, name);
    let head_width = UnicodeWidthStr::width(head.as_str());
    let elapsed_width = UnicodeWidthStr::width(elapsed.as_str());
    let gap = " ".repeat(
        width
            .saturating_sub(head_width.saturating_add(elapsed_width))
            .max(1),
    );
    let mut lines = vec![Line::from(vec![
        Span::styled(head, Style::default().fg(fg_guard(color))),
        Span::styled(
            gap,
            Style::default()
                .fg(fg_guard(theme.text))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            elapsed,
            Style::default()
                .fg(fg_guard(theme.muted))
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    let activity = task.activity.as_deref().unwrap_or(&task.description);
    if !activity.is_empty() {
        lines.extend(prefixed_lines("  ", activity, width, theme.muted));
    }
    lines
}

/// Tree depth of a node: root-level agents are 0, their children 1, etc.
fn tree_depth(app: &App, task: &crate::app::state::Subtask) -> usize {
    let mut depth = 0;
    let mut parent = task.parent_id;
    while let Some(pid) = parent {
        if let Some(node) = app.subtasks.iter().find(|t| t.id == pid) {
            parent = node.parent_id;
            depth += 1;
        } else {
            break;
        }
    }
    depth
}

/// Indent agent rows by tree depth, marking the currently entered node.
fn indent_lines(lines: Vec<Line<'static>>, depth: usize, entered: bool) -> Vec<Line<'static>> {
    let indent = "  ".repeat(depth.min(8));
    lines
        .into_iter()
        .map(|line| {
            let mut spans = vec![Span::raw(indent.clone())];
            spans.extend(line.spans);
            let style = if entered {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Line::from(spans).style(style)
        })
        .collect()
}

fn directory_lines(value: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let style = Style::default()
        .fg(fg_guard(theme.text))
        .add_modifier(Modifier::BOLD);
    wrapped_text(value, width)
        .into_iter()
        .map(|line| Line::from(Span::styled(line, style)))
        .collect()
}

fn brand_lines(width: usize, focused: bool, theme: &Theme) -> Vec<Line<'static>> {
    let brand = "◆ AITUI";
    let state = if focused { "●" } else { "○" };
    let meta = format!("v{} {}", VERSION, state);
    let used = UnicodeWidthStr::width(brand) + UnicodeWidthStr::width(meta.as_str());
    let gap = " ".repeat(width.saturating_sub(used).max(1));
    vec![Line::from(vec![
        Span::styled(
            brand,
            Style::default()
                .fg(fg_guard(theme.accent))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(gap),
        Span::styled(meta, Style::default().fg(fg_guard(theme.muted))),
    ])]
}

#[allow(clippy::too_many_arguments)]
fn render_section(
    f: &mut Frame,
    x: u16,
    y: u16,
    width: usize,
    bottom: u16,
    title: &str,
    theme: &Theme,
) -> u16 {
    let label = title.to_uppercase();
    render_lines(
        f,
        x,
        y,
        width,
        bottom,
        vec![Line::from(Span::styled(
            label,
            Style::default()
                .fg(fg_guard(theme.accent))
                .add_modifier(Modifier::BOLD),
        ))],
        theme.surface(),
    )
}

fn progress_meter_line(
    width: usize,
    percent: usize,
    fill_color: Color,
    theme: &Theme,
) -> Line<'static> {
    let percent = percent.min(100);
    let meter_width = width.saturating_sub(7);
    let filled = percent.saturating_mul(meter_width) / 100;

    Line::from(vec![
        Span::styled("[", Style::default().fg(fg_guard(theme.muted))),
        Span::styled(
            "█".repeat(filled),
            Style::default()
                .fg(fg_guard(fill_color))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "░".repeat(meter_width.saturating_sub(filled)),
            Style::default().fg(fg_guard(theme.muted)),
        ),
        Span::styled("]", Style::default().fg(fg_guard(theme.muted))),
        Span::styled(
            format!(" {:>3}%", percent),
            Style::default()
                .fg(fg_guard(theme.text))
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

#[allow(clippy::too_many_arguments)]
fn render_task_progress(
    f: &mut Frame,
    x: u16,
    y: u16,
    width: usize,
    bottom: u16,
    percent: u8,
    theme: &Theme,
) -> u16 {
    render_lines(
        f,
        x,
        y,
        width,
        bottom,
        vec![progress_meter_line(
            width,
            percent as usize,
            theme.success,
            theme,
        )],
        theme.surface(),
    )
}

#[allow(clippy::too_many_arguments)]
fn render_labeled(
    f: &mut Frame,
    x: u16,
    y: u16,
    width: usize,
    bottom: u16,
    label: &str,
    value: &str,
    theme: &Theme,
) -> u16 {
    render_lines(
        f,
        x,
        y,
        width,
        bottom,
        labeled_lines(label, value, width, theme),
        theme.surface(),
    )
}

fn render_lines(
    f: &mut Frame,
    x: u16,
    y: u16,
    width: usize,
    bottom: u16,
    mut lines: Vec<Line<'static>>,
    surface: Style,
) -> u16 {
    if width == 0 || y >= bottom || lines.is_empty() {
        return y;
    }
    let visible = lines.len().min(bottom.saturating_sub(y) as usize);
    lines.truncate(visible);
    f.render_widget(
        Paragraph::new(lines).style(surface),
        Rect {
            x,
            y,
            width: width.min(u16::MAX as usize) as u16,
            height: visible.min(u16::MAX as usize) as u16,
        },
    );
    y.saturating_add(visible as u16)
}

fn labeled_lines(label: &str, value: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let label = label.to_uppercase();
    let label_width = UnicodeWidthStr::width(label.as_str()).max(9);
    let prefix = format!("{:<label_width$}", label);
    let prefix_width = UnicodeWidthStr::width(prefix.as_str());
    let label_style = Style::default()
        .fg(fg_guard(theme.muted))
        .add_modifier(Modifier::BOLD);
    let value_style = Style::default().fg(fg_guard(theme.text));

    if width <= prefix_width.saturating_add(3) {
        let mut lines = vec![Line::from(Span::styled(label.to_string(), label_style))];
        lines.extend(
            wrapped_text(value, width)
                .into_iter()
                .map(|text| Line::from(Span::styled(text, value_style))),
        );
        return lines;
    }

    let value_width = width - prefix_width;
    wrapped_text(value, value_width)
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            let lead = if index == 0 {
                prefix.clone()
            } else {
                " ".repeat(prefix_width)
            };
            Line::from(vec![
                Span::styled(lead, label_style),
                Span::styled(text, value_style),
            ])
        })
        .collect()
}

fn task_item_lines(
    glyph: &str,
    percent: Option<u8>,
    text: &str,
    width: usize,
    color: Color,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let prefix = format!("{} ", glyph);
    let suffix = percent.map(|value| format!("  {}%", value));
    let suffix_width = suffix.as_deref().map(UnicodeWidthStr::width).unwrap_or(0);
    let text_width = width.saturating_sub(2).saturating_sub(suffix_width).max(1);
    let mut wrapped = wrapped_text(text, text_width);
    if wrapped.is_empty() {
        wrapped.push(String::new());
    }
    wrapped
        .into_iter()
        .enumerate()
        .map(|(index, part)| {
            let lead = if index == 0 {
                prefix.clone()
            } else {
                "  ".to_string()
            };
            let mut spans = vec![
                Span::styled(lead, Style::default().fg(color)),
                Span::styled(part, Style::default().fg(color)),
            ];
            if index == 0 {
                if let Some(suffix) = suffix.clone() {
                    spans.push(Span::styled(
                        suffix,
                        Style::default()
                            .fg(fg_guard(theme.muted))
                            .add_modifier(Modifier::BOLD),
                    ));
                }
            }
            Line::from(spans)
        })
        .collect()
}

fn prefixed_lines(prefix: &str, text: &str, width: usize, color: Color) -> Vec<Line<'static>> {
    let prefix_width = UnicodeWidthStr::width(prefix);
    let style = Style::default().fg(fg_guard(color));
    if width <= prefix_width {
        return wrapped_text(text, width)
            .into_iter()
            .map(|line| Line::from(Span::styled(line, style)))
            .collect();
    }

    wrapped_text(text, width - prefix_width)
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            let lead = if index == 0 {
                prefix.to_string()
            } else {
                " ".repeat(prefix_width)
            };
            Line::from(vec![Span::styled(lead, style), Span::styled(text, style)])
        })
        .collect()
}

fn wrapped_text(text: &str, width: usize) -> Vec<String> {
    text.split('\n')
        .flat_map(|line| wrap_words(line, width.max(1)))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn render_context(
    f: &mut Frame,
    x: u16,
    y: u16,
    width: usize,
    bottom: u16,
    usage: Option<&crate::api::Usage>,
    context_window: u32,
    theme: &Theme,
) -> u16 {
    let (percent, label) = match usage {
        Some(usage) if context_window > 0 => {
            let percent =
                (usage.total_tokens as f64 / context_window as f64 * 100.0).min(100.0) as u16;
            (
                percent,
                format!(
                    "{}K in + {}K out of {}K tokens",
                    usage.prompt_tokens / 1000,
                    usage.completion_tokens / 1000,
                    context_window / 1000
                ),
            )
        }
        _ => (0, "No usage yet".to_string()),
    };

    let fill_color = if percent >= 85 {
        theme.warning
    } else {
        theme.accent
    };
    let lines = vec![
        progress_meter_line(width, percent as usize, fill_color, theme),
        Line::from(Span::styled(
            label,
            Style::default().fg(fg_guard(theme.muted)),
        )),
    ];
    render_lines(f, x, y, width, bottom, lines, theme.surface())
}

#[cfg(test)]
mod tests {
    use super::{
        agent_display_name, agent_lines, brand_lines, directory_lines, labeled_lines,
        prefixed_lines, progress_meter_line, task_item_lines,
    };
    use crate::agent::ToolCall;
    use crate::app::state::{Subtask, SubtaskStatus};
    use crate::render::theme::Theme;
    use unicode_width::UnicodeWidthStr;

    fn subtask(
        id: u64,
        status: SubtaskStatus,
        agent: Option<&str>,
        activity: Option<&str>,
    ) -> Subtask {
        let mut call = ToolCall {
            name: "agent".into(),
            args: serde_json::json!({"agent_index": 2}),
            id: None,
        };
        if agent.is_some() {
            call.args
                .as_object_mut()
                .map(|args| args.insert("agent".into(), serde_json::json!(agent)));
        }
        Subtask {
            id,
            session_id: 0,
            parent_id: None,
            call,
            description: "audit the auth flow".into(),
            todo_index: None,
            prompt: String::new(),
            cwd: std::path::PathBuf::from("."),
            status,
            activity: activity.map(str::to_string),
            log: Vec::new(),
            transcript: Vec::new(),
            output: None,
            message_index: 0,
            started_at: std::time::Instant::now(),
            duration_ms: None,
            abort: None,
            agent: agent.map(str::to_string),
        }
    }

    #[test]
    fn agent_rows_show_glyph_name_and_activity() {
        let theme = Theme::default();
        let task = subtask(
            1,
            SubtaskStatus::Running,
            Some("docs"),
            Some("Running read(src)"),
        );
        let lines = agent_lines(&task, 0, 24, &theme);
        assert!(lines[0].to_string().contains("docs"));
        assert!(lines[0].to_string().starts_with("⠋"));
        assert!(lines[1].to_string().contains("Running read(src)"));
    }

    #[test]
    fn unnamed_agent_falls_back_to_launch_index() {
        let task = subtask(1, SubtaskStatus::Completed, None, None);
        assert_eq!(agent_display_name(&task), "agent 2");
    }

    #[test]
    fn named_agent_uses_config_name() {
        let task = subtask(1, SubtaskStatus::Failed, Some("reviewer"), Some("stuck"));
        assert_eq!(agent_display_name(&task), "reviewer");
        let lines = agent_lines(&task, 0, 24, &Theme::default());
        assert!(lines.len() >= 2);
        assert!(lines[0].to_string().contains("×"));
    }

    #[test]
    fn brand_header_balances_identity_version_and_focus_state() {
        let theme = Theme::default();
        let focused = brand_lines(24, true, &theme);
        let blurred = brand_lines(24, false, &theme);
        assert_eq!(focused.len(), 1);
        assert!(focused[0].to_string().contains("◆ AITUI"));
        assert!(focused[0].to_string().contains("●"));
        assert!(blurred[0].to_string().contains("○"));
        assert!(UnicodeWidthStr::width(focused[0].to_string().as_str()) <= 24);
    }

    #[test]
    fn labels_use_a_consistent_metadata_column() {
        let theme = Theme::default();
        let session = labeled_lines("Session", "demo", 24, &theme);
        let model = labeled_lines("Model", "gpt", 24, &theme);
        assert!(session[0].to_string().starts_with("SESSION  "));
        assert!(model[0].to_string().starts_with("MODEL    "));
    }

    #[test]
    fn session_and_cwd_values_wrap_with_continuation_indent() {
        let theme = Theme::default();
        let lines = labeled_lines("Session", "a long session name that wraps", 16, &theme);
        assert!(lines.len() > 1);
        for line in lines {
            assert!(UnicodeWidthStr::width(line.to_string().as_str()) <= 16);
        }

        let cwd = labeled_lines("CWD", "/a/very/long/project/directory", 14, &theme);
        assert!(cwd.len() > 1);
        for line in cwd {
            assert!(UnicodeWidthStr::width(line.to_string().as_str()) <= 14);
        }
    }

    #[test]
    fn directory_footer_is_value_only_without_a_header() {
        let theme = Theme::default();
        let lines = directory_lines("~/Codes/aitui", 24, &theme);
        assert_eq!(lines[0].to_string(), "~/Codes/aitui");
        assert!(!lines
            .iter()
            .any(|line| line.to_string().contains("DIRECTORY")));
    }

    #[test]
    fn task_text_wraps_and_keeps_space_for_the_status_glyph() {
        let lines = prefixed_lines(
            "○ ",
            "implement the long sidebar task description",
            18,
            ratatui::style::Color::Reset,
        );
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(UnicodeWidthStr::width(line.to_string().as_str()) <= 18);
        }
        assert!(lines[0].to_string().starts_with("○ "));
        assert!(lines[1].to_string().starts_with("  "));
    }

    #[test]
    fn task_percentage_is_inline_with_the_task_name() {
        let theme = Theme::default();
        let lines = task_item_lines(
            "◐",
            Some(85),
            "Run formatter and focused tests",
            40,
            theme.warning,
            &theme,
        );
        assert!(lines[0]
            .to_string()
            .contains("Run formatter and focused tests  85%"));
    }

    #[test]
    fn progress_meter_is_enclosed_and_uses_block_cells() {
        let theme = Theme::default();
        let line = progress_meter_line(24, 50, theme.success, &theme);
        let text = line.to_string();
        assert!(text.starts_with('['));
        assert!(text.contains('█'));
        assert!(text.contains('░'));
        assert!(text.ends_with("  50%"));
        assert_eq!(UnicodeWidthStr::width(text.as_str()), 24);
    }

    #[test]
    fn progress_meter_clamps_percent_and_handles_narrow_widths() {
        let theme = Theme::default();
        let full = progress_meter_line(12, 150, theme.accent, &theme).to_string();
        assert!(full.ends_with(" 100%"));
        assert_eq!(UnicodeWidthStr::width(full.as_str()), 12);

        let narrow = progress_meter_line(4, 25, theme.accent, &theme).to_string();
        assert_eq!(narrow, "[]  25%");
    }

    #[test]
    fn ansi_eight_never_becomes_sidebar_text() {
        let lines = prefixed_lines("○ ", "pending task", 20, ratatui::style::Color::DarkGray);
        for span in &lines[0].spans {
            assert_ne!(span.style.fg, Some(ratatui::style::Color::DarkGray));
        }
    }
}
