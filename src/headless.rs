//! Non-interactive command runner for scripts, editors, and API-like clients.
//! Output is newline-delimited JSON and is flushed after every event.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use serde_json::{json, Value};

use crate::agent::{ToolCall, ToolKind, ToolResult};
use crate::api::StreamEvent;
use crate::app::overlay::Overlay;
use crate::app::{Action, App};
use crate::config::{AccessReviewMode, Config};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub model: Option<String>,
    pub access: Vec<AccessScope>,
    pub session_id: Option<usize>,
    pub command: String,
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessMode {
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessScope {
    mode: AccessMode,
    pattern: String,
}

pub enum Invocation {
    Tui,
    Help,
    Run(Options),
}

impl Invocation {
    pub fn parse(args: impl IntoIterator<Item = String>) -> anyhow::Result<Self> {
        let args: Vec<String> = args.into_iter().collect();
        if args.is_empty() {
            return Ok(Self::Tui);
        }
        if args.iter().any(|arg| arg == "-h" || arg == "--help") {
            return Ok(Self::Help);
        }

        let mut model = None;
        let mut access = Vec::new();
        let mut session_id = None;
        let mut command = None;
        let mut cwd = None;
        let mut index = 0;
        while index < args.len() {
            let flag = &args[index];
            let value = |index: &mut usize| -> anyhow::Result<String> {
                *index += 1;
                args.get(*index)
                    .cloned()
                    .ok_or_else(|| anyhow!("{flag} requires a value"))
            };
            match flag.as_str() {
                "--model" => model = Some(value(&mut index)?),
                "--access" => {
                    let raw = value(&mut index)?;
                    access.extend(parse_access_list(&raw)?);
                }
                "--session-id" => {
                    let raw = value(&mut index)?;
                    session_id = Some(raw.parse().with_context(|| {
                        format!("--session-id must be a positive integer, got {raw:?}")
                    })?);
                }
                "--cmd" => command = Some(value(&mut index)?),
                "--cwd" => cwd = Some(PathBuf::from(value(&mut index)?)),
                other => bail!("unknown argument {other:?}; run aitui --help"),
            }
            index += 1;
        }
        let command = command.ok_or_else(|| anyhow!("--cmd is required in command mode"))?;
        Ok(Self::Run(Options {
            model,
            access,
            session_id,
            command,
            cwd,
        }))
    }
}

pub fn help() -> &'static str {
    "AiTUI\n\n\
Interactive:\n  aitui\n\n\
Headless JSONL command mode:\n  aitui --model MODEL --access 'r{PATH_GLOB},w{PATH_GLOB}' \\\n        [--session-id ID] [--cwd DIR] --cmd PROMPT\n\n\
Every stdout line is one JSON event. Declared read/write scopes are auto-approved;\n\
undeclared tool access is denied and reported without an interactive prompt.\n"
}

