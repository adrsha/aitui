mod agent;
mod api;
mod app;
mod config;
mod domain;
mod files;
mod input;
mod render;
mod skills;
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
    _rt: &tokio::runtime::Runtime,
) -> anyhow::Result<()> {
    loop {
        // ── 1. Render (ui::render owns layout + chat-doc sync) ───────────
        terminal.draw(|f| ui::render(f, app))?;

        // ── 1b. Pending external program: suspend TUI, run it, restore ───
        if let Some(ext) = app.pending_external.take() {
            run_external(terminal, ext)?;
            app.set_status("Back in AiTUI");
            app.touch();
            continue;
        }

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

        // ── 4. Drain all session streams (parallel-safe) ────────────────
        // Collect this pass's events per stream, then dispatch — draining every
        // active stream each loop so background sessions keep progressing.
        {
            use tokio::sync::mpsc::error::TryRecvError;
            let mut actions: Vec<Action> = Vec::new();
            for h in app.streams.iter_mut() {
                let sid = h.session_id;
                loop {
                    match h.rx.try_recv() {
                        Ok(api::StreamEvent::Token(t)) => actions.push(Action::StreamToken(sid, t)),
                        Ok(api::StreamEvent::Reasoning(r)) => actions.push(Action::StreamReasoning(sid, r)),
                        Ok(api::StreamEvent::Usage(u)) => actions.push(Action::StreamUsage(sid, u)),
                        Ok(api::StreamEvent::Done) => { actions.push(Action::StreamDone(sid)); break; }
                        Ok(api::StreamEvent::Error(e)) => { actions.push(Action::StreamError(sid, e)); break; }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => { actions.push(Action::StreamDone(sid)); break; }
                    }
                }
            }
            if !actions.is_empty() {
                dispatch(app, actions);
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

/// Suspend the TUI, run an external program (editor or shell), then restore the
/// terminal. The TUI is always re-entered afterwards, even if the program failed.
fn run_external(terminal: &mut tui::Tui, ext: app::state::PendingExternal) -> anyhow::Result<()> {
    // Leave our alternate screen / raw mode so the child owns the terminal.
    tui::restore()?;
    let result = run_external_inner(ext);
    // Re-enter the TUI regardless of how the child exited.
    *terminal = tui::init()?;
    terminal.clear()?;
    result
}

fn run_external_inner(ext: app::state::PendingExternal) -> anyhow::Result<()> {
    use app::state::PendingExternal;
    use std::io::Write;
    use std::process::Command;

    let editor = || {
        std::env::var("EDITOR")
            .or_else(|_| std::env::var("VISUAL"))
            .unwrap_or_else(|_| "nvim".to_string())
    };
    // vim/nvim/vi accept a bare `+` to open on the last line; other editors
    // would treat `+` as a filename, so only pass it to the vim family.
    let jumps_to_end = |ed: &str| {
        let base = ed.rsplit('/').next().unwrap_or(ed);
        matches!(base, "vim" | "nvim" | "vi" | "view" | "gvim")
    };

    match ext {
        PendingExternal::EditorFiles(paths) => {
            if paths.is_empty() {
                return Ok(());
            }
            let ed = editor();
            let mut cmd = Command::new(&ed);
            if jumps_to_end(&ed) {
                cmd.arg("+"); // open on the last line
            }
            cmd.args(&paths)
                .status()
                .map_err(|e| anyhow::anyhow!("Failed to launch {ed}: {e}"))?;
        }
        PendingExternal::EditorText(text) => {
            let ed = editor();
            let path = std::env::temp_dir().join(format!("aitui-conversation-{}.md", std::process::id()));
            std::fs::File::create(&path)?.write_all(text.as_bytes())?;
            let mut cmd = Command::new(&ed);
            if jumps_to_end(&ed) {
                cmd.arg("+"); // open on the last line (latest turn)
            }
            let status = cmd.arg(&path).status();
            let _ = std::fs::remove_file(&path);
            status.map_err(|e| anyhow::anyhow!("Failed to launch {ed}: {e}"))?;
        }
        PendingExternal::Shell => {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "bash".to_string());
            println!("\n[AiTUI] Shell — type 'exit' to return.\n");
            Command::new(&shell)
                .status()
                .map_err(|e| anyhow::anyhow!("Failed to launch {shell}: {e}"))?;
        }
    }
    Ok(())
}
