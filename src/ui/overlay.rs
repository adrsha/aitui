use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Padding, Paragraph};
use ratatui::Frame;

use crate::app::overlay::{ApiSetup, BrowsePurpose, FileBrowser, Overlay, Picker, PickerKind, Settings, SettingsRow, Startup};
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
        Style::default().fg(theme.faint).add_modifier(Modifier::ITALIC),
    )));
    f.render_widget(Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: true }), inner);
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
        Paragraph::new(Line::from(Span::styled(format!(" 📂 {}", b.dir.display()), Style::default().fg(theme.muted)))),
        Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 },
    );

    let list_h = inner.height.saturating_sub(2) as usize;
    // Keep the cursor visible.
    let start = if b.cursor >= list_h { b.cursor + 1 - list_h } else { 0 };
    let end = (start + list_h).min(b.entries.len());

    let items: Vec<ListItem> = b.entries[start..end].iter().enumerate().map(|(i, e)| {
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
        ListItem::new(Line::from(Span::styled(format!("{}{}{}", mark, glyph, e.name), style)))
    }).collect();
    f.render_widget(
        List::new(items),
        Rect { x: inner.x, y: inner.y + 1, width: inner.width, height: list_h as u16 },
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
        Paragraph::new(Line::from(Span::styled(hint, Style::default().fg(theme.faint)))),
        Rect { x: inner.x, y: inner.y + inner.height - 1, width: inner.width, height: 1 },
    );
}

// ── Launch screen (resume a session or start new) ──────────────────────────────

fn render_startup(f: &mut Frame, app: &App, s: &Startup, theme: &Theme) {
    let area = centered(70, 70, f.area());
    let inner = panel(f, area, " AiTUI — Resume or Start ", theme);

    // "New session" row first, then one row per resumable session.
    let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
    let mut items: Vec<ListItem> = Vec::with_capacity(s.sessions + 1);
    let new_style = if s.selected == 0 { theme.selection() } else { Style::default().fg(theme.text) };
    items.push(ListItem::new(Line::from(vec![
        Span::styled("  ＋  Start a new session", new_style),
        Span::styled(format!("   {}", cwd), Style::default().fg(theme.faint)),
    ])));

    for (i, sess) in app.sessions.all().iter().enumerate() {
        let selected = s.selected == i + 1;
        let style = if selected { theme.selection() } else { Style::default().fg(theme.text) };
        let dir = sess
            .cwd
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "—".to_string());
        let meta = format!("   {} · {} msg", dir, sess.messages.len());
        items.push(ListItem::new(Line::from(vec![
            Span::styled(format!("  ≡  {}", sess.name), style),
            Span::styled(meta, Style::default().fg(theme.faint)),
        ])));
    }

    let list_h = inner.height.saturating_sub(2);
    f.render_widget(
        List::new(items),
        Rect { x: inner.x, y: inner.y, width: inner.width, height: list_h },
    );

    let hint = " j/k move · ⏎/l open · n new · Esc resume current";
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(hint, Style::default().fg(theme.faint)))),
        Rect { x: inner.x, y: inner.y + inner.height - 1, width: inner.width, height: 1 },
    );
}

// ── Picker (models / sessions) ────────────────────────────────────────────────

fn render_picker(f: &mut Frame, _app: &App, picker: &Picker, theme: &Theme) {
    let area = centered(60, 60, f.area());
    let title = match picker.kind {
        PickerKind::Model => " Model Picker ",
        PickerKind::Session => " Sessions ",
        PickerKind::Skill => " Skills ",
    };
    let inner = panel(f, area, title, theme);

    // Search bar
    let search = format!(" 🔍 {}", picker.query);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(search, Style::default().fg(theme.text)))),
        Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 },
    );

    // Items
    let list_area = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width,
        height: inner.height.saturating_sub(3),
    };
    let items: Vec<ListItem> = picker.filtered.iter().enumerate().map(|(i, &idx)| {
        let item = &picker.items[idx];
        let style = if i == picker.selected {
            theme.selection()
        } else {
            Style::default().fg(theme.text)
        };
        ListItem::new(Line::from(Span::styled(format!(" {}", item), style)))
    }).collect();
    f.render_widget(List::new(items), list_area);
}

// ── Command palette ───────────────────────────────────────────────────────────

