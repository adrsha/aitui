#![recursion_limit = "256"]

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

    // Restore the terminal on panic *before* the default hook prints, so a crash
    // anywhere in the loop can't leave the user's shell in raw mode.
    tui::install_panic_hook();

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
    // Draw only when something changed, instead of spinning at ~250fps. `dirty`
    // starts true so the first frame always draws.
    let mut dirty = true;
    let mut last_session_sync = std::time::Instant::now();
    app.sessions.publish_presence(&app.running_session_ids());
    loop {
        // Animations (streaming spinner, "working" indicator) need periodic
        // redraws even without new events; a busy state forces a fast repaint.
        let animating = !app.streams.is_empty() || app.is_busy();

        // ── 1. Render (ui::render owns layout + chat-doc sync) ───────────
        if dirty || animating {
            terminal.draw(|f| ui::render(f, app))?;
            dirty = display_pending_image(app);
        }

        // ── 1b. Pending external program: suspend TUI, run it, restore ───
        if let Some(ext) = app.pending_external.take() {
            let follow_up = run_external(terminal, ext)?;
            if let Some(action) = follow_up {
                dispatch(app, vec![action]);
            }
            app.touch();
            dirty = true;
            continue;
        }

        // ── 2. Poll crossterm events ────────────────────────────────────
        // Poll fast while animating (smooth spinner), slow when idle (low CPU).
        let timeout = if animating { 33 } else { 250 };
        if crossterm::event::poll(Duration::from_millis(timeout))? {
            let event = crossterm::event::read()?;
            let actions = input::handler::handle_event(app, event);
            if !actions.is_empty() {
                dispatch(app, actions);
            }
            dirty = true; // an event may move the cursor / selection even with no action
        }

        // ── 2b. Drain actionable desktop-notification responses ─────────
        while let Ok(action) = app.notification_rx.try_recv() {
            dispatch(app, vec![Action::DesktopNotification(action)]);
            dirty = true;
        }

        // ── 3. Drain model fetch channel ─────────────────────────────────
        if let Some(rx) = app.models_rx.as_mut() {
            match rx.try_recv() {
                Ok(Ok(models)) => {
                    dispatch(app, vec![Action::ModelsLoaded(models)]);
                    app.models_rx = None;
                    dirty = true;
                }
                Ok(Err(_)) => {
                    dispatch(app, vec![Action::ModelsFailed]);
                    app.models_rx = None;
                    dirty = true;
                }
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
                        Ok(api::StreamEvent::Reasoning(r)) => {
                            actions.push(Action::StreamReasoning(sid, r))
                        }
                        Ok(api::StreamEvent::Usage(u)) => actions.push(Action::StreamUsage(sid, u)),
                        Ok(api::StreamEvent::ToolCallStarted(name)) => {
                            actions.push(Action::StreamToolCallStarted(sid, name))
                        }
                        Ok(api::StreamEvent::ImageReady(path)) => {
                            actions.push(Action::StreamImageReady(sid, path))
                        }
                        Ok(api::StreamEvent::ImageError(error)) => {
                            actions.push(Action::StreamImageError(sid, error));
                            break;
                        }
                        Ok(api::StreamEvent::Done) => {
                            actions.push(Action::StreamDone(sid));
                            break;
                        }
                        Ok(api::StreamEvent::Error(e)) => {
                            actions.push(Action::StreamError(sid, e));
                            break;
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            actions.push(Action::StreamDone(sid));
                            break;
                        }
                    }
                }
            }
            if !actions.is_empty() {
                dispatch(app, actions);
                dirty = true;
            }
        }

        // ── 5. Drain agent tool result channel ─────────────────────────
        if let Some(rx) = app.agent_tool_rx.as_mut() {
            if let Ok(result) = rx.try_recv() {
                dispatch(app, vec![Action::AgentToolResult(result)]);
                app.agent_tool_rx = None;
                dirty = true;
            }
        }
        if let Some(rx) = app.agent_tool_batch_rx.as_mut() {
            if let Ok(results) = rx.try_recv() {
                dispatch(app, vec![Action::AgentToolBatchResult(results)]);
                app.agent_tool_batch_rx = None;
                dirty = true;
            }
        }

        // ── 5a. Drain parallel child-agent progress/results ────────────────
        while let Ok(event) = app.subtask_rx.try_recv() {
            dispatch(app, vec![Action::SubtaskEvent(event)]);
            dirty = true;
        }

        // ── 5a2. Drain access-policy judge verdicts ─────────────────────
        if let Some(rx) = app.judge_rx.as_mut() {
            if let Ok((sid, verdicts)) = rx.try_recv() {
                dispatch(app, vec![Action::AccessJudged(sid, verdicts)]);
                dirty = true;
            }
        }

        // ── 5a3. Drain generated session titles ─────────────────────────
        if let Some(rx) = app.title_rx.as_mut() {
            if let Ok((sid, title)) = rx.try_recv() {
                dispatch(app, vec![Action::SessionTitleGenerated(sid, title)]);
                app.title_rx = None;
                dirty = true;
            }
        }

        // ── 5a4. Drain optional response suggestions ───────────────────
        while let Ok((sid, signature, suggestions)) = app.suggestion_rx.try_recv() {
            dispatch(
                app,
                vec![Action::ResponseSuggestionsReady(
                    sid,
                    signature,
                    suggestions,
                )],
            );
            dirty = true;
        }

        // ── 5a5. Drain session-memory extraction results ───────────────
        while let Ok((session_id, source_turn, result)) = app.memory_rx.try_recv() {
            dispatch(
                app,
                vec![Action::SessionMemoryExtracted {
                    session_id,
                    source_turn,
                    result,
                }],
            );
            dirty = true;
        }

        // ── 5a6. Drain parallel task-tracker results ──────────────────
        while let Ok((sid, signature, result)) = app.todo_rx.try_recv() {
            dispatch(app, vec![Action::TodoUpdateReady(sid, signature, result)]);
            dirty = true;
        }

        // ── 5b. Drain speculative (pre-run read-only) tool results ──────
        while let Ok((epoch, result)) = app.spec_rx.try_recv() {
            app.store_spec_result(epoch, result);
        }

        // ── 5c. Restart streams that have produced no visible output ───────
        if let Some((sid, retries)) = cold_stream_to_retry(app) {
            dispatch(app, vec![Action::RetryStream(sid, retries + 1)]);
            dirty = true;
        }

        // ── 5d. A stream was cut early (tool call detected) — start its round
        // now, on a clean pass, so any leftover tokens from the cut stream have
        // already been drained (and no-op'd) before the next stream begins.
        if let Some(sid) = app.cut_stream.take() {
            dispatch(app, vec![Action::StartAgentRound(sid)]);
            dirty = true;
        }

        // ── 5e. Reconcile sessions written by other AiTUI clients ───────
        if last_session_sync.elapsed() >= Duration::from_millis(500) {
            app.sessions.publish_presence(&app.running_session_ids());
            dispatch(app, vec![Action::SyncSessions]);
            last_session_sync = std::time::Instant::now();
            dirty = true;
        }

        // ── 6. Check quit flag ─────────────────────────────────────────
        if app.should_quit {
            break;
        }
    }

    app.sessions.clear_presence();
    Ok(())
}

