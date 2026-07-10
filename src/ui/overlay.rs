use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Padding, Paragraph};
use ratatui::Frame;

use crate::app::overlay::{
    ApiSetup, BrowsePurpose, DecisionRequest, FileBrowser, Overlay, Picker, PickerKind,
    PlanRequest, Settings, SettingsRow, Startup, ToolRequest,
};
use crate::app::state::App;
use crate::render::theme::Theme;

pub fn render(f: &mut Frame, app: &App, theme: &Theme) {
    match &app.overlay {
        Overlay::None => {}
        Overlay::Startup(s) => render_startup(f, app, s, theme),
        Overlay::Picker(p) => render_picker(f, app, p, theme),
        Overlay::Browser(b) => render_browser(f, b, theme),
        Overlay::Palette(p) => render_palette(f, p, theme),
        Overlay::Settings(s) => render_settings(f, app, s, theme),
        Overlay::Permission(p) => render_permission(f, p, theme),
        Overlay::Decision(r) => render_decision(f, r, theme),
        Overlay::Plan(r) => render_plan(f, r, theme),
        Overlay::ToolRequest(r) => render_tool_request(f, r, theme),
        Overlay::ApiSetup(s) => render_api_setup(f, s, theme),
        Overlay::Notice { title, body } => render_notice(f, title, body, theme),
    }
}

/// A small centered informational dialog: title + wrapped body + a dismiss hint.
fn render_notice(f: &mut Frame, title: &str, body: &str, theme: &Theme) {
    let area = centered(46, 34, f.area());
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

fn render_browser(f: &mut Frame, b: &FileBrowser, theme: &Theme) {
    let area = centered(60, 70, f.area());
    let title = match b.purpose {
        BrowsePurpose::Attach => " Attach File ",
        BrowsePurpose::Edit => " Open in $EDITOR ",
    };
    let inner = panel(f, area, title, theme);

    // Current directory header.
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" 📂 {}", b.dir.display()),
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
            let glyph = if e.is_dir { "📁 " } else { "📄 " };
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

// ── Launch screen (resume a session or start new) ──────────────────────────────

fn render_startup(f: &mut Frame, app: &App, s: &Startup, theme: &Theme) {
    let area = centered(70, 70, f.area());
    let inner = panel(f, area, " AiTUI — Resume or Start ", theme);

    // "New session" row first, then one row per resumable session.
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let mut items: Vec<ListItem> = Vec::with_capacity(s.sessions + 1);
    let new_style = if s.selected == 0 {
        theme.selection()
    } else {
        Style::default().fg(theme.text)
    };
    items.push(ListItem::new(Line::from(vec![
        Span::styled("  ＋  Start a new session", new_style),
        Span::styled(format!("   {}", cwd), Style::default().fg(theme.muted)),
    ])));

    for (i, sess) in app.sessions.all().iter().enumerate() {
        let selected = s.selected == i + 1;
        let style = if selected {
            theme.selection()
        } else {
            Style::default().fg(theme.text)
        };
        let dir = sess
            .cwd
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "—".to_string());
        let meta = format!("   {} · {} msg", dir, sess.messages.len());
        items.push(ListItem::new(Line::from(vec![
            Span::styled(format!("  ≡  {}", sess.name), style),
            Span::styled(meta, Style::default().fg(theme.muted)),
        ])));
    }

    let list_h = inner.height.saturating_sub(2);
    f.render_widget(
        List::new(items),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: list_h,
        },
    );

    let hint = " j/k move · ⏎/l open · n new · Esc resume current";
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

