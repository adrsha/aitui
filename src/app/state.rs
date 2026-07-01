//! The application state container plus pure helpers (mention completion, file
//! walking). Mutations live in `reducer.rs`; side effects in `effects.rs`.

use std::collections::VecDeque;
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};

use crate::agent::{PermissionMemory, ToolCall, ToolResult};
use crate::api::{ApiClient, StreamEvent};
use crate::app::input_buffer::InputBuffer;
use crate::app::overlay::{sync_auto_approvals, Mention, Overlay};
use crate::config::Config;
use crate::domain::session::SessionManager;
use crate::input::keymap::Keymap;
use crate::input::vim::VimMode;
use crate::render::chat::ChatState;
use crate::render::theme::Theme;

#[derive(Debug, Clone, Copy, Default)]
pub struct PanelLayout {
    /// The transcript rect, cached so the reducer can compute page heights.
    pub chat: ratatui::layout::Rect,
}

/// A model stream tagged with the session it belongs to, so several sessions can
/// generate concurrently and events route to the right one regardless of which is
/// currently on screen.
pub struct StreamHandle {
    pub session_id: usize,
    pub rx: mpsc::Receiver<StreamEvent>,
}

/// A request to leave the TUI, run an external program, then return. Handled by
/// the main loop (suspend terminal → run → restore).
#[derive(Debug, Clone)]
pub enum PendingExternal {
    /// Open one or more existing files in `$EDITOR`.
    EditorFiles(Vec<PathBuf>),
    /// Write text to a temp file and open it in `$EDITOR` (e.g. the conversation).
    EditorText(String),
    /// Drop into an interactive `$SHELL`.
    Shell,
}

pub struct App {
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

    pub overlay: Overlay,
    pub mention: Mention,

    /// Stored medium-size pastes, shown in the input as `[PASTED#N-…]` chips and
    /// expanded back to their full text on submit. Index + 1 = the chip's N.
    pub pastes: Vec<String>,

    pub models: Vec<String>,
    pub model_idx: usize,

    pub attachment: Option<PathBuf>,
    pub status: Option<String>,
    pub show_help: bool,
    pub should_quit: bool,
    pub yank: Option<String>,
    /// The character just typed in insert mode (for the `jk`-style escape chord).
    /// Reset by any edit/navigation that isn't a consecutive insert.
    pub last_insert: Option<char>,
    /// Show the full output of executed tools (off by default; toggled at runtime).
    pub show_output: bool,
    /// Offline mock backend: drive the agent tools without a live API.
    pub mock: bool,
    /// Files the agent has created/edited this session (relative paths, most
    /// recent first) — for quick "jump into the edited file" access.
    pub edited_files: Vec<String>,
    /// When set, the main loop suspends the TUI, runs the external program, then
    /// restores. Used to open files/the conversation in `$EDITOR` or a shell.
    pub pending_external: Option<PendingExternal>,

    /// Token usage from the most recent completed response, shown top-right.
    /// `None` until the endpoint reports usage (some servers never do).
    pub usage: Option<crate::api::Usage>,

    /// Toggleable instruction snippets loaded from `~/.config/aitui/skills/`.
    /// Active skills are injected as system messages on each request.
    pub skills: Vec<crate::skills::Skill>,

    /// Current reasoning effort ("low"/"medium"/"high"), or None to omit it.
    /// Cycled with `:effort`; sent to reasoning-capable models.
    pub reasoning_effort: Option<String>,

    /// Bumped whenever chat content/collapse changes, to invalidate the doc cache.
    pub content_rev: u64,

    pub permissions: PermissionMemory,
    pub pending_tools: VecDeque<ToolCall>,
    pub agent_iterations: usize,
    /// Which session the in-progress agent tool round belongs to (rounds are
    /// serialized; a background session that finishes needing tools waits its turn).
    pub agent_session: Option<usize>,
    /// Sessions whose finished stream has tool calls to run, waiting for the
    /// current agent round to free up (parallel sessions share one tool loop).
    pub agent_queue: std::collections::VecDeque<usize>,

    /// Concurrent model streams, each tagged with the session it writes to, so a
    /// background session keeps generating while you work in another (parallel).
    pub streams: Vec<StreamHandle>,
    pub agent_tool_rx: Option<mpsc::Receiver<ToolResult>>,
    pub models_rx: Option<oneshot::Receiver<anyhow::Result<Vec<String>>>>,

