pub mod chat;
pub mod help;
pub mod input;
pub mod layout;
pub mod overlay;
pub mod scrollbar;
pub mod statusbar;
pub mod todo;

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::app::state::{App, PanelLayout};
use crate::render::theme::Theme;

/// Input box auto-sizes to its content: one row per logical line, at least one row
/// so it's always visible, and at most `config.ui.input_height` rows so a huge
/// paste can't crowd out the transcript (that cap is what `:resize` adjusts).
pub fn render(f: &mut Frame, app: &mut App) {
    let max_rows = app.config.ui.input_height.max(1);
    let input_rows = (app.input.lines.len() as u16).clamp(1, max_rows);
    let todo_count = app.sessions.active().todos.len();
    let todo_h = todo::height(todo_count);
    let lay = layout::compute(f.area(), input_rows, todo_h);

    // Reserve the rightmost column of the transcript for a scrollbar; the text
    // area (used for wrapping + click hit-testing) is everything left of it.
    let (chat_area, scroll_area) = split_scrollbar(lay.chat);
    app.layout = PanelLayout { chat: chat_area };

    // Rebuild the (cached) chat document for the current width/height if stale,
    // then draw everything.
    app.sync_chat_doc(chat_area.width as usize, chat_area.height as usize);

    let theme = app.theme();

    chat::render(f, app, chat_area, &theme);
    scrollbar::render(f, app, scroll_area, &theme);
    render_tokens(f, app, chat_area, &theme);
    render_jump_pill(f, app, chat_area, &theme);
    statusbar::render_activity(f, app, lay.activity, &theme);
    todo::render(f, app, lay.todo, &theme);
    input::render(f, app, lay.input, &theme);
    statusbar::render(f, app, lay.statusbar, &theme);

    // Dim the whole UI behind a modal so the overlay stands out. The overlay's
    // own `Clear` resets its cells back to full brightness, so only the backdrop
    // dims. Uses the ANSI DIM attribute (no custom colour) per the terminal-only
    // theme rule.
    if app.show_help || app.overlay.is_active() {
        dim_area(f, f.area());
    }

    if app.show_help {
        help::render(f, app, &theme);
    }

    overlay::render(f, app, &theme);

    // Display pending image, then clear it (one-shot). Only emit the Kitty
    // graphics escape sequence on terminals that understand it; otherwise those
    // bytes render as garbage. On unsupported terminals show nothing at all.
    if let Some(ref path) = app.pending_image.take() {
        if crate::render::image::supports_kitty() {
            let col = chat_area.x + 2;
            let row = chat_area.y + 3;
            let cols = chat_area.width.saturating_sub(4).max(4);
            let rows = cols / 2;
            let _ = crate::render::image::display_image(path, col, row, cols, rows);
        }
    }

    // Flush any queued clipboard copy via OSC 52 (one-shot). Kept here so all raw
    // terminal writes live in the render layer, like the image protocol above.
    if let Some(text) = app.pending_clipboard.take() {
        crate::app::clipboard::copy(&text);
    }
}

/// Fade everything already drawn in `area` by adding the ANSI DIM attribute and
/// dropping BOLD, so a modal on top reads as the focused layer.
fn dim_area(f: &mut Frame, area: Rect) {
    use ratatui::style::Modifier;
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            cell.modifier.insert(Modifier::DIM);
            cell.modifier.remove(Modifier::BOLD);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::context_gauge;

    #[test]
    fn gauge_fills_proportionally_and_clamps() {
        assert_eq!(context_gauge(0, 100, 4), "▱▱▱▱ 0%");
        assert_eq!(context_gauge(50, 100, 4), "▰▰▱▱ 50%");
        assert_eq!(context_gauge(100, 100, 4), "▰▰▰▰ 100%");
        // Over budget clamps to full, not past it.
        assert_eq!(context_gauge(300, 100, 4), "▰▰▰▰ 100%");
        // Zero window is safe (no divide-by-zero).
        assert_eq!(context_gauge(10, 0, 4), "▱▱▱▱ 0%");
    }
}

/// Split the transcript rect into (text area, 2-column scrollbar on the right).
fn split_scrollbar(chat: Rect) -> (Rect, Rect) {
    if chat.width < 4 {
        return (
            chat,
            Rect {
                x: chat.x,
                y: chat.y,
                width: 0,
                height: chat.height,
            },
        );
    }
    let bar_w = 2;
    let text = Rect {
        x: chat.x,
        y: chat.y,
        width: chat.width - bar_w,
        height: chat.height,
    };
    let bar = Rect {
        x: chat.x + chat.width - bar_w,
        y: chat.y,
        width: bar_w,
        height: chat.height,
    };
    (text, bar)
}

/// When the transcript is scrolled up off the tail, draw a small "jump to bottom"
/// pill in the chat pane's bottom-right showing how many rows are hidden below.
/// Pressing the scroll-to-bottom key (or sending) returns to the live tail.
fn render_jump_pill(f: &mut Frame, app: &App, chat: Rect, theme: &Theme) {
    let hidden = app.chat.rows_below(chat.height as usize);
    if hidden == 0 || chat.height == 0 {
        return;
    }
    let label = format!(
        " ↓ {} below · {} ",
        hidden,
        app.keymap.scroll_bottom.label()
    );
    let w = (label.chars().count() as u16).min(chat.width);
    if w == 0 {
        return;
    }
    let area = Rect {
        x: chat.x + chat.width.saturating_sub(w),
        y: chat.y + chat.height - 1,
        width: w,
        height: 1,
    };
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            label,
            Style::default()
                .bg(theme.accent)
                .fg(ratatui::style::Color::Black)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ))),
        area,
    );
}

/// A compact filled/empty bar plus percentage showing how much of the model's
/// context window the prompt currently occupies, e.g. `▰▰▰▱▱▱▱▱ 38%`. `cells` is
/// the bar width. Clamps to 100% so an over-budget prompt reads as full, not
/// overflowing. Pure so the fill math is unit-testable.
fn context_gauge(used: u32, window: u32, cells: usize) -> String {
    let pct = if window == 0 {
        0
    } else {
        ((used as u64 * 100) / window as u64).min(100) as u32
    };
    let filled = (pct as usize * cells) / 100;
    let bar: String = (0..cells)
        .map(|i| if i < filled { '▰' } else { '▱' })
        .collect();
    format!("{} {}%", bar, pct)
}

/// Draw the token counter for the last response in the chat pane's top-right
/// corner (overlaid, like a context gauge). No-op until the endpoint reports it.
fn render_tokens(f: &mut Frame, app: &App, chat: Rect, theme: &Theme) {
    let Some(u) = app.usage else { return };
    if chat.width < 12 || chat.height == 0 {
        return;
    }
    let gauge = context_gauge(u.prompt_tokens, app.config.ui.context_window, 8);
    let label = format!(
        " ↑{} ↓{} · {} tok · {} ",
        u.prompt_tokens, u.completion_tokens, u.total_tokens, gauge
    );
    let w = (label.chars().count() as u16).min(chat.width);
    let area = Rect {
        x: chat.x + chat.width - w,
        y: chat.y,
        width: w,
        height: 1,
    };
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            label,
            Style::default().fg(theme.faint),
        ))),
        area,
    );
}
