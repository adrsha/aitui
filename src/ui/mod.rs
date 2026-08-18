pub mod chat;
pub mod help;
pub mod input;
pub mod layout;
pub mod overlay;
pub mod promptbar;
pub mod sidepanel;
pub mod statusbar;
pub mod toast;

use crate::app::input_layout;
use crate::app::state::{App, PanelLayout};
use crate::render::theme::Theme;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

/// Input box auto-sizes to its wrapped visual content, at least one row so it's
/// always visible, and at most `config.ui.input_height` rows so a huge paste can't
/// crowd out the transcript (that cap is what `:resize` adjusts).
pub fn render(f: &mut Frame, app: &mut App) {
    let max_rows = app.config.ui.input_height.max(1);
    let main_width = f
        .area()
        .width
        .saturating_sub(layout::sidebar_width(f.area().width));
    let input_width = main_width.saturating_sub(5).max(1) as usize;
    app.input.set_width(input_width);
    let input_rows = input_layout::layout(&app.input.lines, input_width)
        .len()
        .min(u16::MAX as usize) as u16;
    let input_rows = input_rows.clamp(1, max_rows);
    let topbar_h = 0u16;
    let promptbar_h = promptbar::height(app, f.area().width).min(f.area().height.saturating_sub(5));
    let max_bottom_h = f
        .area()
        .height
        .saturating_sub(promptbar_h.saturating_add(4))
        .max(1);
    let input_panel_h = overlay::height(&app.overlay, f.area().width, max_bottom_h)
        .unwrap_or_else(|| input_rows.saturating_add(2));
    let lay = layout::compute(f.area(), topbar_h, promptbar_h, input_panel_h, 0);

    app.layout = PanelLayout {
        chat: lay.chat,
        session_tabs: Vec::new(),
        access: None,
        sidebar_tasks: None,
        sidebar_tasks_tab: None,
        sidebar_agents_tab: None,
        access_rows: Vec::new(),
        sidebar_agents: Vec::new(),
        panel_agents: Vec::new(),
        prompt: None,
        prompt_goto: None,
    };

    app.sync_chat_doc(lay.chat.width as usize, lay.chat.height as usize);

    let theme = app.theme();

    // Side panel (right side, width-responsive)
    let sidebar_hitboxes = sidepanel::render(f, app, lay.sidebar, &theme);
    app.layout.sidebar_agents = sidebar_hitboxes.agents;
    app.layout.access = sidebar_hitboxes.access.first().copied();
    app.layout.sidebar_tasks = sidebar_hitboxes.tasks;
    app.layout.sidebar_tasks_tab = sidebar_hitboxes.tasks_tab;
    app.layout.sidebar_agents_tab = sidebar_hitboxes.agents_tab;
    app.layout.access_rows.clear();

    // Main content area
    let (prompt_hb, prompt_goto_hb) = promptbar::render(f, app, lay.promptbar, &theme);
    app.layout.prompt = prompt_hb;
    app.layout.prompt_goto = prompt_goto_hb;
    chat::render(f, app, lay.chat, &theme);
    render_jump_pill(f, app, lay.chat, &theme);
    app.layout.panel_agents.clear();
    statusbar::render_activity(f, app, lay.activity, &theme);
    if matches!(app.overlay, crate::app::overlay::Overlay::None) {
        input::render(f, app, lay.input, &theme);
    } else if let Some(max_scroll) = overlay::render(f, app, lay.input, &theme) {
        if let crate::app::overlay::Overlay::SubtaskDetail { scroll, .. } = &mut app.overlay {
            *scroll = (*scroll).min(max_scroll);
        }
    }
    statusbar::render(f, app, lay.statusbar, &theme);

    if app.show_help {
        help::render(f, app, &theme);
    }
    toast::render(f, app, &theme);

    // Flush any queued clipboard copy via OSC 52 (one-shot).
    if let Some(text) = app.pending_clipboard.take() {
        crate::app::clipboard::copy(&text);
    }
}

/// When the transcript is scrolled up off the tail, draw a small "jump to bottom"
/// pill in the chat pane's bottom-right showing how many rows are hidden below.
fn render_jump_pill(f: &mut Frame, app: &App, _chat: Rect, theme: &Theme) {
    let Some((area, label)) = app.jump_pill() else {
        return;
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
