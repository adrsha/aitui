//! The application state container plus pure helpers (mention completion, file
//! walking). Mutations live in `reducer.rs`; side effects in `effects.rs`.

use std::collections::VecDeque;
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};

use crate::agent::{AccessVerdict, PermissionMemory, ToolCall, ToolResult};
use crate::api::{ApiClient, StreamEvent};
use crate::app::input_buffer::InputBuffer;
use crate::app::overlay::{sync_auto_approvals, Mention, Overlay};
use crate::config::Config;
use crate::domain::session::SessionManager;
use crate::input::keymap::Keymap;
use crate::input::vim::VimMode;
use crate::render::chat::ChatState;
use crate::render::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionTabHitbox {
    pub session_idx: usize,
    pub area: ratatui::layout::Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessHitbox {
    /// Index into the access entries list (`access_entries` ordering).
    pub index: usize,
    pub area: ratatui::layout::Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptHitbox {
    pub area: ratatui::layout::Rect,
    pub msg: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubtaskHitbox {
    pub task_id: u64,
    pub area: ratatui::layout::Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SidebarTab {
    #[default]
    Tasks,
    Agents,
}

#[derive(Debug, Clone, Default)]
pub struct PanelLayout {
    /// The transcript rect, cached so the reducer can compute page heights.
    pub chat: ratatui::layout::Rect,
    /// Click targets produced by the top-bar renderer for direct session switching.
    pub session_tabs: Vec<SessionTabHitbox>,
    /// Click target for opening the complete access manager.
    pub access: Option<AccessHitbox>,
    /// Visible viewport for the scrollable sidebar task list.
    pub sidebar_tasks: Option<ratatui::layout::Rect>,
    /// Click targets for switching between task and agent sidebar views.
    pub sidebar_tasks_tab: Option<ratatui::layout::Rect>,
    pub sidebar_agents_tab: Option<ratatui::layout::Rect>,
    /// Click targets for individual access rules in the sidebar.
    pub access_rows: Vec<AccessHitbox>,
    /// Click targets for child-agent rows in the sidebar.
    pub sidebar_agents: Vec<SubtaskHitbox>,
    /// Click targets for child-agent rows in the sticky panel below the chat.
    pub panel_agents: Vec<SubtaskHitbox>,
    /// Click target for expanding/collapsing the latest prompt preview.
    pub prompt: Option<PromptHitbox>,
    /// Click target to scroll the transcript to the prompt.
    pub prompt_goto: Option<PromptHitbox>,
}

/// Loading status of the model list from `/v1/models`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelLoad {
    /// Fetch in flight — show a loading animation instead of a model name.
    Loading,
    /// The list arrived (or we're offline on `mock`).
    Loaded,
    /// The fetch failed (connection/timeout) — show a failed indicator.
    Failed,
}

/// Progress state of one agent-declared task in the sticky todo panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Done,
}

impl TodoStatus {
    /// Parse the model's status string; unknown/empty defaults to pending.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "in_progress" | "in-progress" | "active" | "doing" => TodoStatus::InProgress,
            "done" | "completed" | "complete" => TodoStatus::Done,
            _ => TodoStatus::Pending,
        }
    }
    /// Canonical wire name for prompts and JSON output.
    pub fn name(&self) -> &'static str {
        match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Done => "done",
        }
    }
}

/// One item in the agent's task breakdown, maintained by the parallel task
/// tracker and shown in the sidebar.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TodoItem {
    pub text: String,
    pub status: TodoStatus,
    /// Tracker-estimated per-task progress (0–100), when known.
    #[serde(default)]
    pub percent: Option<u8>,
}

/// Combined output of the parallel task-tracker agent: the full checklist plus
/// the overall progress estimate.
#[derive(Debug, Clone, Default)]
pub struct TodoUpdate {
    pub items: Vec<TodoItem>,
    pub overall_percent: Option<u8>,
}

