//! Sticky child-agent panel pinned below the chat. Shows a compact agent list
//! (same rows as the sidebar) when the view is at the root, and expands into
//! full per-agent detail — status, duration, cwd, prompt, activity log, report —
//! plus its indented child tree when an agent is entered (sidebar row or panel
//! row click).

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::state::{App, Subtask, SubtaskHitbox, SubtaskStatus};
use crate::render::theme::Theme;
use crate::render::wrap::wrap_words;

/// Most agents to list before eliding in compact mode.
const MAX_COMPACT_AGENTS: usize = 4;
/// Cap on the expanded detail panel, so a huge log can't swallow the screen.
const MAX_DETAIL_LINES: usize = 24;
/// Prompt preview lines in the expanded detail.
const MAX_PROMPT_LINES: usize = 6;
/// Report preview lines in the expanded detail.
const MAX_REPORT_LINES: usize = 10;
/// Tool-result output preview lines per tool entry.
const MAX_TOOL_OUTPUT_LINES: usize = 2;

fn active_tasks<'a>(app: &'a App) -> Vec<&'a Subtask> {
    let sid = app.sessions.active_id();
    app.subtasks
        .iter()
        .filter(|task| task.session_id == sid)
        .collect()
}

fn find_task<'a>(tasks: &'a [&Subtask], id: u64) -> Option<&'a Subtask> {
    tasks.iter().find(|task| task.id == id).copied()
}

/// Direct children of `task`, in registration order.
fn children_of<'a>(app: &'a App, task: &Subtask) -> Vec<&'a Subtask> {
    let sid = task.session_id;
    app.subtasks
        .iter()
        .filter(|t| t.session_id == sid && t.parent_id == Some(task.id))
        .collect()
}

/// Panel height: 0 when the session has no child agents; otherwise the header
/// plus either the compact list or the entered agent's detail + child rows.
pub fn height(app: &App, width: usize) -> u16 {
    let tasks = active_tasks(app);
    if tasks.is_empty() {
        return 0;
    }
    let entered = app.view_node.and_then(|id| find_task(&tasks, id));
    let content = match entered {
        Some(task) => {
            let lines = detail_lines(task, width, &app.theme());
            let child_rows = children_of(app, task).len().min(MAX_COMPACT_AGENTS);
            lines
                .len()
                .min(MAX_DETAIL_LINES)
                .saturating_add(child_rows)
                .saturating_add(1)
        }
        None => tasks.len().min(MAX_COMPACT_AGENTS).saturating_mul(2),
    };
    (content + 1) as u16
}

/// Render the panel and return click targets for its agent rows.
pub fn render(f: &mut Frame, app: &App, area: Rect, theme: &Theme) -> Vec<SubtaskHitbox> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let tasks = active_tasks(app);
    if tasks.is_empty() {
        return Vec::new();
    }

    let pad = 3u16;
    let inner = Rect {
        x: area.x.saturating_add(pad),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(pad.saturating_mul(2)),
        height: area.height.saturating_sub(2),
    };
    f.render_widget(Paragraph::new("").style(theme.surface()), area);

    let running = tasks
        .iter()
        .filter(|task| task.status == SubtaskStatus::Running)
        .count();
    let done = tasks.len().saturating_sub(running);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" AGENTS {} running · {} done", running, done),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))),
        Rect {
            y: area.y,
            height: 1,
            ..inner
        },
    );

    let mut hitboxes = Vec::new();
    let mut y = inner.y;
    let ms = crate::ui::statusbar::now_ms();

    let entered = app.view_node.and_then(|id| find_task(&tasks, id));
    match entered {
        Some(task) => {
            // Clicking the entered row collapses the panel back to the root view.
            let head = Line::from(Span::styled(
                format!(" ▾ {}", crate::ui::sidepanel::agent_display_name(task)),
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ));
            let head_area = Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 1,
            };
            hitboxes.push(SubtaskHitbox {
                task_id: task.id,
                area: head_area,
            });
            f.render_widget(Paragraph::new(head), head_area);
            y = y.saturating_add(1);

            let mut lines = detail_lines(task, inner.width as usize, theme);
            lines.truncate(MAX_DETAIL_LINES);
            for line in lines {
                if y >= inner.y + inner.height {
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
                y = y.saturating_add(1);
            }

            let children = children_of(app, task);
            if !children.is_empty() {
                if y >= inner.y + inner.height {
                    return hitboxes;
                }
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        " ── CHILDREN ──",
                        Style::default()
                            .fg(theme.muted)
                            .add_modifier(Modifier::BOLD),
                    ))),
                    Rect {
                        x: inner.x,
                        y,
                        width: inner.width,
                        height: 1,
                    },
                );
                y = y.saturating_add(1);
                for child in children.into_iter().take(MAX_COMPACT_AGENTS) {
                    if y >= inner.y + inner.height {
                        break;
                    }
                    let line = Line::from(vec![
                        Span::styled("  └ ", Style::default().fg(theme.muted)),
                        Span::styled(
                            crate::ui::sidepanel::agent_display_name(child),
                            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!(
                                "  {}",
                                child.activity.as_deref().unwrap_or(&child.description)
                            ),
                            Style::default().fg(theme.muted),
                        ),
                    ]);
                    let rows = Rect {
                        x: inner.x,
                        y,
                        width: inner.width,
                        height: 1,
                    };
                    hitboxes.push(SubtaskHitbox {
                        task_id: child.id,
                        area: rows,
                    });
                    f.render_widget(Paragraph::new(line), rows);
                    y = y.saturating_add(1);
                }
            }
        }
        None => {
            let mut shown = 0usize;
            for task in &tasks {
                if shown >= MAX_COMPACT_AGENTS || y >= inner.y + inner.height {
                    break;
                }
                let lines =
                    crate::ui::sidepanel::agent_lines(task, ms, inner.width as usize, theme);
                let remaining = (inner.y + inner.height).saturating_sub(y) as usize;
                if lines.len() > remaining {
                    break;
                }
                let rows = Rect {
                    x: inner.x,
                    y,
                    width: inner.width,
                    height: lines.len() as u16,
                };
                hitboxes.push(SubtaskHitbox {
                    task_id: task.id,
                    area: rows,
                });
                for line in lines {
                    f.render_widget(
                        Paragraph::new(line),
                        Rect {
                            x: inner.x,
                            y,
                            width: inner.width,
                            height: 1,
                        },
                    );
                    y = y.saturating_add(1);
                }
                shown += 1;
            }
            let hidden = tasks.len().saturating_sub(shown);
            if hidden > 0 && y < inner.y + inner.height {
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        format!("+ {} more agents", hidden),
                        Style::default().fg(theme.muted),
                    ))),
                    Rect {
                        x: inner.x,
                        y,
                        width: inner.width,
                        height: 1,
                    },
                );
            }
        }
    }
    hitboxes
}