pub async fn run(options: Options) -> anyhow::Result<()> {
    if let Some(cwd) = &options.cwd {
        std::env::set_current_dir(cwd)
            .with_context(|| format!("failed to enter --cwd {}", cwd.display()))?;
    }

    let mut config = Config::load()?;
    // Command mode must never inherit an interactive auto-approval or model judge
    // that could grant more access than the explicit --access contract.
    config.ui.auto_approve_reads = false;
    config.api.access_review_mode = AccessReviewMode::Off;
    let mut app = App::new(config)?;
    app.overlay = Overlay::None;
    app.permissions = Default::default();
    app.session_permissions.clear();

    if let Some(id) = options.session_id {
        let index = app
            .sessions
            .all()
            .iter()
            .position(|session| session.id == id)
            .ok_or_else(|| anyhow!("session {id} does not exist"))?;
        app.sessions.select(index);
    } else if !app.sessions.active().messages.is_empty() {
        app.sessions.new_session();
    }
    if let Ok(cwd) = std::env::current_dir() {
        app.sessions.active_mut().cwd = Some(cwd);
    }
    app.sessions.active_mut().agent_mode = true;
    if let Some(model) = options.model.clone() {
        dispatch(&mut app, Action::SelectModel(model));
    }

    let sid = app.sessions.active_id();
    let initial_messages = app.sessions.active().messages.len();
    emit(json!({
        "type": "run.started",
        "session_id": sid,
        "model": app.current_model(),
        "access": options.access.iter().map(AccessScope::label).collect::<Vec<_>>()
    }))?;

    app.input.set_text(&options.command);
    dispatch(&mut app, Action::Submit);
    if app.streams.is_empty() && !app.is_busy() {
        bail!(
            "command was not submitted: {}",
            app.status.clone().unwrap_or_default()
        );
    }

    let mut fatal: Option<String> = None;
    let mut last_tool_summary: Option<String> = None;
    loop {
        drain_streams(&mut app, sid, &mut fatal)?;
        drain_tools(&mut app)?;
        drain_subtasks(&mut app)?;
        drain_judge(&mut app);
        drain_background(&mut app);

        if let Some(cut_sid) = app.cut_stream.take() {
            dispatch(&mut app, Action::StartAgentRound(cut_sid));
        }

        if let Err(error) = resolve_headless_overlay(&mut app, &options.access) {
            emit(json!({"type": "run.error", "session_id": sid, "error": error.to_string()}))?;
            return Err(error);
        }

        let active_summary = app.active_tool.as_ref().map(|(summary, _)| summary.clone());
        if active_summary != last_tool_summary {
            if let Some(summary) = &active_summary {
                emit(json!({"type": "tool.started", "session_id": sid, "summary": summary}))?;
            }
            last_tool_summary = active_summary;
        }

        if let Some(error) = fatal.take() {
            emit(json!({"type": "run.error", "session_id": sid, "error": error}))?;
            bail!("headless run failed");
        }
        if run_complete(&app, sid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let session = app
        .sessions
        .by_id(sid)
        .ok_or_else(|| anyhow!("session {sid} disappeared during the run"))?;
    let final_text = session
        .messages
        .iter()
        .skip(initial_messages)
        .rev()
        .find(|message| message.role == "assistant")
        .map(message_text)
        .unwrap_or_default();
    emit(json!({
        "type": "run.completed",
        "session_id": sid,
        "model": app.current_model(),
        "output": final_text,
        "usage": app.session_usage.get(&sid).map(usage_json)
    }))?;
    app.sessions.clear_presence();
    Ok(())
}

fn drain_streams(
    app: &mut App,
    target_sid: usize,
    fatal: &mut Option<String>,
) -> anyhow::Result<()> {
    use tokio::sync::mpsc::error::TryRecvError;
    let mut actions = Vec::new();
    let mut target_error = None;
    for handle in app.streams.iter_mut() {
        let sid = handle.session_id;
        loop {
            match handle.rx.try_recv() {
                Ok(StreamEvent::Token(text)) => {
                    emit(json!({"type": "assistant.delta", "session_id": sid, "text": text}))?;
                    actions.push(Action::StreamToken(sid, text));
                }
                Ok(StreamEvent::Reasoning(text)) => {
                    emit(
                        json!({"type": "assistant.reasoning_delta", "session_id": sid, "text": text}),
                    )?;
                    actions.push(Action::StreamReasoning(sid, text));
                }
                Ok(StreamEvent::Usage(usage)) => {
                    emit(json!({"type": "usage", "session_id": sid, "usage": usage_json(&usage)}))?;
                    actions.push(Action::StreamUsage(sid, usage));
                }
                Ok(StreamEvent::ToolCallStarted(name)) => {
                    emit(json!({"type": "tool.call_started", "session_id": sid, "name": name}))?;
                    actions.push(Action::StreamToolCallStarted(sid, name));
                }
                Ok(StreamEvent::ImageReady(path)) => {
                    emit(json!({"type": "image.ready", "session_id": sid, "path": path}))?;
                    actions.push(Action::StreamImageReady(sid, path));
                }
                Ok(StreamEvent::ImageError(error)) => {
                    emit(json!({"type": "image.error", "session_id": sid, "error": error}))?;
                    if sid == target_sid {
                        *fatal = Some(error.clone());
                    }
                    actions.push(Action::StreamImageError(sid, error));
                    break;
                }
                Ok(StreamEvent::Done) => {
                    emit(json!({"type": "assistant.stream_completed", "session_id": sid}))?;
                    actions.push(Action::StreamDone(sid));
                    break;
                }
                Ok(StreamEvent::Error(error)) => {
                    emit(
                        json!({"type": "assistant.stream_error", "session_id": sid, "error": error}),
                    )?;
                    if sid == target_sid {
                        target_error = Some(error.clone());
                    }
                    actions.push(Action::StreamError(sid, error));
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
    for action in actions {
        dispatch(app, action);
    }
    if let Some(error) = target_error {
        let retrying = app
            .streams
            .iter()
            .any(|stream| stream.session_id == target_sid);
        if !retrying {
            *fatal = Some(error);
        }
    }
    Ok(())
}

fn drain_tools(app: &mut App) -> anyhow::Result<()> {
    if let Some(rx) = app.agent_tool_rx.as_mut() {
        if let Ok(result) = rx.try_recv() {
            emit_tool_result(app.agent_session, &result)?;
            dispatch(app, Action::AgentToolResult(result));
        }
    }
    if let Some(rx) = app.agent_tool_batch_rx.as_mut() {
        if let Ok(results) = rx.try_recv() {
            for result in &results {
                emit_tool_result(app.agent_session, result)?;
            }
            dispatch(app, Action::AgentToolBatchResult(results));
        }
    }
    Ok(())
}

fn emit_tool_result(session_id: Option<usize>, result: &ToolResult) -> anyhow::Result<()> {
    emit(json!({
        "type": "tool.completed",
        "session_id": session_id,
        "call": result.call,
        "ok": result.is_ok(),
        "output": result.text(),
        "duration_ms": result.duration_ms
    }))
}

fn drain_subtasks(app: &mut App) -> anyhow::Result<()> {
    while let Ok(event) = app.subtask_rx.try_recv() {
        if let crate::app::state::SubtaskEvent::AccessRequested {
            id,
            request_id,
            call,
            cwd,
            response,
        } = event
        {
            emit(json!({
                "type": "agent.access_required",
                "id": id,
                "request_id": request_id,
                "call": call,
                "cwd": cwd,
            }))?;
            let _ = response.send(Err(
                "Interactive access approval is unavailable in headless mode; declare the required access scope before launch".into(),
            ));
            continue;
        }
        let payload = match &event {
            crate::app::state::SubtaskEvent::AccessRequested { .. } => {
                unreachable!("access requests are handled before event dispatch")
            }
            crate::app::state::SubtaskEvent::Registered {
                id,
                parent_id,
                call,
                description,
                agent,
                cwd,
                ..
            } => {
                json!({"type":"agent.registered","id":id,"parent_id":parent_id,"call":call,"description":description,"agent":agent,"cwd":cwd})
            }
            crate::app::state::SubtaskEvent::Progress { id, progress } => {
                json!({"type":"agent.progress","id":id,"progress":format!("{:?}", progress)})
            }
            crate::app::state::SubtaskEvent::Round { id, role, content } => {
                json!({"type":"agent.round","id":id,"role":format!("{:?}", role),"content":content})
            }
            crate::app::state::SubtaskEvent::Finished {
                id,
                output,
                duration_ms,
            } => {
                json!({"type":"agent.completed","id":id,"ok":output.is_ok(),"output":output.as_ref().map_or_else(|e|e.as_str(),|v|v.as_str()),"duration_ms":duration_ms})
            }
        };
        emit(payload)?;
        dispatch(app, Action::SubtaskEvent(event));
    }
    Ok(())
}

fn drain_judge(app: &mut App) {
    if let Some(rx) = app.judge_rx.as_mut() {
        if let Ok((sid, verdicts)) = rx.try_recv() {
            dispatch(app, Action::AccessJudged(sid, verdicts));
        }
    }
}

fn drain_background(app: &mut App) {
    while let Ok((epoch, result)) = app.spec_rx.try_recv() {
        app.store_spec_result(epoch, result);
    }
    while let Ok((sid, signature, result)) = app.todo_rx.try_recv() {
        dispatch(app, Action::TodoUpdateReady(sid, signature, result));
    }
    while let Ok((sid, source_turn, result)) = app.memory_rx.try_recv() {
        dispatch(
            app,
            Action::SessionMemoryExtracted {
                session_id: sid,
                source_turn,
                result,
            },
        );
    }
    while let Ok((sid, signature, suggestions)) = app.suggestion_rx.try_recv() {
        dispatch(
            app,
            Action::ResponseSuggestionsReady(sid, signature, suggestions),
        );
    }
    if let Some(rx) = app.title_rx.as_mut() {
        if let Ok((sid, title)) = rx.try_recv() {
            dispatch(app, Action::SessionTitleGenerated(sid, title));
        }
    }
}

fn resolve_headless_overlay(app: &mut App, scopes: &[AccessScope]) -> anyhow::Result<()> {
    match app.overlay.clone() {
        Overlay::None | Overlay::Notice { .. } => Ok(()),
        Overlay::Permission(request) => {
            app.overlay = Overlay::None;
            for call in request.calls {
                let allowed = call_allowed(&call, &request.cwd, scopes);
                emit(json!({
                    "type": "tool.permission",
                    "call": call,
                    "decision": if allowed { "allow" } else { "deny" },
                    "reason": if allowed { "matched --access" } else { "outside --access" }
                }))?;
                if allowed {
                    app.approved.push_back(call);
                } else {
                    app.record_tool_result(ToolResult::failure(
                        call,
                        "Denied by headless --access policy".to_string(),
                        0,
                    ));
                }
            }
            if let Some(action) = app.process_next_tool() {
                dispatch(app, action);
            }
            Ok(())
        }
        Overlay::Decision(request) => {
            emit(
                json!({"type":"input.required","kind":"decision","question":request.question,"options":request.options,"multi":request.multi}),
            )?;
            bail!("the model requested interactive input; command mode cannot answer it")
        }
        Overlay::PromptDuringRun(_) => {
            bail!("a prompt was submitted while the command-mode agent was still running")
        }
        Overlay::Plan(request) => {
            emit(json!({"type":"input.required","kind":"plan_approval","path":request.path}))?;
            bail!("the model requested plan approval; command mode cannot approve it")
        }
        other => bail!("headless run requires interaction: {other:?}"),
    }
}

fn run_complete(app: &App, sid: usize) -> bool {
    !app.streams.iter().any(|stream| stream.session_id == sid)
        && app.agent_session != Some(sid)
        && app
            .judging
            .as_ref()
            .is_none_or(|judge| judge.session_id != sid)
        && app
            .task_barrier
            .as_ref()
            .is_none_or(|barrier| barrier.session_id != sid)
        && app.subtasks.iter().all(|task| {
            task.session_id != sid || task.status != crate::app::state::SubtaskStatus::Running
        })
        && !app
            .sessions
            .by_id(sid)
            .is_some_and(|session| session.is_streaming())
        && matches!(app.overlay, Overlay::None | Overlay::Notice { .. })
}

fn dispatch(app: &mut App, action: Action) {
    let mut queue = VecDeque::from([action]);
    while let Some(action) = queue.pop_front() {
        if let Some(next) = app.apply(action) {
            queue.push_back(next);
        }
    }
}

fn usage_json(usage: &crate::api::Usage) -> Value {
    json!({
        "prompt_tokens": usage.prompt_tokens,
        "completion_tokens": usage.completion_tokens,
        "total_tokens": usage.total_tokens
    })
}

fn emit(value: Value) -> anyhow::Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &value)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

fn message_text(message: &crate::api::ChatMessage) -> String {
    use crate::api::models::{ContentPart, MessageContent};
    match &message.content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                ContentPart::ImageUrl { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn parse_access_list(raw: &str) -> anyhow::Result<Vec<AccessScope>> {
    let mut scopes = Vec::new();
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len() && (bytes[index] == b',' || bytes[index].is_ascii_whitespace()) {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        let mode = match bytes[index] {
            b'r' => AccessMode::Read,
            b'w' => AccessMode::Write,
            other => bail!(
                "invalid access mode {:?}; expected r{{...}} or w{{...}}",
                other as char
            ),
        };
        index += 1;
        if bytes.get(index) != Some(&b'{') {
            bail!("access scope must use r{{...}} or w{{...}}");
        }
        index += 1;
        let start = index;
        while index < bytes.len() && bytes[index] != b'}' {
            index += 1;
        }
        if index >= bytes.len() {
            bail!("unterminated access scope in {raw:?}");
        }
        let pattern = raw[start..index].trim();
        if pattern.is_empty() {
            bail!("access scope path cannot be empty");
        }
        scopes.push(AccessScope {
            mode,
            pattern: pattern.to_string(),
        });
        index += 1;
    }
    Ok(scopes)
}

impl AccessScope {
    fn label(&self) -> String {
        format!(
            "{}{{{}}}",
            if self.mode == AccessMode::Read {
                "r"
            } else {
                "w"
            },
            self.pattern
        )
    }

    fn matches(&self, path: &Path, cwd: &Path) -> bool {
        let expanded = expand_path(&self.pattern, cwd);
        let candidate = normalize_path(path, cwd)
            .to_string_lossy()
            .replace('\\', "/");
        let pattern = expanded.to_string_lossy().replace('\\', "/");
        wildcard_match(&pattern, &candidate)
    }
}

fn call_allowed(call: &ToolCall, cwd: &Path, scopes: &[AccessScope]) -> bool {
    let concrete = match call.expanded_calls() {
        Ok(Some(calls)) => return calls.iter().all(|call| call_allowed(call, cwd, scopes)),
        Ok(None) => call,
        Err(_) => return false,
    };
    let Some(kind) = concrete.kind() else {
        return false;
    };
    match kind {
        ToolKind::Todo | ToolKind::Task | ToolKind::Finish => true,
        ToolKind::WebSearch | ToolKind::WebImages | ToolKind::ReverseImage | ToolKind::WebFetch => {
            true
        }
        ToolKind::Ask | ToolKind::Plan | ToolKind::ProposeStep => true,
        ToolKind::Shell => scope_covers(scopes, AccessMode::Write, cwd, cwd),
        ToolKind::Read | ToolKind::List | ToolKind::Search => {
            paths_for(concrete, &["path", "paths"])
                .iter()
                .all(|path| read_covers(scopes, path, cwd))
        }
        ToolKind::Write | ToolKind::Edit | ToolKind::MakeDir | ToolKind::Delete => {
            paths_for(concrete, &["path", "paths"])
                .iter()
                .all(|path| scope_covers(scopes, AccessMode::Write, path, cwd))
        }
        ToolKind::Move => paths_for(concrete, &["from", "to"])
            .iter()
            .all(|path| scope_covers(scopes, AccessMode::Write, path, cwd)),
        ToolKind::Copy => {
            let from = paths_for(concrete, &["from"]);
            let to = paths_for(concrete, &["to"]);
            from.iter().all(|path| read_covers(scopes, path, cwd))
                && to
                    .iter()
                    .all(|path| scope_covers(scopes, AccessMode::Write, path, cwd))
        }
        ToolKind::Download => paths_for(concrete, &["path"])
            .iter()
            .all(|path| scope_covers(scopes, AccessMode::Write, path, cwd)),
        ToolKind::PowerPoint => {
            paths_for(concrete, &["input_path"])
                .iter()
                .all(|path| read_covers(scopes, path, cwd))
                && paths_for(concrete, &["output_path"])
                    .iter()
                    .all(|path| scope_covers(scopes, AccessMode::Write, path, cwd))
        }
        ToolKind::Video => {
            paths_for(concrete, &["entry_file"])
                .iter()
                .all(|path| read_covers(scopes, path, cwd))
                && paths_for(concrete, &["output_path"])
                    .iter()
                    .all(|path| scope_covers(scopes, AccessMode::Write, path, cwd))
        }
    }
}

fn paths_for(call: &ToolCall, keys: &[&str]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for key in keys {
        if let Some(path) = call.args.get(*key).and_then(Value::as_str) {
            paths.push(PathBuf::from(path));
        }
        if let Some(items) = call.args.get(*key).and_then(Value::as_array) {
            paths.extend(items.iter().filter_map(Value::as_str).map(PathBuf::from));
        }
    }
    paths
}

fn read_covers(scopes: &[AccessScope], path: &Path, cwd: &Path) -> bool {
    scope_covers(scopes, AccessMode::Read, path, cwd)
        || scope_covers(scopes, AccessMode::Write, path, cwd)
}

fn scope_covers(scopes: &[AccessScope], mode: AccessMode, path: &Path, cwd: &Path) -> bool {
    let path = normalize_path(path, cwd);
    scopes
        .iter()
        .any(|scope| scope.mode == mode && scope.matches(&path, cwd))
}

fn expand_path(raw: &str, cwd: &Path) -> PathBuf {
    let path = if raw == "~" {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(raw))
    } else if let Some(rest) = raw.strip_prefix("~/") {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("~"))
            .join(rest)
    } else {
        PathBuf::from(raw)
    };
    normalize_path(&path, cwd)
}

fn normalize_path(path: &Path, cwd: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

fn wildcard_match(pattern: &str, candidate: &str) -> bool {
    let escaped = regex::escape(pattern).replace("\\*", ".*");
    regex::Regex::new(&format!("^{escaped}$")).is_ok_and(|regex| regex.is_match(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_command_mode_arguments_and_multiple_access_scopes() {
        let Invocation::Run(options) = Invocation::parse([
            "--model".into(),
            "gpt-test".into(),
            "--access".into(),
            "r{~/Codes/*},w{./out.txt}".into(),
            "--session-id".into(),
            "7".into(),
            "--cmd".into(),
            "hello".into(),
        ])
        .unwrap() else {
            panic!("expected command mode");
        };
        assert_eq!(options.model.as_deref(), Some("gpt-test"));
        assert_eq!(options.session_id, Some(7));
        assert_eq!(options.access.len(), 2);
        assert_eq!(options.command, "hello");
    }

    #[test]
    fn exact_write_scope_does_not_cover_a_sibling() {
        let cwd = Path::new("/tmp/project");
        let scopes = parse_access_list("w{/tmp/project/out.txt}").unwrap();
        assert!(scope_covers(
            &scopes,
            AccessMode::Write,
            Path::new("out.txt"),
            cwd
        ));
        assert!(!scope_covers(
            &scopes,
            AccessMode::Write,
            Path::new("other.txt"),
            cwd
        ));
    }

    #[test]
    fn star_scope_covers_descendants() {
        let cwd = Path::new("/tmp");
        let scopes = parse_access_list("r{/tmp/project/*}").unwrap();
        assert!(read_covers(
            &scopes,
            Path::new("/tmp/project/src/main.rs"),
            cwd
        ));
        assert!(!read_covers(&scopes, Path::new("/tmp/other/main.rs"), cwd));
    }
}