fn display_pending_image(app: &mut app::App) -> bool {
    let Some(path) = app.pending_image.take() else {
        return false;
    };
    let area = app.layout.chat;
    if area.width < 8 || area.height < 4 {
        app.set_status(format!("Image saved: {}", path.display()));
        return true;
    }
    let cols = area.width.saturating_sub(4).min(80);
    let rows = area.height.saturating_sub(2).min((cols / 2).max(4));
    if let Err(error) = crate::render::image::display_image(
        &path,
        area.x.saturating_add(2),
        area.y.saturating_add(1),
        cols,
        rows,
    ) {
        app.set_status(format!("Image saved: {} · {}", path.display(), error));
        return true;
    }
    false
}

fn dispatch(app: &mut app::App, actions: Vec<Action>) {
    let mut queue: VecDeque<Action> = actions.into();
    while let Some(action) = queue.pop_front() {
        if let Some(follow_up) = app.apply(action) {
            queue.push_back(follow_up);
        }
    }
}

fn cold_stream_to_retry(app: &app::App) -> Option<(usize, u8)> {
    app.streams.iter().find_map(|h| {
        if h.cold_retries >= domain::session::MAX_COLD_STREAM_RETRIES {
            return None;
        }
        let session = app.sessions.by_id(h.session_id)?;
        let start = session.pending_started_at?;
        if session.pending_first_at.is_none()
            && start.elapsed() >= domain::session::COLD_STREAM_RETRY_AFTER
        {
            Some((h.session_id, h.cold_retries))
        } else {
            None
        }
    })
}