fn render_picker(f: &mut Frame, _app: &App, picker: &Picker, theme: &Theme) {
    let area = centered(70, 70, f.area());
    let title = match picker.kind {
        PickerKind::Model => " Model Picker ",
        PickerKind::Session => " Sessions ",
        PickerKind::Skill => " Skills ",
    };
    let inner = panel(f, area, title, theme);

    // Search bar
    let search = format!(" 🔍 {}", picker.query);
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

    // Items
    let list_area = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width,
        height: inner.height.saturating_sub(4),
    };
    let items: Vec<ListItem> = picker
        .filtered
        .iter()
        .enumerate()
        .map(|(i, &idx)| {
            let item = &picker.items[idx];
            let style = if i == picker.selected {
                theme.selection()
            } else {
                Style::default().fg(theme.text)
            };
            ListItem::new(Line::from(Span::styled(format!(" {}", item), style)))
        })
        .collect();
    f.render_widget(List::new(items), list_area);

    let hint = match picker.kind {
        PickerKind::Session => " j/k move · ⏎/l open · n new · d delete · r rename · type search · Esc close ",
        PickerKind::Model | PickerKind::Skill => " ↑↓ move · ⏎ select · type search · Esc close ",
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

// ── Command palette ───────────────────────────────────────────────────────────

fn render_palette(f: &mut Frame, palette: &crate::app::overlay::Palette, theme: &Theme) {
    let area = centered(50, 30, f.area());
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
    let start = palette
        .selected
        .saturating_sub(visible.saturating_sub(1));
    let end = (start + visible).min(palette.filtered.len());
    let commands = crate::app::overlay::slash_commands();
    let items: Vec<ListItem> = palette
        .filtered
        .iter()
        .enumerate()
        .skip(start)
        .take(end.saturating_sub(start))
        .map(|(i, &idx)| {
            let cmd = &commands[idx];
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

fn render_settings(f: &mut Frame, app: &App, settings: &Settings, theme: &Theme) {
    let area = centered(50, 16, f.area());
    let inner = panel(f, area, " Settings ", theme);

    let rows = SettingsRow::all();
    for (i, row) in rows.iter().enumerate() {
        let y = inner.y + i as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let selected = i == settings.selected;
        let label = match row {
            SettingsRow::AgentDefault => format!(
                "  Agent by default: {}",
                if app.config.ui.agent_default {
                    "ON"
                } else {
                    "OFF"
                }
            ),
            SettingsRow::AutoApprove => format!(
                "  Auto-approve reads: {}",
                if app.config.ui.auto_approve_reads {
                    "ON"
                } else {
                    "OFF"
                }
            ),
            SettingsRow::InputHeight => format!("  Input height: {}", app.config.ui.input_height),
            SettingsRow::SystemPrompt => {
                if settings.editing_prompt {
                    format!("  System prompt: {}", settings.prompt_buf)
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

fn render_permission(f: &mut Frame, req: &crate::app::overlay::PermissionRequest, theme: &Theme) {
    // A tall, wide modal so a whole batch of commands is visible (scroll for more).
    let area = centered_fixed(100, f.area().height.saturating_sub(2), f.area());
    let inner = panel(f, area, " Access Request ", theme);
    let cwd = std::env::current_dir().unwrap_or_default();

    // ── Layout: title (1) · blank · command list (flex) · blank · options (8) · legend (1)
    const OPTIONS: u16 = 8;
    let list_h = inner.height.saturating_sub(2 + OPTIONS + 2).max(3);

    // ── Command list (scrollable, syntax-highlighted, all fields) ───────────────
    let all_lines = command_lines(&req.calls, theme, &cwd, inner.width as usize);
    let total = all_lines.len();
    let max_start = total.saturating_sub(list_h as usize);
    let start = req.scroll.min(max_start);
    let end = (start + list_h as usize).min(total);

    let count = req.calls.len();
    let title = if total > list_h as usize {
        format!(
            "Assistant wants to run {} call{} — lines {}–{} of {}",
            count,
            if count == 1 { "" } else { "s" },
            start + 1,
            end,
            total
        )
    } else {
        format!(
            "Assistant wants to run {} call{}:",
            count,
            if count == 1 { "" } else { "s" }
        )
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        ))),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
    );

    f.render_widget(
        Paragraph::new(all_lines[start..end].to_vec()),
        Rect {
            x: inner.x,
            y: inner.y + 2,
            width: inner.width,
            height: list_h,
        },
    );
    // Scroll affordances at the list's edges.
    if start > 0 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("▲ more (PgUp)", theme.subtle()))),
            Rect {
                x: inner.x + inner.width.saturating_sub(14),
                y: inner.y + 2,
                width: 14,
                height: 1,
            },
        );
    }
    if end < total {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("▼ more (PgDn)", theme.subtle()))),
            Rect {
                x: inner.x + inner.width.saturating_sub(14),
                y: inner.y + 1 + list_h,
                width: 14,
                height: 1,
            },
        );
    }

    // ── Allow/deny options ──────────────────────────────────────────────────────
    let first = req.calls.first();
    let kind_name = first
        .and_then(|c| c.kind())
        .map(|k| k.name())
        .unwrap_or("this tool");
    let dir = first
        .and_then(|c| c.permission_directory(&cwd))
        .map(|d| {
            d.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| d.display().to_string())
        })
        .unwrap_or_else(|| "this dir".to_string());
    let options = [
        "(a) Allow once / allow all listed".to_string(),
        format!("    Allow all  {}  (this session)", kind_name),
        format!("    Allow all in  {}/  (this session)", dir),
        "    Allow everything (this session)".to_string(),
        "(d) Deny once / deny all listed".to_string(),
        format!("    Deny all  {}  (this session)", kind_name),
        format!("    Deny all in  {}/  (this session)", dir),
        "    Deny everything (this session)".to_string(),
    ];
    let start_y = inner.y + 3 + list_h;
    for (i, opt) in options.iter().enumerate() {
        let y = start_y + i as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let style = if i == req.selected {
            theme.selection()
        } else {
            Style::default().fg(theme.text)
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(format!("  {}", opt), style))),
            Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 1,
            },
        );
    }

    // ── Shortcut legend (always visible) ────────────────────────────────────────
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "↑↓ option · PgUp/PgDn scroll · a allow · d deny · e edit · p set policy · ⏎ run · Esc cancel",
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

