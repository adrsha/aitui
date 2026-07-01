pub mod chat;
pub mod help;
pub mod input;
pub mod layout;
pub mod overlay;
pub mod scrollbar;
pub mod statusbar;

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::app::state::{App, PanelLayout};
use crate::render::theme::Theme;

pub fn render(f: &mut Frame, app: &mut App) {
    let lay = layout::compute(f.area(), app.config.ui.input_height);

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

/// Split the transcript rect into (text area, 1-column scrollbar on the right).
fn split_scrollbar(chat: Rect) -> (Rect, Rect) {
    if chat.width < 2 {
        return (chat, Rect { x: chat.x, y: chat.y, width: 0, height: chat.height });
    }
    let text = Rect { x: chat.x, y: chat.y, width: chat.width - 1, height: chat.height };
    let bar = Rect { x: chat.x + chat.width - 1, y: chat.y, width: 1, height: chat.height };
    (text, bar)
}

/// Draw the token counter for the last response in the chat pane's top-right
/// corner (overlaid, like a context gauge). No-op until the endpoint reports it.
fn render_tokens(f: &mut Frame, app: &App, chat: Rect, theme: &Theme) {
    let Some(u) = app.usage else { return };
    if chat.width < 12 || chat.height == 0 {
        return;
    }
    let label = format!(" ↑{} ↓{} · {} tok ", u.prompt_tokens, u.completion_tokens, u.total_tokens);
    let w = (label.chars().count() as u16).min(chat.width);
    let area = Rect { x: chat.x + chat.width - w, y: chat.y, width: w, height: 1 };
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(label, Style::default().fg(theme.faint)))),
        area,
    );
}