/// Wrapped, indented text lines at the given width.
fn wrapped_indented(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let wrapped = wrap_words(paragraph, width.max(1));
        if wrapped.is_empty() {
            lines.push(String::new());
        } else {
            lines.extend(wrapped);
        }
    }
    lines
}

/// Full detail lines for one child agent: status chips, cwd, prompt, activity
/// log, and report.
fn detail_lines(task: &Subtask, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    use crate::app::state::{SubtaskLogEntry, SubtaskToolStatus};

    let mut out: Vec<Line<'static>> = Vec::new();

    let (state, color) = match task.status {
        SubtaskStatus::Running => ("RUNNING", theme.warning),
        SubtaskStatus::Completed => ("COMPLETED", theme.success),
        SubtaskStatus::Unresolved => ("UNRESOLVED", theme.warning),
        SubtaskStatus::Failed => ("FAILED", theme.danger),
    };
    let elapsed = crate::render::document::fmt_duration_ms(
        task.duration_ms
            .unwrap_or_else(|| task.started_at.elapsed().as_millis() as u64),
    );
    let mut status = vec![
        Span::styled(
            " STATUS ",
            Style::default()
                .bg(color)
                .fg(ratatui::style::Color::Black)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} ", state),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " DURATION ",
            Style::default()
                .bg(ratatui::style::Color::DarkGray)
                .fg(theme.text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {} ", elapsed), Style::default().fg(theme.text)),
    ];
    if let Some(todo) = task.todo_index {
        status.push(Span::styled(
            format!(" TASK {} ", todo),
            Style::default()
                .bg(ratatui::style::Color::DarkGray)
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        ));
    }
    out.push(Line::from(status));

    out.push(Line::from(vec![
        Span::styled(
            " CWD ",
            Style::default()
                .bg(ratatui::style::Color::DarkGray)
                .fg(theme.text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                " {}",
                crate::render::path::abbreviate_home(&task.cwd.to_string_lossy())
            ),
            Style::default().fg(theme.muted),
        ),
    ]));

    out.push(Line::from(Span::styled(
        " PROMPT ",
        Style::default()
            .bg(theme.accent)
            .fg(ratatui::style::Color::Black)
            .add_modifier(Modifier::BOLD),
    )));
    for text in wrapped_indented(&task.prompt, width.saturating_sub(3))
        .into_iter()
        .take(MAX_PROMPT_LINES)
    {
        out.push(Line::from(Span::styled(
            format!("  {}", text),
            Style::default().fg(theme.text),
        )));
    }

    if !task.log.is_empty() {
        out.push(Line::from(Span::styled(
            " ACTIVITY ",
            Style::default()
                .bg(theme.accent)
                .fg(ratatui::style::Color::Black)
                .add_modifier(Modifier::BOLD),
        )));
        for entry in &task.log {
            match entry {
                SubtaskLogEntry::Phase { text } => {
                    out.push(Line::from(Span::styled(
                        format!("  {}", text),
                        Style::default().fg(theme.muted),
                    )));
                }
                SubtaskLogEntry::Checklist {
                    done,
                    running,
                    pending,
                } => {
                    out.push(Line::from(Span::styled(
                        format!(
                            "  ✓ {} done · ◐ {} running · {} pending",
                            done, running, pending
                        ),
                        Style::default().fg(theme.warning),
                    )));
                }
                SubtaskLogEntry::Tool {
                    summary,
                    status: tool_status,
                    duration_ms,
                    output,
                    ..
                } => {
                    let (glyph, color) = match tool_status {
                        SubtaskToolStatus::Running => ("◐", theme.warning),
                        SubtaskToolStatus::Completed => ("●", theme.success),
                        SubtaskToolStatus::Failed => ("×", theme.danger),
                    };
                    let duration = duration_ms
                        .map(|millis| crate::render::document::fmt_duration_ms(millis))
                        .unwrap_or_default();
                    out.push(Line::from(vec![
                        Span::styled(format!("  {} ", glyph), Style::default().fg(color)),
                        Span::styled(summary.to_string(), Style::default().fg(theme.text)),
                        Span::styled(
                            if duration.is_empty() {
                                String::new()
                            } else {
                                format!("  {}", duration)
                            },
                            Style::default().fg(theme.muted),
                        ),
                    ]));
                    if let Some(text) = output {
                        let truncated: Vec<String> = text
                            .split('\n')
                            .take(MAX_TOOL_OUTPUT_LINES)
                            .map(str::to_string)
                            .collect();
                        for line in truncated {
                            let line = line.trim();
                            if line.is_empty() {
                                continue;
                            }
                            let mut wrapped = wrap_words(line, width.saturating_sub(6).max(1));
                            wrapped.truncate(MAX_TOOL_OUTPUT_LINES);
                            for text in wrapped {
                                out.push(Line::from(Span::styled(
                                    format!("      {}", text),
                                    Style::default().fg(theme.muted),
                                )));
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(report) = task
        .output
        .as_deref()
        .filter(|report| !report.trim().is_empty())
    {
        let unresolved = task.status == SubtaskStatus::Unresolved
            || crate::agent::subtask::is_unresolved_report(report);
        out.push(Line::from(Span::styled(
            if unresolved { " REVIEW UNRESOLVED " } else { " REVIEW " },
            Style::default()
                .bg(if unresolved { theme.warning } else { theme.accent })
                .fg(ratatui::style::Color::Black)
                .add_modifier(Modifier::BOLD),
        )));
        let report = report
            .trim()
            .strip_prefix("[agent-outcome:unresolved]")
            .unwrap_or(report.trim())
            .trim();
        if let Some(structured) = crate::agent::report::verification_report(report) {
            use crate::agent::report::FindingAnswer;
            for finding in structured.findings.iter().take(MAX_REPORT_LINES / 2) {
                let (glyph, color) = match finding.answer {
                    FindingAnswer::Yes => ("✓", theme.success),
                    FindingAnswer::No => ("×", theme.danger),
                    FindingAnswer::Mixed => ("◐", theme.warning),
                    FindingAnswer::Unknown => ("?", theme.muted),
                };
                out.push(Line::from(vec![
                    Span::styled(format!("  {} ", glyph), Style::default().fg(color)),
                    Span::styled(
                        finding.check_id.clone(),
                        Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {}", finding.support),
                        Style::default().fg(theme.muted),
                    ),
                ]));
                for text in wrap_words(&finding.statement, width.saturating_sub(6).max(1))
                    .into_iter()
                    .take(1)
                {
                    out.push(Line::from(Span::styled(
                        format!("    {}", text),
                        Style::default().fg(theme.text),
                    )));
                }
            }
            if !structured.unresolved.is_empty() {
                out.push(Line::from(vec![
                    Span::styled("  ? ", Style::default().fg(theme.warning)),
                    Span::styled(
                        format!("Unresolved: {}", structured.unresolved.join(", ")),
                        Style::default().fg(theme.warning),
                    ),
                ]));
            }
        } else {
            for text in wrapped_indented(report, width.saturating_sub(3))
                .into_iter()
                .take(MAX_REPORT_LINES)
            {
                out.push(Line::from(Span::styled(
                    format!("  {}", text),
                    Style::default().fg(theme.text),
                )));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{detail_lines, height};
    use crate::agent::ToolCall;
    use crate::app::state::{Subtask, SubtaskLogEntry, SubtaskStatus, SubtaskToolStatus};
    use crate::render::theme::Theme;

    fn subtask(status: SubtaskStatus, log: Vec<SubtaskLogEntry>) -> Subtask {
        Subtask {
            id: 1,
            session_id: 0,
            parent_id: None,
            call: ToolCall {
                name: "agent".into(),
                args: serde_json::json!({"agent_index": 1}),
                id: None,
            },
            description: "audit the auth flow".into(),
            todo_index: Some(2),
            prompt: "Check the auth flow end to end.".into(),
            cwd: std::path::PathBuf::from("/tmp/project"),
            status,
            activity: None,
            log,
            transcript: Vec::new(),
            output: Some("Everything looks fine.".into()),
            message_index: 0,
            started_at: std::time::Instant::now(),
            duration_ms: Some(1_234),
            abort: None,
            agent: Some("docs".into()),
        }
    }

    #[test]
    fn detail_lines_include_status_duration_and_activity() {
        let task = subtask(
            SubtaskStatus::Running,
            vec![
                SubtaskLogEntry::Phase {
                    text: "scanning".into(),
                },
                SubtaskLogEntry::Tool {
                    name: "read".into(),
                    summary: "read(src/main.rs)".into(),
                    status: SubtaskToolStatus::Running,
                    duration_ms: None,
                    call: None,
                    output: None,
                },
            ],
        );
        let lines = detail_lines(&task, 60, &Theme::default());
        let text: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        assert!(text.iter().any(|line| line.contains("RUNNING")));
        assert!(text.iter().any(|line| line.contains("1.2s")));
        assert!(text.iter().any(|line| line.contains("scanning")));
        assert!(text.iter().any(|line| line.contains("read(src/main.rs)")));
        assert!(text.iter().any(|line| line.contains("TASK 2")));
        assert!(text.iter().any(|line| line.contains("/tmp/project")));
        assert!(text
            .iter()
            .any(|line| line.contains("Everything looks fine.")));
    }

    #[test]
    fn completed_tool_entries_show_duration_and_output() {
        let task = subtask(
            SubtaskStatus::Running,
            vec![SubtaskLogEntry::Tool {
                name: "shell".into(),
                summary: "run_shell(cargo test)".into(),
                status: SubtaskToolStatus::Completed,
                duration_ms: Some(2_000),
                call: None,
                output: Some("test result: ok".into()),
            }],
        );
        let lines = detail_lines(&task, 60, &Theme::default());
        let text: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        assert!(text.iter().any(|line| line.contains("2.0s")));
        assert!(text.iter().any(|line| line.contains("test result: ok")));
    }

    #[test]
    fn unresolved_detail_uses_review_card_label_and_hides_marker() {
        let mut task = subtask(SubtaskStatus::Unresolved, Vec::new());
        task.output = Some(crate::agent::subtask::unresolved_report(
            "No tool output found for function call call_private",
        ));
        let lines = detail_lines(&task, 70, &Theme::default());
        let text: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        assert!(text.iter().any(|line| line.contains("UNRESOLVED")));
        assert!(text.iter().any(|line| line.contains("REVIEW UNRESOLVED")));
        assert!(text.iter().any(|line| line.contains("required tool result")));
        assert!(!text.iter().any(|line| line.contains("call_private")));
        assert!(!text.iter().any(|line| line.contains("agent-outcome")));
    }

    #[test]
    fn structured_report_detail_uses_finding_and_unresolved_icons() {
        let mut task = subtask(SubtaskStatus::Unresolved, Vec::new());
        task.output = Some(concat!(
            "{\"schema\":\"aitui.verification-summary.v1\",\"status\":\"partially_verified\",",
            "\"findings\":[{\"check_id\":\"latency\",\"answer\":\"yes\",",
            "\"statement\":\"Avoidable waits exist.\",\"support\":\"2/2 replicas\",",
            "\"evidence\":[]}],\"unresolved\":[\"access\"],\"diagnostics\":[]}"
        ).into());
        let lines = detail_lines(&task, 80, &Theme::default());
        let text: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        assert!(text.iter().any(|line| line.contains("✓ latency")));
        assert!(text.iter().any(|line| line.contains("2/2 replicas")));
        assert!(text.iter().any(|line| line.contains("Unresolved: access")));
        assert!(!text
            .iter()
            .any(|line| line.contains("aitui.verification-summary.v1")));
    }

    #[test]
    fn detail_lines_are_width_bounded() {
        let task = subtask(SubtaskStatus::Completed, Vec::new());
        let lines = detail_lines(&task, 60, &Theme::default());
        use unicode_width::UnicodeWidthStr;
        for line in lines {
            assert!(UnicodeWidthStr::width(line.to_string().as_str()) <= 60);
        }
    }

    #[test]
    fn height_is_zero_without_agents() {
        let app = crate::app::state::App::new(crate::config::Config::default()).unwrap();
        assert_eq!(height(&app, 60), 0);
    }
}