/// Suspend the TUI, run an external program (editor or shell), then restore the
/// terminal. The TUI is always re-entered afterwards, even if the program failed.
/// Returns an optional follow-up action to dispatch (e.g. the edited permission
/// buffer read back from the temp file).
fn run_external(
    terminal: &mut tui::Tui,
    ext: app::state::PendingExternal,
) -> anyhow::Result<Option<Action>> {
    // Leave our alternate screen / raw mode so the child owns the terminal.
    tui::restore()?;
    let result = run_external_inner(ext);
    // Re-enter the TUI regardless of how the child exited.
    *terminal = tui::init()?;
    terminal.clear()?;
    result
}

fn run_external_inner(ext: app::state::PendingExternal) -> anyhow::Result<Option<Action>> {
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
                return Ok(None);
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
            let path =
                std::env::temp_dir().join(format!("aitui-conversation-{}.md", std::process::id()));
            std::fs::File::create(&path)?.write_all(text.as_bytes())?;
            let mut cmd = Command::new(&ed);
            if jumps_to_end(&ed) {
                cmd.arg("+"); // open on the last line (latest turn)
            }
            let status = cmd.arg(&path).status();
            let _ = std::fs::remove_file(&path);
            status.map_err(|e| anyhow::anyhow!("Failed to launch {ed}: {e}"))?;
        }
        PendingExternal::EditReadback(path) => {
            let ed = editor();
            // No `+` jump — open at the top so the whole batch is in view.
            Command::new(&ed)
                .arg(&path)
                .status()
                .map_err(|e| anyhow::anyhow!("Failed to launch {ed}: {e}"))?;
            let contents = std::fs::read_to_string(&path).unwrap_or_default();
            let _ = std::fs::remove_file(&path);
            return Ok(Some(Action::AgentPermissionEdited(contents)));
        }
        PendingExternal::DecisionReadback(path) => {
            let ed = editor();
            Command::new(&ed)
                .arg(&path)
                .status()
                .map_err(|e| anyhow::anyhow!("Failed to launch {ed}: {e}"))?;
            let contents = std::fs::read_to_string(&path).unwrap_or_default();
            let _ = std::fs::remove_file(&path);
            return Ok(Some(Action::AgentDecisionEdited(contents)));
        }
        PendingExternal::PolicyReadback(path) => {
            let ed = editor();
            Command::new(&ed)
                .arg(&path)
                .status()
                .map_err(|e| anyhow::anyhow!("Failed to launch {ed}: {e}"))?;
            let raw = std::fs::read_to_string(&path).unwrap_or_default();
            let _ = std::fs::remove_file(&path);
            // Drop `#` comment lines (the seeded instructions) and keep the rest.
            let policy = raw
                .lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();
            return Ok(Some(Action::SetAccessPolicy(policy)));
        }
        PendingExternal::LoopReadback(path) => {
            let ed = editor();
            Command::new(&ed)
                .arg(&path)
                .status()
                .map_err(|e| anyhow::anyhow!("Failed to launch {ed}: {e}"))?;
            let raw = std::fs::read_to_string(&path).unwrap_or_default();
            let _ = std::fs::remove_file(&path);
            return Ok(Some(Action::StartLoopSpec(raw)));
        }
        PendingExternal::Shell => {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "bash".to_string());
            println!("\n[AiTUI] Shell — type 'exit' to return.\n");
            Command::new(&shell)
                .status()
                .map_err(|e| anyhow::anyhow!("Failed to launch {shell}: {e}"))?;
        }
    }
    Ok(None)
}