/// Build the flat, styled, scrollable line list for a permission batch: a bold
/// header per call plus every editable field, with shell commands / file content
/// syntax-highlighted so the user can read exactly what will run.
fn command_lines(
    calls: &[crate::agent::ToolCall],
    theme: &Theme,
    cwd: &std::path::Path,
    width: usize,
) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    for (i, call) in calls.iter().enumerate() {
        let kind = call.kind();
        let icon = kind.map(|k| k.icon()).unwrap_or("⚙");
        let risk = kind.map(|k| k.risk().label()).unwrap_or("UNKNOWN");
        let name = kind
            .map(|k| k.name())
            .unwrap_or(call.name.as_str())
            .to_string();
        let scope = call
            .permission_directory(cwd)
            .map(|d| d.display().to_string())
            .unwrap_or_else(|| "network / external".to_string());
        out.push(Line::from(vec![
            Span::styled(
                format!("▸ {} {}. {}", icon, i + 1, name),
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("   risk: {} · {}", risk, scope), theme.subtle()),
        ]));
        for key in call.editable_arg_keys() {
            let Some(val) = call.get_arg(key) else {
                continue;
            };
            out.push(Line::from(Span::styled(
                format!("    {}:", key),
                theme.subtle(),
            )));
            let lang = value_lang(call, key);
            out.extend(value_lines(&lang, val, 6, width, theme));
        }
        out.push(Line::from(String::new()));
    }
    out
}

