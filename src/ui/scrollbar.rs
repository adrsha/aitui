use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::Frame;

use crate::app::state::App;
use crate::render::theme::Theme;

/// A single-column map of the whole transcript: a **blue** vertical line for your
/// prompts (user turns) and a **green** vertical line for everything else (model
/// replies + tool output). Nothing else — no thumb, no track, no per-turn pips.
pub fn render(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let h = area.height as usize;
    let doc = app.chat.doc();
    let total = doc.len();
    if total == 0 {
        return;
    }

    // Per-row role: `role_start` only tags a message's first row, so carry the last
    // seen role forward to every row it owns.
    let mut roles: Vec<&str> = Vec::with_capacity(total);
    let mut current = "assistant";
    for row in doc.iter() {
        if let Some(r) = row.role_start {
            current = r;
        }
        roles.push(current);
    }

    // Hug the transcript's right edge.
    let x = area.x + area.width.saturating_sub(1);
    let blue = theme.gutter_user; // your prompts
    let green = theme.gutter_tool; // model + tool output

    for i in 0..h {
        // Which transcript row maps to this bar cell.
        let row_idx = (i * total) / h;
        let is_prompt = roles.get(row_idx).is_some_and(|r| *r == "user");
        let color = if is_prompt { blue } else { green };
        f.render_widget(
            ratatui::widgets::Paragraph::new(Span::styled(
                "│",
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )),
            Rect {
                x,
                y: area.y + i as u16,
                width: 1,
                height: 1,
            },
        );
    }
}
