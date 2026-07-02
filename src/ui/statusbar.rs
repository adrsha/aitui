use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::state::App;
use crate::render::theme::Theme;

/// Braille spinner frames, advanced by wall-clock time so the "working"
/// indicator animates smoothly while the loop redraws (~60fps).
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn spinner_frame() -> &'static str {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    SPINNER[((ms / 90) as usize) % SPINNER.len()]
}

/// A solid background "chip": ` text ` with a coloured background and a readable
/// foreground, so each status reads as a distinct badge rather than dim text.
fn chip(text: impl Into<String>, bg: Color) -> Span<'static> {
    Span::styled(
        format!(" {} ", text.into()),
        Style::default()
            .bg(bg)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    )
}

pub fn render(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let session = app.sessions.active();
    let agent_mode = session.agent_mode;
    // Spinner reflects *any* session generating (parallel streams), not just the
    // active one, so a background turn still shows activity.
    let busy = app.any_busy();

    let sess_label = format!(
        "{}/{} {}",
        app.sessions.active_idx() + 1,
        app.sessions.all().len(),
        session.name,
    );

    let model = app.current_model();
    let status = app.status.as_deref().unwrap_or("");

    let mut left: Vec<Span<'static>> = Vec::new();

    // Vim mode as the leading background chip — each mode gets its own colour.
    use crate::input::vim::VimMode;
    let (mode_label, mode_bg) = match app.vim {
        VimMode::Normal => ("NORMAL", Color::Blue),
        VimMode::Insert => ("INSERT", Color::Green),
        VimMode::Visual if app.input.visual_line => ("V-LINE", Color::Magenta),
        VimMode::Visual => ("VISUAL", Color::Magenta),
        VimMode::Operator(_) => ("OP", Color::Cyan),
    };
    left.push(chip(mode_label, mode_bg));
    left.push(Span::raw(" "));

    // Busy indicator, as an animated spinner chip (no "generating" word).
    if busy {
        left.push(chip(format!("{} working", spinner_frame()), theme.accent));
        left.push(Span::raw(" "));
    }

    // Session badge (low-key chip): white on the faint bg so it stays readable.
    left.push(Span::styled(
        format!(" {} ", sess_label),
        Style::default()
            .bg(theme.faint)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));

    let mut chips: Vec<(String, Color)> = Vec::new();
    if agent_mode {
        chips.push(("agent".into(), theme.warning));
    }
    if app.show_output {
        chips.push(("output".into(), theme.accent));
    }
    if !app.edited_files.is_empty() {
        chips.push((format!("✎{}", app.edited_files.len()), theme.success));
    }
    let active_skills = app.skills.iter().filter(|s| s.active).count();
    if active_skills > 0 {
        chips.push((format!("✦{}", active_skills), theme.link));
    }
    if let Some(effort) = app.reasoning_effort.as_deref() {
        chips.push((format!("🧠{}", effort), theme.warning));
    }
    for (text, bg) in chips {
        left.push(Span::raw(" "));
        left.push(chip(text, bg));
    }

    // Free-text status message (plain, follows terminal fg).
    if !status.is_empty() {
        left.push(Span::raw("  "));
        left.push(Span::styled(
            status.to_string(),
            Style::default().fg(theme.muted),
        ));
    }

    // Right side: model chip. While the list loads it animates a spinner; if the
    // fetch failed it shows a warning; otherwise the selected model name.
    use crate::app::state::ModelLoad;
    let (right, right_bg) = match app.model_load {
        ModelLoad::Loading => (
            format!(" {} loading models ", spinner_frame()),
            theme.warning,
        ),
        ModelLoad::Failed => (" ⚠ models unavailable ".to_string(), theme.danger),
        ModelLoad::Loaded => (format!(" {} ", model), theme.accent),
    };
    let left_len: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let right_len = right.chars().count();
    let pad = area.width.saturating_sub((left_len + right_len) as u16) as usize;

    let mut spans = left;
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(
        right,
        Style::default()
            .bg(right_bg)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    ));

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