/// A tool-call batch awaiting a judgment from the access-policy judge model. Held
/// while the async call is in flight; verdicts arrive via `AccessJudged`.
pub struct JudgeBatch {
    pub session_id: usize,
    pub calls: Vec<ToolCall>,
    /// Rule assembled in the permission overlay, remembered only when the review
    /// model confirms at least one matching verdict.
    pub reviewed_rule: Option<crate::agent::PermissionRuleDraft>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtaskStatus {
    Running,
    Completed,
    /// The child returned no trustworthy review, but did return a bounded,
    /// user-facing reason instead of leaking a provider or transport error.
    Unresolved,
    /// Explicit cancellation or an internal worker failure.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtaskToolStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubtaskLogEntry {
    Phase {
        text: String,
    },
    Checklist {
        done: usize,
        running: usize,
        pending: usize,
    },
    Tool {
        name: String,
        summary: String,
        status: SubtaskToolStatus,
        duration_ms: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        call: Option<ToolCall>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub enum SubtaskProgress {
    Phase(String),
    Checklist {
        done: usize,
        running: usize,
        pending: usize,
    },
    ToolStarted {
        name: String,
        summary: String,
        call: ToolCall,
    },
    ToolOutput {
        name: String,
        summary: String,
        chunk: String,
    },
    ToolFinished {
        name: String,
        summary: String,
        call: ToolCall,
        output: String,
        ok: bool,
        duration_ms: u64,
    },
}

/// One captured conversation line inside a child agent: its own prose reply, a
/// tool call it made, or the result of that call. Rendered when the user
/// navigates into the agent (chat area).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtaskRoundRole {
    Assistant,
    ToolCall,
    ToolResult,
}

#[derive(Debug, Clone)]
pub struct SubtaskRound {
    pub role: SubtaskRoundRole,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct Subtask {
    pub id: u64,
    pub session_id: usize,
    /// Parent child agent when this agent was spawned by another child agent;
    /// `None` means a direct child of the root (UI-session) agent.
    pub parent_id: Option<u64>,
    pub call: ToolCall,
    pub description: String,
    /// One-based main checklist subtask delegated to this child agent, when declared.
    pub todo_index: Option<usize>,
    pub prompt: String,
    pub cwd: PathBuf,
    pub status: SubtaskStatus,
    pub activity: Option<String>,
    pub log: Vec<SubtaskLogEntry>,
    /// Captured conversation inside this agent (its replies, tool calls, and
    /// tool results), shown when the user navigates into the agent.
    pub transcript: Vec<SubtaskRound>,
    pub output: Option<String>,
    /// Parent transcript message updated in place as this child agent progresses.
    pub message_index: usize,
    pub started_at: std::time::Instant,
    pub duration_ms: Option<u64>,
    /// Aborts the child model loop when the parent round is cancelled.
    pub abort: Option<tokio::task::AbortHandle>,
    /// Resolved named-agent from `[agents]` config, when the call referenced one.
    pub agent: Option<String>,
}

#[derive(Debug)]
pub enum SubtaskEvent {
    /// A child reached a call outside its current read-only/named-agent policy.
    /// The app routes it through the ordinary access prompt and answers the
    /// waiter without bypassing the user's session permission rules.
    AccessRequested {
        id: u64,
        request_id: u64,
        call: ToolCall,
        cwd: PathBuf,
        response: tokio::sync::oneshot::Sender<Result<ToolCall, String>>,
    },
    /// A nested child agent registered itself after being spawned inside another
    /// child agent; the app creates its tree node on receipt.
    Registered {
        id: u64,
        parent_id: u64,
        call: ToolCall,
        description: String,
        prompt: String,
        agent: Option<String>,
        cwd: PathBuf,
    },
    Progress {
        id: u64,
        progress: SubtaskProgress,
    },
    /// One captured conversation line from inside the agent.
    Round {
        id: u64,
        role: SubtaskRoundRole,
        content: String,
    },
    Finished {
        id: u64,
        output: Result<String, String>,
        duration_ms: u64,
    },
}

#[derive(Debug)]
pub struct TaskBarrier {
    pub session_id: usize,
    pub task_ids: Vec<u64>,
}

#[derive(Debug, Clone)]
pub struct QueuedPrompt {
    pub text: String,
    pub attachment: Option<PathBuf>,
}

#[derive(Debug)]
pub struct ChildAccessRequest {
    pub request_id: u64,
    pub task_id: u64,
    pub call: ToolCall,
    pub cwd: PathBuf,
    pub response: tokio::sync::oneshot::Sender<Result<ToolCall, String>>,
}

/// A model stream tagged with the session it belongs to, so several sessions can
/// generate concurrently and events route to the right one regardless of which is
/// currently on screen.
pub struct StreamHandle {
    pub session_id: usize,
    pub rx: mpsc::Receiver<StreamEvent>,
    /// Number of times this exact turn has been restarted before any visible
    /// token/reasoning/tool event arrived.
    pub cold_retries: u8,
}

/// A request to leave the TUI, run an external program, then return. Handled by
/// the main loop (suspend terminal → run → restore).
#[derive(Debug, Clone)]
pub enum PendingExternal {
    /// Open one or more existing files in `$EDITOR`.
    EditorFiles(Vec<PathBuf>),
    /// Write text to a temp file and open it in `$EDITOR` (e.g. the conversation).
    EditorText(String),
    /// Open an already-written temp file in `$EDITOR`, then read its edited
    /// contents back (the file is written before this is set). Used to edit a
    /// pending permission batch; the contents return via `AgentPermissionEdited`.
    EditReadback(PathBuf),
    /// Open an already-written temp file in `$EDITOR`, then read its edited
    /// contents back into the selected ask option.
    DecisionReadback(PathBuf),
    /// Open an already-written temp file in `$EDITOR`, then read it back as the new
    /// session access policy (contents return via `SetAccessPolicy`).
    PolicyReadback(PathBuf),
    /// Open a pre-written temp file in `$EDITOR`, then read it back as an autonomous
    /// loop spec (goal / stop / max), returned via `StartLoopSpec`.
    LoopReadback(PathBuf),
    /// Drop into an interactive `$SHELL`.
    Shell,
}

pub struct App {
    // TODO(audit): split long-lived UI/session/agent/network state into smaller
    // structs so reducer/effects invariants can be tested without constructing all of App.
    pub config: Config,
    pub keymap: Keymap,
    pub sessions: SessionManager,
    pub chat: ChatState,
    /// Per-message rendered-row cache, so streaming only rebuilds changed messages.
    pub doc_cache: crate::render::chat::DocCache,
    pub vim: VimMode,

    pub input: InputBuffer,
    pub command: String,
    pub command_history: Vec<String>,
    pub command_history_idx: Option<usize>,

    /// Previously sent messages (oldest first) for shell-style up/down recall.
    pub input_history: Vec<String>,
    /// Position while browsing `input_history`; None means editing the live draft.
    pub input_history_idx: Option<usize>,
    /// The in-progress text saved when history browsing begins, restored on exit.
    pub input_draft: String,
    pub input_undo: Vec<InputBuffer>,
    pub input_redo: Vec<InputBuffer>,

    pub overlay: Overlay,
    pub mention: Mention,

    /// Stored medium-size pastes, shown in the input as `[PASTED#N-…]` chips and
    /// expanded back to their full text on submit. Index + 1 = the chip's N.
    pub pastes: Vec<String>,

    pub models: Vec<String>,
    pub model_idx: usize,
    /// Whether the `/v1/models` list is still loading, arrived, or failed — drives
    /// the model chip's loading / failed animation.
    pub model_load: ModelLoad,

    pub attachment: Option<PathBuf>,
    pub status: Option<String>,
    /// Timed, bounded warnings/errors rendered as an overlay without consuming
    /// transcript or input layout rows.
    pub toasts: VecDeque<crate::app::toast::Toast>,
    /// Whether the terminal reports AiTUI as focused; desktop notifications are
    /// suppressed while true so permission prompts do not double-notify onscreen.
    pub focused: bool,
    pub notification_tx: std::sync::mpsc::Sender<crate::app::notify::DesktopResponse>,
    pub notification_rx: std::sync::mpsc::Receiver<crate::app::notify::DesktopResponse>,
    pub notification_generation: u64,
    pub show_help: bool,
    pub help_detail: Option<usize>,
    pub help_selected: usize,
    pub help_scroll: usize,
    /// First rendered row in the sidebar task list.
    pub sidebar_task_scroll: usize,
    /// First rendered row in the sidebar agent list.
    pub sidebar_agent_scroll: usize,
    /// Content currently shown in the tabbed lower sidebar.
    pub sidebar_tab: SidebarTab,
    pub should_quit: bool,
    pub yank: Option<String>,
    /// The character just typed in insert mode (for the `jk`-style escape chord).
    /// Reset by any edit/navigation that isn't a consecutive insert.
    pub last_insert: Option<char>,
    /// Show the full output of executed tools (off by default; toggled at runtime).
    pub show_output: bool,
    /// Expand the latest user prompt preview below the header. It remains a
    /// clickable single-line preview when collapsed.
    pub show_last_prompt: bool,
    /// Path to a completed generated image awaiting Kitty/Sixel display.
    pub pending_image: Option<PathBuf>,
    /// Text queued for the system clipboard, flushed once (via OSC 52) by the
    /// renderer — mirrors `pending_image`, keeping raw stdout writes in the UI layer.
    pub pending_clipboard: Option<String>,
    /// Files the agent has created/edited this session (relative paths, most
    /// recent first) — for quick "jump into the edited file" access.
    pub edited_files: Vec<String>,
    /// When set, the main loop suspends the TUI, runs the external program, then
    /// restores. Used to open files/the conversation in `$EDITOR` or a shell.
    pub pending_external: Option<PendingExternal>,

    /// Session ids currently running in another live AiTUI process, refreshed from
    /// per-process heartbeat files alongside session synchronization.
    pub remote_running_sessions: std::collections::HashSet<usize>,
    /// Most recent token usage reported for each session, keyed by stable session id.
    /// Endpoints may omit usage; top bar keeps an explicit pending state in that case.
    pub session_usage: std::collections::HashMap<usize, crate::api::Usage>,

    /// Toggleable instruction snippets loaded from `~/.config/aitui/skills/`.
    /// Active skills are injected as system messages on each request.
    pub skills: Vec<crate::skills::Skill>,

    /// Current free-form reasoning effort, or None to omit it.
    pub reasoning_effort: Option<String>,
    /// Current free-form reasoning mode, or None to omit it.
    pub reasoning_mode: Option<String>,

    /// Bumped whenever chat content/collapse changes, to invalidate the doc cache.
    pub content_rev: u64,

    /// Per-session access state. `permissions` is the active session's working copy;
    /// this map stores inactive sessions' copies.
    pub session_permissions: std::collections::HashMap<usize, PermissionMemory>,
    pub permissions: PermissionMemory,
    pub pending_tools: VecDeque<ToolCall>,
    /// Calls already cleared to run (by the access-policy judge or a batch allow)
    /// this round — drained ahead of `pending_tools` and executed WITHOUT a fresh
    /// permission check, so a judged-allow never re-prompts or re-judges.
    pub approved: VecDeque<ToolCall>,
    /// The batch currently being judged against the session access policy, if any.
    pub judging: Option<JudgeBatch>,
    /// Verdicts stream back here from the async judge task, tagged with the session.
    pub judge_rx: Option<mpsc::Receiver<(usize, Vec<AccessVerdict>)>>,
    /// Abort handle for the live review-model request, allowing review to be
    /// disabled without waiting for the network request to finish.
    pub judge_task: Option<tokio::task::JoinHandle<()>>,
    pub agent_iterations: usize,
    /// Which session the in-progress agent tool round belongs to (rounds are
    /// serialized; a background session that finishes needing tools waits its turn).
    pub agent_session: Option<usize>,
    /// Sessions whose finished stream has tool calls to run, waiting for the
    /// current agent round to free up (parallel sessions share one tool loop).
    pub agent_queue: std::collections::VecDeque<usize>,
    /// User prompts explicitly queued while a main-agent turn was running.
    pub queued_prompts: std::collections::HashMap<usize, VecDeque<QueuedPrompt>>,

    /// Concurrent model streams, each tagged with the session it writes to, so a
    /// background session keeps generating while you work in another (parallel).
    pub streams: Vec<StreamHandle>,
    pub agent_tool_rx: Option<mpsc::Receiver<ToolResult>>,
    pub agent_tool_batch_rx: Option<mpsc::Receiver<Vec<ToolResult>>>,
    /// Shared abort flag for the in-flight tool round; set by `AgentCancel` so a
    /// running tool (notably a shell) stops side effects instead of only having
    /// its result dropped. Replaced by a fresh flag per round.
    pub agent_abort: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Parallel child-agent runs for the active parent tool round.
    pub subtasks: Vec<Subtask>,
    /// Highlighted child agent in the activity-row tab strip.
    pub selected_subtask: Option<u64>,
    /// Child agent the user is currently viewing inside (chat + sidebar show
    /// that agent's own content); `None` shows the root (UI-session) chat.
    pub view_node: Option<u64>,
    /// Unique child-agent id allocator shared between the app (top-level
    /// children) and nested children spawned inside child agents.
    pub subtask_id_alloc: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub task_barrier: Option<TaskBarrier>,
    pub subtask_tx: mpsc::Sender<SubtaskEvent>,
    pub subtask_rx: mpsc::Receiver<SubtaskEvent>,
    /// Restricted child calls waiting for the shared permission UI. Only one is
    /// displayed at a time; the rest remain blocked in launch order.
    pub child_access_queue: VecDeque<ChildAccessRequest>,
    /// The tool currently executing, for the transcript header animation.
    pub active_tool: Option<(String, std::time::Instant)>,
    /// The tool call the model is currently assembling natively, shown inline
    /// beneath the live assistant turn instead of in the status bar.
    pub preparing_tool: Option<(usize, String, std::time::Instant)>,
    pub models_rx: Option<oneshot::Receiver<anyhow::Result<Vec<String>>>>,
    pub title_rx: Option<mpsc::Receiver<(usize, String)>>,
    /// Shared result channel for optional per-session response suggestions.
    pub suggestion_tx: mpsc::Sender<(usize, u64, Vec<String>)>,
    pub suggestion_rx: mpsc::Receiver<(usize, u64, Vec<String>)>,
    /// Turn signatures already being suggested, preventing duplicate requests when
    /// a stream emits both Done and channel-disconnect completion signals.
    pub suggestion_inflight: std::collections::HashSet<(usize, u64)>,
    /// Shared result channel for the parallel task-tracker agent, which updates
    /// the checklist after each completed response.
    pub todo_tx: mpsc::Sender<(usize, u64, Result<crate::app::state::TodoUpdate, String>)>,
    pub todo_rx: mpsc::Receiver<(usize, u64, Result<crate::app::state::TodoUpdate, String>)>,
    /// Signatures already being tracked, preventing duplicate tracker calls when
    /// a stream emits both Done and channel-disconnect completion signals.
    pub todo_inflight: std::collections::HashMap<(usize, u64), u64>,
    pub memory_tx: mpsc::Sender<(
        usize,
        u64,
        Result<Vec<crate::app::memory::MemoryOperation>, String>,
    )>,
    pub memory_rx: mpsc::Receiver<(
        usize,
        u64,
        Result<Vec<crate::app::memory::MemoryOperation>, String>,
    )>,
    pub memory_inflight: std::collections::HashSet<usize>,
    pub memory_pending: std::collections::HashSet<usize>,

    /// Speculative tool execution: while an agent-mode reply streams, complete
    /// read-only tool blocks are pre-run in the background so their results are
    /// ready the instant the turn finishes. Results are keyed by `hash(name,args)`.
    pub spec_results: std::collections::HashMap<u64, ToolResult>,
    /// Call signatures already dispatched speculatively this turn (dedup guard).
    pub spec_dispatched: std::collections::HashSet<u64>,
    /// Bumped every turn (`begin_stream_for`); tags each speculative task so a
    /// result that lands after the turn moved on is dropped instead of served stale.
    pub spec_epoch: u64,
    /// Limits concurrent speculative tool executions. Atomic counter: speculative
    /// tasks increment on spawn and decrement on completion. When at cap, new
    /// speculative work is skipped until existing tasks finish.
    pub spec_inflight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Set when an agent-mode stream is cut early (a complete tool call appeared
    /// mid-generation). The main loop drains it *after* the batch so the tool round
    /// starts on a clean pass — no leftover tokens land in the next stream.
    pub cut_stream: Option<usize>,
    /// Sender cloned into each speculative exec task (tagged with its epoch);
    /// results drained via `spec_rx`.
    pub spec_tx: mpsc::Sender<(u64, ToolResult)>,
    pub spec_rx: mpsc::Receiver<(u64, ToolResult)>,

    /// Cached project file list for `@`-mention completion, refreshed lazily so
    /// typing `@` doesn't walk the filesystem on every keystroke.
    pub mention_files: Vec<String>,
    pub mention_files_at: Option<std::time::Instant>,
    pub mention_files_root: Option<PathBuf>,

    /// Mouse-driven text selection state. None when idle; Some when the user is
    /// dragging to select transcript text.
    pub mouse_select: Option<MouseSelection>,

    pub layout: PanelLayout,
    pub(crate) api: Option<ApiClient>,
}

/// Active mouse selection in the transcript (drag-to-select).
#[derive(Debug, Clone, Copy)]
pub struct MouseSelection {
    pub anchor_col: u16,
    pub anchor_row: u16,
    pub drag_row: u16,
    pub active: bool,
    /// True after any drag event in this press/release gesture. This suppresses
    /// click activation even when the pointer leaves the transcript.
    pub dragged: bool,
}

/// Runaway loop guard. Effectively unlimited: the assistant is free to take as
/// many tool rounds as it needs. Kept at the ceiling only so a truly pathological
/// infinite loop still can't overflow the counter (Ctrl-C cancels a round anyway).
pub const MAX_AGENT_ITERATIONS: usize = usize::MAX;

fn optional_reasoning_value(value: &str) -> Option<String> {
    match value.trim() {
        "" => None,
        value if value.eq_ignore_ascii_case("off") || value.eq_ignore_ascii_case("none") => None,
        value => Some(value.to_string()),
    }
}

fn relative_prompt_time(timestamp: Option<u64>) -> String {
    let Some(timestamp) = timestamp else {
        return "never".to_string();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(timestamp);
    let elapsed = now.saturating_sub(timestamp);
    match elapsed {
        0..=59 => "just now".to_string(),
        60..=3_599 => format!("{}m ago", elapsed / 60),
        3_600..=86_399 => format!("{}h ago", elapsed / 3_600),
        86_400..=604_799 => format!("{}d ago", elapsed / 86_400),
        _ => format!("{}w ago", elapsed / 604_800),
    }
}

impl App {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        crate::agent::configure_search(crate::agent::SearchSettings {
            provider: config.search.provider.clone(),
            searxng_url: config.search.searxng_url.clone(),
        });

        // Force offline mock: explicit config flag, the AITUI_MOCK env var, or an
        // empty endpoint (nothing to talk to). Mock is now just a model, so this
        // simply means "start on the `mock` model and skip the fetch".
        let force_mock = config.api.mock
            || std::env::var("AITUI_MOCK")
                .map(|v| !v.is_empty() && v != "0")
                .unwrap_or(false)
            || config.api.endpoint.trim().is_empty();

        let api = ApiClient::new(&config.api.endpoint, &config.api.api_key)?;

        // Fetch the real model list from a live endpoint. Until it arrives the list
        // is empty and `model_load` is Loading (the model chip shows a spinner). If
        // forced offline, we skip the fetch and go straight to the mock-only list.
        let (models_tx, models_rx) = oneshot::channel();
        let (models, model_idx, model_load) = if force_mock {
            drop(models_tx);
            (vec![MOCK_MODEL.to_string()], 0, ModelLoad::Loaded)
        } else {
            let fetch = ApiClient::new(&config.api.endpoint, &config.api.api_key)?;
            tokio::spawn(async move {
                let _ = models_tx.send(fetch.fetch_models().await);
            });
            (Vec::new(), 0, ModelLoad::Loading)
        };

        let keymap = Keymap::from_config(&config.keybinds);
        let reasoning_effort = optional_reasoning_value(&config.api.reasoning_effort);
        let reasoning_mode = optional_reasoning_value(&config.api.reasoning_mode);
        let (spec_tx, spec_rx) = mpsc::channel(64);
        let (suggestion_tx, suggestion_rx) = mpsc::channel(32);
        let (todo_tx, todo_rx) = mpsc::channel(32);
        let (memory_tx, memory_rx) = mpsc::channel(32);
        let (subtask_tx, subtask_rx) = mpsc::channel(64);
        let (notification_tx, notification_rx) = std::sync::mpsc::channel();
        let mut app = Self {
            config,
            keymap,
            sessions: SessionManager::load(),
            chat: ChatState::new(),
            doc_cache: crate::render::chat::DocCache::default(),
            vim: VimMode::Normal,
            input: InputBuffer::default(),
            command: String::new(),
            command_history: Vec::new(),
            command_history_idx: None,
            input_history: Vec::new(),
            input_history_idx: None,
            input_draft: String::new(),
            input_undo: Vec::new(),
            input_redo: Vec::new(),
            overlay: Overlay::None,
            mention: Mention::default(),
            pastes: Vec::new(),
            models,
            model_idx,
            model_load,
            attachment: None,
            status: Some(if model_load == ModelLoad::Loaded {
                "i = insert  ·  @ = file  ·  / = commands  ·  :w = send  ·  ? = help".into()
            } else {
                "Loading models…".into()
            }),
            toasts: VecDeque::new(),
            focused: true,
            notification_tx,
            notification_rx,
            notification_generation: 0,
            show_help: false,
            help_detail: None,
            help_selected: 0,
            help_scroll: 0,
            sidebar_task_scroll: 0,
            sidebar_agent_scroll: 0,
            sidebar_tab: SidebarTab::Tasks,
            should_quit: false,
            yank: None,
            last_insert: None,
            show_output: false,
            show_last_prompt: false,
            pending_image: None,
            pending_clipboard: None,
            edited_files: Vec::new(),
            pending_external: None,
            remote_running_sessions: std::collections::HashSet::new(),
            session_usage: std::collections::HashMap::new(),
            skills: crate::skills::load(),
            reasoning_effort,
            reasoning_mode,
            content_rev: 0,
            session_permissions: std::collections::HashMap::new(),
            permissions: PermissionMemory::default(),
            pending_tools: VecDeque::new(),
            approved: VecDeque::new(),
            judging: None,
            judge_rx: None,
            judge_task: None,
            agent_iterations: 0,
            streams: Vec::new(),
            agent_session: None,
            agent_queue: std::collections::VecDeque::new(),
            queued_prompts: std::collections::HashMap::new(),
            agent_tool_rx: None,
            agent_abort: None,
            agent_tool_batch_rx: None,
            subtasks: Vec::new(),
            selected_subtask: None,
            view_node: None,
            subtask_id_alloc: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1)),
            task_barrier: None,
            subtask_tx,
            subtask_rx,
            child_access_queue: VecDeque::new(),
            active_tool: None,
            preparing_tool: None,
            models_rx: Some(models_rx),
            title_rx: None,
            suggestion_tx,
            suggestion_rx,
            suggestion_inflight: std::collections::HashSet::new(),
            todo_tx,
            todo_rx,
            todo_inflight: std::collections::HashMap::new(),
            memory_tx,
            memory_rx,
            memory_inflight: std::collections::HashSet::new(),
            memory_pending: std::collections::HashSet::new(),
            spec_results: std::collections::HashMap::new(),
            spec_dispatched: std::collections::HashSet::new(),
            spec_epoch: 0,
            spec_inflight: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            cut_stream: None,
            spec_tx,
            spec_rx,
            mention_files: Vec::new(),
            mention_files_at: None,
            mention_files_root: None,
            mouse_select: None,
            layout: PanelLayout::default(),
            api: Some(api),
        };

        sync_auto_approvals(&mut app.permissions, app.config.ui.auto_approve_reads);

        // Show the launch screen when there is any non-empty session to resume,
        // so the user can pick up a past conversation (and `cd` to its folder) or
        // start fresh. A clean first run drops straight into an empty session.
        let resumable = app.sessions.all().iter().any(|s| !s.messages.is_empty());
        if resumable {
            app.overlay = Overlay::Picker(crate::app::overlay::Picker::sessions(
                app.session_items(),
                app.sessions.active_idx() + 1,
            ));
        } else {
            // No launch screen: we drop straight into the active session, so it
            // should operate where the binary was launched — not the stale folder
            // a previous run saved. (Resuming via the launch screen `cd`s on
            // purpose; that path is unaffected.)
            if let Ok(cwd) = std::env::current_dir() {
                app.sessions.active_mut().cwd = Some(cwd);
            }
        }
        Ok(app)
    }

    pub fn theme(&self) -> Theme {
        Theme::named(&self.config.ui.theme)
    }

    /// Geometry + label of the "↓ N below" jump pill (bottom-right of the chat
    /// pane), or `None` when the tail is already visible so no pill shows. Shared by
    /// the renderer (to draw it) and the click handler (to hit-test it), so the two
    /// can never drift out of sync.
    pub fn jump_pill(&self) -> Option<(ratatui::layout::Rect, String)> {
        let chat = self.layout.chat;
        if chat.height == 0 {
            return None;
        }
        let hidden = self.chat.rows_below(chat.height as usize);
        if hidden == 0 {
            return None;
        }
        let label = format!(
            " ↓ {} below · {} ",
            hidden,
            self.keymap.scroll_bottom.label()
        );
        let w = (label.chars().count() as u16).min(chat.width);
        if w == 0 {
            return None;
        }
        let rect = ratatui::layout::Rect {
            x: chat.x + chat.width.saturating_sub(w),
            y: chat.y + chat.height - 1,
            width: w,
            height: 1,
        };
        Some((rect, label))
    }

    pub fn current_model(&self) -> &str {
        self.models
            .get(self.model_idx)
            .map(|s| s.as_str())
            .unwrap_or(MOCK_MODEL)
    }

    pub fn session_items(&self) -> Vec<String> {
        let cwd = std::env::current_dir()
            .map(|path| crate::render::path::display_path(&path))
            .unwrap_or_else(|_| "—".to_string());
        let local_running: std::collections::HashSet<usize> =
            self.running_session_ids().into_iter().collect();
        let mut items = Vec::with_capacity(self.sessions.all().len() + 1);
        items.push(format!("＋  New session  ·  cwd {}", cwd));
        for (index, session) in self.sessions.all().iter().enumerate() {
            let cwd = session
                .cwd
                .as_ref()
                .map(|path| crate::render::path::display_path(path))
                .unwrap_or_else(|| "—".to_string());
            let state = if local_running.contains(&session.id) {
                "RUNNING here"
            } else if self.remote_running_sessions.contains(&session.id) {
                "RUNNING elsewhere"
            } else {
                "idle"
            };
            let marker = if index == self.sessions.active_idx() {
                "●"
            } else {
                "○"
            };
            items.push(format!(
                "{}  {}  ·  {}  ·  last {}  ·  cwd {}  ·  {} msg",
                marker,
                session.name,
                state,
                relative_prompt_time(session.last_prompt_at),
                cwd,
                session.messages.len()
            ));
        }
        items
    }

    /// Whether the selected model is the offline mock backend. Mock is just a model
    /// now, so "mock mode" is simply having it selected.
    pub fn is_mock(&self) -> bool {
        self.current_model() == MOCK_MODEL
    }

    pub fn running_session_ids(&self) -> Vec<usize> {
        let mut ids: std::collections::HashSet<usize> = self
            .streams
            .iter()
            .map(|stream| stream.session_id)
            .collect();
        if let Some(id) = self.agent_session {
            ids.insert(id);
        }
        if let Some(judge) = &self.judging {
            ids.insert(judge.session_id);
        }
        for task in self
            .subtasks
            .iter()
            .filter(|task| task.status == SubtaskStatus::Running)
        {
            ids.insert(task.session_id);
        }
        let mut ids: Vec<usize> = ids.into_iter().collect();
        ids.sort_unstable();
        ids
    }

    /// Whether the **active** session is mid-turn: streaming a reply, running its
    /// agent tool round, or waiting on a permission prompt. Blocks a second send
    /// *in that session* — but other sessions can stream in parallel, and the input
    /// box stays editable so a follow-up can be composed ahead of time.
    pub fn child_agents_only_busy(&self) -> bool {
        let active = self.sessions.active_id();
        let children_running = self.task_barrier.as_ref().is_some_and(|barrier| {
            barrier.session_id == active
                && barrier.task_ids.iter().any(|id| {
                    self.subtasks
                        .iter()
                        .any(|task| task.id == *id && task.status == SubtaskStatus::Running)
                })
        });
        children_running && !self.main_agent_busy_for(active)
    }

    pub fn main_agent_busy_for(&self, sid: usize) -> bool {
        self.sessions
            .by_id(sid)
            .is_some_and(|session| session.is_streaming())
            || self.streams.iter().any(|stream| stream.session_id == sid)
            || self
                .judging
                .as_ref()
                .is_some_and(|judge| judge.session_id == sid)
            || (self.agent_session == Some(sid)
                && (self.agent_tool_rx.is_some()
                    || self.agent_tool_batch_rx.is_some()
                    || !self.pending_tools.is_empty()
                    || !self.approved.is_empty()))
            || matches!(
                self.overlay,
                Overlay::Permission(_) | Overlay::Decision(_) | Overlay::Plan(_)
            )
    }

    pub fn session_busy(&self, sid: usize) -> bool {
        self.main_agent_busy_for(sid)
            || self
                .task_barrier
                .as_ref()
                .is_some_and(|barrier| barrier.session_id == sid)
    }

    pub fn is_busy(&self) -> bool {
        self.session_busy(self.sessions.active_id())
    }

    /// Invalidate the chat document cache (content or collapse changed).
    pub fn touch(&mut self) {
        self.content_rev = self.content_rev.wrapping_add(1);
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some(msg.into());
    }

    pub fn save_config(&self) {
        if let Err(error) = self.config.save() {
            crate::app::toast::error(format!("Failed to save configuration: {}", error));
        }
    }

    pub fn stash_active_permissions(&mut self) {
        let sid = self.sessions.active_id();
        self.session_permissions
            .insert(sid, self.permissions.clone());
    }

    pub fn load_active_permissions(&mut self) {
        let sid = self.sessions.active_id();
        self.permissions = self.session_permissions.remove(&sid).unwrap_or_default();
        sync_auto_approvals(&mut self.permissions, self.config.ui.auto_approve_reads);
    }

    // ── @ mention completion ────────────────────────────────────────────────

    /// Re-evaluate whether the cursor sits inside an `@token` and refresh matches.
    pub fn update_mention(&mut self) {
        if self.vim != VimMode::Insert {
            self.mention.reset();
            return;
        }
        let line = self.input.current_line();
        let chars: Vec<char> = line.chars().collect();
        let cur = self.input.col.min(chars.len());
        let mut i = cur;
        let mut at = None;
        while i > 0 {
            let ch = chars[i - 1];
            if ch == '@' {
                if i == 1 || chars[i - 2].is_whitespace() {
                    at = Some(i - 1);
                }
                break;
            }
            if ch.is_whitespace() {
                break;
            }
            i -= 1;
        }
        match at {
            Some(idx) if idx < cur => {
                self.mention.active = true;
                self.mention.anchor_row = self.input.row;
                self.mention.anchor_col = idx;
                self.mention.query = chars[idx + 1..cur].iter().collect();
                self.refresh_mention_matches();
            }
            Some(idx) if idx == cur.saturating_sub(1) => {
                self.mention.active = true;
                self.mention.anchor_row = self.input.row;
                self.mention.anchor_col = idx;
                self.mention.query.clear();
                self.refresh_mention_matches();
            }
            _ => self.mention.reset(),
        }
    }

    fn refresh_mention_matches(&mut self) {
        self.ensure_mention_files();
        let q = self.mention.query.to_lowercase();
        let mut scored: Vec<(usize, &String)> = self
            .mention_files
            .iter()
            .filter_map(|f| fuzzy_score(&f.to_lowercase(), &q).map(|s| (s, f)))
            .collect();
        scored.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.1.len().cmp(&b.1.len()))
                .then(a.1.cmp(b.1))
        });
        self.mention.matches = scored
            .into_iter()
            .take(50)
            .map(|(_, f)| f.clone())
            .collect();
        if self.mention.selected >= self.mention.matches.len() {
            self.mention.selected = 0;
        }
    }

    /// Refresh the cached project file list if it's missing or older than ~5s, so
    /// `@`-mention completion filters an in-memory list instead of walking the
    /// filesystem on every keystroke.
    fn ensure_mention_files(&mut self) {
        let root = self
            .sessions
            .active()
            .cwd
            .clone()
            .or_else(|| std::env::current_dir().ok());
        let stale = self.mention_files_root != root
            || self
                .mention_files_at
                .map(|t| t.elapsed() > std::time::Duration::from_secs(5))
                .unwrap_or(true);
        if stale {
            self.mention_files = root
                .as_deref()
                .map(|root| find_project_files(root, 4000))
                .unwrap_or_default();
            self.mention_files_at = Some(std::time::Instant::now());
            self.mention_files_root = root;
        }
    }

    pub fn accept_mention(&mut self) {
        let path = match self.mention.matches.get(self.mention.selected).cloned() {
            Some(p) => p,
            None => {
                self.mention.reset();
                return;
            }
        };
        let row = self.mention.anchor_row;
        if row >= self.input.lines.len() {
            self.mention.reset();
            return;
        }
        let chars: Vec<char> = self.input.lines[row].chars().collect();
        let start = self.mention.anchor_col;
        let end = self.input.col.min(chars.len());
        let mut new: String = chars[..start].iter().collect();
        new.push('@');
        new.push_str(&path);
        new.push(' ');
        let col = new.chars().count();
        new.push_str(&chars[end..].iter().collect::<String>());
        self.input.lines[row] = new;
        self.input.col = col;
        self.set_status(format!("Added @{}", path));
        self.mention.reset();
    }
}

