mod agent;
mod api;
mod app;
mod config;
mod domain;
mod files;
mod input;
mod render;
mod tui;
mod ui;

use std::collections::VecDeque;
use std::time::Duration;

use app::Action;

fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let _guard = rt.enter();

    let config = config::Config::load()?;
    let mut app = app::App::new(config)?;
    let mut terminal = tui::init()?;

    let result = run(&mut terminal, &mut app, &rt);
    tui::restore()?;

    if let Err(ref e) = result {
        eprintln!("AiTUI error: {e}");
    }
    result
}

fn run(
    terminal: &mut tui::Tui,
    app: &mut app::App,
    rt: &tokio::runtime::Runtime,
) -> anyhow::Result<()> {
    let mut last_rebuild = 0u64;

    loop {
        // ── 1. Render ────────────────────────────────────────────────────
        terminal.draw(|f| {
            // Rebuild layout cache
            let total_w = f.area().width;
            let sidebar_w = if app.sidebar_collapsed { 0 } else { app.config.ui.sidebar_width };
            let fits = total_w >= sidebar_w + 24;
            let effective_sidebar = if fits && !app.sidebar_collapsed { sidebar_w } else { 0 };

            let layout = ui::layout::compute(f.area(), effective_sidebar, app.config.ui.input_height);
            app.layout = app::state::PanelLayout {
                sidebar: layout.sidebar,
                chat: layout.chat,
                input: layout.input,
                statusbar: layout.statusbar,
                toggle: ratatui::layout::Rect::default(),
            };

            // Rebuild chat doc if stale, then render everything
            let viewport_h = layout.chat.height.saturating_sub(2) as usize;
            app.sync_chat_doc(layout.chat.width as usize, viewport_h);

            ui::render(f, app);
        })?;

        // ── 2. Poll crossterm events ────────────────────────────────────
        if crossterm::event::poll(Duration::from_millis(16))? {
            let event = crossterm::event::read()?;
            let actions = input::handler::handle_event(app, event);
            dispatch(app, actions);
        }

        // ── 3. Drain model fetch channel ─────────────────────────────────
        if let Some(rx) = app.models_rx.as_mut() {
            match rx.try_recv() {
                Ok(Ok(models)) => {
                    dispatch(app, vec![Action::ModelsLoaded(models)]);
                    app.models_rx = None;
                }
                Ok(Err(_)) => { app.models_rx = None; }
                Err(_) => {}
            }
        }

        // ── 4. Drain stream channel ─────────────────────────────────────
        while app.stream_rx.is_some() {
            use tokio::sync::mpsc::error::TryRecvError;
            let rx = app.stream_rx.as_mut().unwrap();
            match rx.try_recv() {
                Ok(api::StreamEvent::Token(t)) => {
                    dispatch(app, vec![Action::StreamToken(t)]);
                }
                Ok(api::StreamEvent::Reasoning(r)) => {
                    dispatch(app, vec![Action::StreamReasoning(r)]);
                }
                Ok(api::StreamEvent::Done) => {
                    dispatch(app, vec![Action::StreamDone]);
                    break;
                }
                Ok(api::StreamEvent::Error(e)) => {
                    dispatch(app, vec![Action::StreamError(e)]);
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    dispatch(app, vec![Action::StreamDone]);
                    break;
                }
            }
        }

        // ── 5. Drain agent tool result channel ─────────────────────────
        if let Some(rx) = app.agent_tool_rx.as_mut() {
            match rx.try_recv() {
                Ok(result) => {
                    dispatch(app, vec![Action::AgentToolResult(result)]);
                    app.agent_tool_rx = None;
                }
                Err(_) => {}
            }
        }

        // ── 6. Check quit flag ─────────────────────────────────────────
        if app.should_quit {
            break;
        }
    }

    Ok(())
}

fn dispatch(app: &mut app::App, actions: Vec<Action>) {
    let mut queue: VecDeque<Action> = actions.into();
    while let Some(action) = queue.pop_front() {
        if let Some(follow_up) = app.apply(action) {
            queue.push_back(follow_up);
        }
    }
}