fn render_palette(f: &mut Frame, palette: &crate::app::overlay::Palette, theme: &Theme) {
    let area = centered(50, 30, f.area());
    let inner = panel(f, area, " Command Palette ", theme);

    let search = format!(" / {}", palette.query);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(search, Style::default().fg(theme.text)))),
        Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 },
    );

    let list_area = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width,
        height: inner.height.saturating_sub(3),
    };
    let commands = crate::app::overlay::slash_commands();
    let items: Vec<ListItem> = palette.filtered.iter().enumerate().map(|(i, &idx)| {
        let cmd = &commands[idx];
        let style = if i == palette.selected {
            theme.selection()
        } else {
            Style::default().fg(theme.text)
        };
        let text = format!(" {}  {}  — {}", cmd.icon, cmd.name, cmd.desc);
        ListItem::new(Line::from(Span::styled(text, style)))
    }).collect();
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
            SettingsRow::AgentDefault => format!("  Agent by default: {}", if app.config.ui.agent_default { "ON" } else { "OFF" }),
            SettingsRow::AutoApprove => format!("  Auto-approve reads: {}", if app.config.ui.auto_approve_reads { "ON" } else { "OFF" }),
            SettingsRow::InputHeight => format!("  Input height: {}", app.config.ui.input_height),
            SettingsRow::SystemPrompt => {
                if settings.editing_prompt {
                    format!("  System prompt: {}", settings.prompt_buf)
                } else {
                    "  System prompt: edit".to_string()
                }
            }
        };
        let style = if selected { theme.selection() } else { Style::default().fg(theme.text) };
        f.render_widget(Paragraph::new(Line::from(Span::styled(label, style))), Rect {
            x: inner.x, y, width: inner.width, height: 1,
        });
    }
}

// ── Permission prompt ─────────────────────────────────────────────────────────

fn render_permission(f: &mut Frame, req: &crate::app::overlay::PermissionRequest, theme: &Theme) {
    // Fixed height: header + blank + 4 options = 6 content rows, + 4 chrome
    // (border + padding). Percentage sizing collapsed this under the new border.
    let area = centered_fixed(64, 10, f.area());
    let inner = panel(f, area, " Tool Permission ", theme);

    let icon = req.call.kind().map(|k| k.icon()).unwrap_or("⚙");
    let summary = req.call.summary();
    let kind_info = match req.call.kind() {
        Some(k) => format!(" [{} risk]", k.risk().label()),
        None => String::new(),
    };
    let header = format!(" {}  {}{}", icon, summary, kind_info);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(header, Style::default().fg(theme.warning).add_modifier(Modifier::BOLD)))),
        Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 },
    );

    let options = vec!["(a) Allow once", "(A) Allow all", "(d) Deny once", "(D) Deny all"];
    for (i, opt) in options.iter().enumerate() {
        let y = inner.y + 2 + i as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let style = if i == req.selected {
            theme.selection()
        } else {
            Style::default().fg(theme.text)
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(format!("    {}", opt), style))),
            Rect { x: inner.x, y, width: inner.width, height: 1 },
        );
    }
}

// ── API setup prompt ──────────────────────────────────────────────────────────

fn render_api_setup(f: &mut Frame, s: &ApiSetup, theme: &Theme) {
    let area = centered_fixed(64, 9, f.area());
    let inner = panel(f, area, " API Setup ", theme);

    let field = |focused: bool, label: &str, value: String| {
        // Focused field shows a block cursor; the key is masked.
        let marker = if focused { "▸ " } else { "  " };
        let val_style = if focused {
            Style::default().fg(theme.text).add_modifier(Modifier::UNDERLINED)
        } else {
            Style::default().fg(theme.muted)
        };
        let shown = if value.is_empty() { "—".to_string() } else { value };
        vec![
            Span::styled(format!("{}{}: ", marker, label), Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
            Span::styled(shown, val_style),
            if focused { Span::styled("▌", Style::default().fg(theme.accent)) } else { Span::raw("") },
        ]
    };
    let masked_key: String = if s.api_key.is_empty() { String::new() } else { "•".repeat(s.api_key.chars().count().min(24)) };

    let lines = vec![
        Line::from(field(s.field == 0, "URL", s.endpoint.clone())),
        Line::from(""),
        Line::from(field(s.field == 1, "Key", masked_key)),
        Line::from(""),
        Line::from(Span::styled(
            "Tab switch field · ⏎ save · Esc cancel",
            Style::default().fg(theme.faint).add_modifier(Modifier::ITALIC),
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
        .title(Span::styled(title.to_string(), Style::default().add_modifier(Modifier::BOLD)))
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