    /// Speculative tool execution: while an agent-mode reply streams, complete
    /// read-only tool blocks are pre-run in the background so their results are
    /// ready the instant the turn finishes. Results are keyed by `hash(name,args)`.
    pub spec_results: std::collections::HashMap<u64, ToolResult>,
    /// Call signatures already dispatched speculatively this turn (dedup guard).
    pub spec_dispatched: std::collections::HashSet<u64>,
    /// Bumped every turn (`begin_stream_for`); tags each speculative task so a
    /// result that lands after the turn moved on is dropped instead of served stale.
    pub spec_epoch: u64,
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

    pub layout: PanelLayout,
    pub(crate) api: Option<ApiClient>,
}

pub const MAX_AGENT_ITERATIONS: usize = 25;

impl App {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        // Mock mode: explicit config flag, the AITUI_MOCK env var, or an empty
        // endpoint (nothing to talk to). Lets every agent feature be tested offline.
        let mock = config.api.mock
            || std::env::var("AITUI_MOCK").map(|v| !v.is_empty() && v != "0").unwrap_or(false)
            || config.api.endpoint.trim().is_empty();

        let api = ApiClient::new(&config.api.endpoint, &config.api.api_key)?;

        // Only fetch the model list from a real endpoint.
        let (models_tx, models_rx) = oneshot::channel();
        if !mock {
            let fetch = ApiClient::new(&config.api.endpoint, &config.api.api_key)?;
            tokio::spawn(async move {
                let _ = models_tx.send(fetch.fetch_models().await);
            });
        } else {
            drop(models_tx); // no fetch; receiver resolves to Closed and is ignored
        }

        // Seed the model list with the configured default so it is selected and
        // shown even before the real `/v1/models` list arrives (or if the default
        // isn't in the built-in fallback list).
        let mut models = default_models();
        if !config.api.default_model.is_empty() && !models.contains(&config.api.default_model) {
            models.insert(0, config.api.default_model.clone());
        }
        let model_idx = models.iter().position(|m| m == &config.api.default_model).unwrap_or(0);