/// The highlight language for an editable field: shell commands as bash, an
/// edit/write's body by the target file's extension, everything else plain.
fn value_lang(call: &crate::agent::ToolCall, key: &str) -> String {
    match key {
        "command" => "bash".to_string(),
        "content" | "old" | "new" => call
            .get_arg("path")
            .and_then(|p| p.rsplit('.').next())
            .map(|e| e.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Render a field value (possibly multi-line) as indented, width-clipped, syntax-
/// highlighted lines. Falls back to a plain accent colour when `lang` has no
/// grammar. Each source line becomes one row (horizontal overflow is clipped —
/// `e` opens the full text in `$EDITOR`).
fn value_lines(
    lang: &str,
    value: &str,
    indent: usize,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let pad = " ".repeat(indent);
    let avail = width.saturating_sub(indent).max(4);
    let hl = if lang.is_empty() {
        None
    } else {
        crate::render::highlight::highlight(value, lang, theme)
    };
    match hl {
        Some(hl_lines) => hl_lines
            .into_iter()
            .map(|segs| {
                let mut spans = vec![Span::raw(pad.clone())];
                let mut used = 0usize;
                for (text, style) in segs {
                    if used >= avail {
                        break;
                    }
                    let take = avail - used;
                    let clipped: String = text.chars().take(take).collect();
                    used += clipped.chars().count();
                    spans.push(Span::styled(clipped, style));
                }
                Line::from(spans)
            })
            .collect(),
        None => value
            .split('\n')
            .map(|src| {
                let clipped: String = if src.chars().count() > avail {
                    src.chars()
                        .take(avail.saturating_sub(1))
                        .collect::<String>()
                        + "…"
                } else {
                    src.to_string()
                };
                Line::from(vec![
                    Span::raw(pad.clone()),
                    Span::styled(clipped, Style::default().fg(theme.accent)),
                ])
            })
            .collect(),
    }
}

// ── Decision / plan prompts ──────────────────────────────────────────────────

fn render_decision(f: &mut Frame, req: &DecisionRequest, theme: &Theme) {
    let rows = req.options.len().min(8) as u16;
    let area = centered_fixed(70, 8 + rows, f.area());
    let inner = panel(f, area, " Decision Request ", theme);

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
            height: 2,
        },
    );
    for (i, option) in req.options.iter().take(8).enumerate() {
        let mark = if req.multi {
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
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {} {}", mark, option),
                style,
            ))),
            Rect {
                x: inner.x,
                y: inner.y + 3 + i as u16,
                width: inner.width,
                height: 1,
            },
        );
    }
    let hint = if req.multi {
        " ↑↓ move · space toggle · ⏎ confirm · Esc cancel"
    } else {
        " ↑↓ move · ⏎ choose · Esc cancel"
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

fn render_plan(f: &mut Frame, req: &PlanRequest, theme: &Theme) {
    let area = centered_fixed(70, 9, f.area());
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
                req.path.display().to_string(),
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

fn render_tool_request(f: &mut Frame, req: &ToolRequest, theme: &Theme) {
    let area = centered_fixed(66, 10, f.area());
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

fn render_api_setup(f: &mut Frame, s: &ApiSetup, theme: &Theme) {
    let area = centered_fixed(64, 9, f.area());
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

/// A padded, bordered overlay panel with `title`. Returns the inner content rect
/// after rendering the Clear + framed block.
fn panel(f: &mut Frame, area: Rect, title: &str, _theme: &Theme) -> Rect {
    // Clear occludes (and un-dims) the transcript behind the overlay — cells reset
    // to the terminal's default bg. A rounded border + bold title makes the modal
    // pop against the dimmed backdrop. Border uses the terminal's own fg (no custom
    // colour), so it still follows the terminal's light/dark theme.
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(Span::styled(
            title.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .padding(Padding::uniform(1));
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

/// A centered box of a fixed width/height (clamped to the screen). Used for
/// dialogs whose content is a known number of lines, so borders + padding never
/// clip the body (percentage sizing made the permission prompt collapse).
fn centered_fixed(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

fn centered(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((area.height * (100 - percent_y)) / 200),
            Constraint::Length((area.height * percent_y) / 100),
            Constraint::Length((area.height * (100 - percent_y)) / 200),
        ])
        .split(area)[1];
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length((area.width * (100 - percent_x)) / 200),
            Constraint::Length((area.width * percent_x) / 100),
            Constraint::Length((area.width * (100 - percent_x)) / 200),
        ])
        .split(popup)[1]
}