// ── Free helpers ────────────────────────────────────────────────────────────

/// The offline mock backend, exposed as a selectable model. Always present in the
/// list as the fallback when no real models exist or the endpoint is unreachable.
pub const MOCK_MODEL: &str = "mock";

/// Expand `@path` mentions in `text` into inline file-context blocks resolved
/// relative to the active session's project root.
pub fn expand_mentions(text: &str, root: &std::path::Path) -> String {
    let mut paths: Vec<String> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let at_start = i == 0 || chars[i - 1].is_whitespace();
        if chars[i] == '@' && at_start {
            let mut j = i + 1;
            while j < chars.len() && !chars[j].is_whitespace() {
                j += 1;
            }
            let token: String = chars[i + 1..j].iter().collect();
            let token = token.trim_end_matches(['.', ',', ')', ':', ';']);
            if !token.is_empty() && !paths.iter().any(|p| p == token) {
                paths.push(token.to_string());
            }
            i = j;
        } else {
            i += 1;
        }
    }
    let mut blocks = Vec::new();
    for p in paths {
        if let Some(path) = resolve_mention_path(root, &p) {
            if let Ok(content) = crate::files::read_text(&path) {
                let capped: String = content.chars().take(100_000).collect();
                blocks.push(format!("File: {}\n```\n{}\n```", p, capped));
            }
        }
    }
    blocks.join("\n\n")
}