        let keymap = Keymap::from_config(&config.keybinds);
        let reasoning_effort = match config.api.reasoning_effort.trim() {
            "" => None,
            e => Some(e.to_string()),
        };
        let (spec_tx, spec_rx) = mpsc::channel(64);
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
            overlay: Overlay::None,
            mention: Mention::default(),
            pastes: Vec::new(),
            models,
            model_idx,
            attachment: None,
            status: Some(if mock {
                "⚗ MOCK mode — type 'help' then :w to drive the agent offline  ·  ? = keys".into()
            } else {
                "i = insert  ·  @ = file  ·  / = commands  ·  :w = send  ·  ? = help".into()
            }),
            show_help: false,
            should_quit: false,
            yank: None,
            last_insert: None,
            show_output: false,
            mock,
            edited_files: Vec::new(),
            pending_external: None,
            usage: None,
            skills: crate::skills::load(),
            reasoning_effort,
            content_rev: 0,
            permissions: PermissionMemory::default(),
            pending_tools: VecDeque::new(),
            agent_iterations: 0,
            streams: Vec::new(),
            agent_session: None,
            agent_queue: std::collections::VecDeque::new(),
            agent_tool_rx: None,
            models_rx: Some(models_rx),
            spec_results: std::collections::HashMap::new(),
            spec_dispatched: std::collections::HashSet::new(),
            spec_epoch: 0,
            cut_stream: None,
            spec_tx,
            spec_rx,
            mention_files: Vec::new(),
            mention_files_at: None,
            layout: PanelLayout::default(),
            api: Some(api),
        };

        if app.config.ui.agent_default {
            // Apply to all sessions, not just the active one — loaded sessions
            // default to agent-off, which would silently ignore tool calls.
            app.sessions.set_agent_mode_all(true);
        }
        sync_auto_approvals(&mut app.permissions, app.config.ui.auto_approve_reads);

        // Show the launch screen when there is any non-empty session to resume,
        // so the user can pick up a past conversation (and `cd` to its folder) or
        // start fresh. A clean first run drops straight into an empty session.
        let resumable = app.sessions.all().iter().any(|s| !s.messages.is_empty());
        if resumable {
            let n = app.sessions.all().len();
            app.overlay = Overlay::Startup(crate::app::overlay::Startup::new(n));
        }
        Ok(app)
    }

    pub fn theme(&self) -> Theme {
        Theme::default()
    }

    pub fn current_model(&self) -> &str {
        self.models.get(self.model_idx).map(|s| s.as_str()).unwrap_or("unknown")
    }

    /// Whether the **active** session is mid-turn: streaming a reply, running its
    /// agent tool round, or waiting on a permission prompt. Blocks a second send
    /// *in that session* — but other sessions can stream in parallel, and the input
    /// box stays editable so a follow-up can be composed ahead of time.
    pub fn is_busy(&self) -> bool {
        let active = self.sessions.active_id();
        self.sessions.active().is_streaming()
            || self.streams.iter().any(|s| s.session_id == active)
            || (self.agent_session == Some(active)
                && (self.agent_tool_rx.is_some() || !self.pending_tools.is_empty()))
            || matches!(self.overlay, Overlay::Permission(_))
    }

    /// Whether *any* session is currently generating (used for the busy spinner).
    pub fn any_busy(&self) -> bool {
        !self.streams.is_empty()
            || self.agent_tool_rx.is_some()
            || !self.pending_tools.is_empty()
    }

    /// Invalidate the chat document cache (content or collapse changed).
    pub fn touch(&mut self) {
        self.content_rev = self.content_rev.wrapping_add(1);
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some(msg.into());
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
            Some(idx) => {
                self.mention.active = true;
                self.mention.anchor_row = self.input.row;
                self.mention.anchor_col = idx;
                self.mention.query = chars[idx + 1..cur].iter().collect();
                self.refresh_mention_matches();
            }
            None => self.mention.reset(),
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
        scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.len().cmp(&b.1.len())).then(a.1.cmp(b.1)));
        self.mention.matches = scored.into_iter().take(50).map(|(_, f)| f.clone()).collect();
        if self.mention.selected >= self.mention.matches.len() {
            self.mention.selected = 0;
        }
    }

    /// Refresh the cached project file list if it's missing or older than ~5s, so
    /// `@`-mention completion filters an in-memory list instead of walking the
    /// filesystem on every keystroke.
    fn ensure_mention_files(&mut self) {
        let stale = self
            .mention_files_at
            .map(|t| t.elapsed() > std::time::Duration::from_secs(5))
            .unwrap_or(true);
        if stale {
            self.mention_files = find_project_files(4000);
            self.mention_files_at = Some(std::time::Instant::now());
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

pub fn default_models() -> Vec<String> {
    vec![
        "gemini-2.5-flash".into(),
        "gemini-2.5-pro".into(),
        "claude-sonnet-4-6".into(),
        "claude-opus-4-8".into(),
        "gpt-4o".into(),
        "gpt-4o-mini".into(),
    ]
}

/// Expand `@path` mentions in `text` into inline file-context blocks.
pub fn expand_mentions(text: &str) -> String {
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
            let token = token.trim_end_matches(|c| matches!(c, '.' | ',' | ')' | ':' | ';'));
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
        let path = std::path::Path::new(&p);
        if path.is_file() {
            if let Ok(content) = crate::files::read_text(path) {
                let capped: String = content.chars().take(100_000).collect();
                blocks.push(format!("File: {}\n```\n{}\n```", p, capped));
            }
        }
    }
    blocks.join("\n\n")
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

/// Recursively list project files (relative paths) for `@`-mention completion.
pub fn find_project_files(max: usize) -> Vec<String> {
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut out: Vec<String> = Vec::new();
    let mut stack = vec![root.clone()];
    let skip = [".git", "target", "node_modules", ".cache", "dist", "build", ".next", ".venv", "venv", "__pycache__"];
    while let Some(dir) = stack.pop() {
        if out.len() >= max {
            break;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if name.starts_with('.') || skip.contains(&name.as_str()) {
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
    fn expand_mentions_ignores_missing_files() {
        assert_eq!(expand_mentions("see @does_not_exist_xyz.txt here"), "");
    }
}