fn resolve_mention_path(root: &std::path::Path, token: &str) -> Option<PathBuf> {
    let root = std::fs::canonicalize(root).ok()?;
    let path = std::fs::canonicalize(root.join(token)).ok()?;
    path.is_file()
        .then_some(path)
        .filter(|path| path.starts_with(root))
}

/// Subsequence fuzzy score (lower = better); None if not a subsequence.
pub fn fuzzy_score(text: &str, query: &str) -> Option<usize> {
    if query.is_empty() {
        return Some(text.len());
    }
    let t: Vec<char> = text.chars().collect();
    let q: Vec<char> = query.chars().collect();
    let (mut ti, mut qi) = (0, 0);
    let mut first = None;
    let mut last = 0;
    while ti < t.len() && qi < q.len() {
        if t[ti] == q[qi] {
            if first.is_none() {
                first = Some(ti);
            }
            last = ti;
            qi += 1;
        }
        ti += 1;
    }
    if qi == q.len() {
        Some((last - first.unwrap_or(0)) * 4 + first.unwrap_or(0))
    } else {
        None
    }
}

/// Recursively list project files relative to `root` for `@` completion.
pub fn find_project_files(root: &std::path::Path, max: usize) -> Vec<String> {
    if max == 0 || !root.is_dir() {
        return Vec::new();
    }
    let root = root.to_path_buf();
    let mut out: Vec<String> = Vec::new();
    let mut stack = vec![root.clone()];
    let mut visited = 0usize;
    let visit_limit = max.saturating_mul(16).max(max);
    let skip = [
        ".git",
        "target",
        "node_modules",
        ".cache",
        "dist",
        "build",
        ".next",
        ".venv",
        "venv",
        "__pycache__",
    ];
    while let Some(dir) = stack.pop() {
        if out.len() >= max {
            break;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > visit_limit {
                stack.clear();
                break;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if name.starts_with('.') || skip.contains(&name.as_str()) {
                    continue;
                }
                if entry.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
                    continue;
                }
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(&root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
                if out.len() >= max {
                    break;
                }
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_status_parses_and_defaults_to_pending() {
        assert_eq!(TodoStatus::parse("in_progress"), TodoStatus::InProgress);
        assert_eq!(TodoStatus::parse("DONE"), TodoStatus::Done);
        assert_eq!(TodoStatus::parse("completed"), TodoStatus::Done);
        assert_eq!(TodoStatus::parse("whatever"), TodoStatus::Pending);
        assert_eq!(TodoStatus::parse(""), TodoStatus::Pending);
    }

    #[test]
    fn fuzzy_matches_subsequence() {
        assert!(fuzzy_score("src/main.rs", "main").is_some());
        assert!(fuzzy_score("src/main.rs", "xyz").is_none());
    }

    #[test]
    fn fuzzy_prefers_tighter() {
        let tight = fuzzy_score("main.rs", "main").unwrap();
        let loose = fuzzy_score("m_a_i_n.rs", "main").unwrap();
        assert!(tight < loose);
    }

    #[test]
    fn mention_discovery_and_expansion_use_the_explicit_project_root() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("aitui_mentions_{}_{}", std::process::id(), unique));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        assert_eq!(find_project_files(&root, 10), vec!["src/main.rs"]);
        assert!(expand_mentions("see @src/main.rs", &root).contains("fn main() {}"));
        assert_eq!(expand_mentions("see @../outside.txt", &root), "");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn expand_mentions_ignores_missing_files() {
        assert_eq!(
            expand_mentions(
                "see @does_not_exist_xyz.txt here",
                std::path::Path::new(".")
            ),
            ""
        );
    }
}
