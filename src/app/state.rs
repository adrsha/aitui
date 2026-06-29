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
use crate::input::vim::VimMode;
use crate::render::chat::ChatState;
use crate::render::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Chat,
    Input,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PanelLayout {
    pub sidebar: ratatui::layout::Rect,
    pub chat: ratatui::layout::Rect,
    pub input: ratatui::layout::Rect,
    pub statusbar: ratatui::layout::Rect,
    pub toggle: ratatui::layout::Rect,
}

pub struct App {
    pub config: Config,
    pub sessions: SessionManager,
    pub chat: ChatState,
    pub focus: Focus,
    pub vim: VimMode,

    pub input: InputBuffer,
    pub command: String,
    pub command_history: Vec<String>,
    pub command_history_idx: Option<usize>,
    pub pending_escape: Option<char>,

    pub overlay: Overlay,
    pub mention: Mention,

    pub models: Vec<String>,
    pub model_idx: usize,

    pub attachment: Option<PathBuf>,
    pub status: Option<String>,
    pub show_help: bool,
    pub sidebar_collapsed: bool,
    pub should_quit: bool,
    pub yank: Option<String>,

    /// Bumped whenever chat content/collapse changes, to invalidate the doc cache.
    pub content_rev: u64,

    pub permissions: PermissionMemory,
    pub pending_tools: VecDeque<ToolCall>,
    pub agent_iterations: usize,

    pub stream_rx: Option<mpsc::Receiver<StreamEvent>>,
    pub agent_tool_rx: Option<mpsc::Receiver<ToolResult>>,
    pub models_rx: Option<oneshot::Receiver<anyhow::Result<Vec<String>>>>,

    pub layout: PanelLayout,
    pub(crate) api: Option<ApiClient>,
}

pub const MAX_AGENT_ITERATIONS: usize = 25;

impl App {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let api = ApiClient::new(&config.api.endpoint, &config.api.api_key)?;

        let (models_tx, models_rx) = oneshot::channel();
        {
            let fetch = ApiClient::new(&config.api.endpoint, &config.api.api_key)?;
            tokio::spawn(async move {
                let _ = models_tx.send(fetch.fetch_models().await);
            });
        }

        let models = default_models();
        let model_idx = models.iter().position(|m| m == &config.api.default_model).unwrap_or(0);

        let mut app = Self {
            config,
            sessions: SessionManager::load(),
            chat: ChatState::new(),
            focus: Focus::Input,
            vim: VimMode::Normal,
            input: InputBuffer::default(),
            command: String::new(),
            command_history: Vec::new(),
            command_history_idx: None,
            pending_escape: None,
            overlay: Overlay::None,
            mention: Mention::default(),
            models,
            model_idx,
            attachment: None,
            status: Some("i = insert  ·  @ = file  ·  / = commands  ·  :w = send  ·  ? = help".into()),
            show_help: false,
            sidebar_collapsed: false,
            should_quit: false,
            yank: None,
            content_rev: 0,
            permissions: PermissionMemory::default(),
            pending_tools: VecDeque::new(),
            agent_iterations: 0,
            stream_rx: None,
            agent_tool_rx: None,
            models_rx: Some(models_rx),
            layout: PanelLayout::default(),
            api: Some(api),
        };

        if app.config.ui.agent_default {
            app.sessions.active_mut().agent_mode = true;
        }
        sync_auto_approvals(&mut app.permissions, app.config.ui.auto_approve_reads);
        Ok(app)
    }

    pub fn theme(&self) -> Theme {
        Theme::default()
    }

    pub fn current_model(&self) -> &str {
        self.models.get(self.model_idx).map(|s| s.as_str()).unwrap_or("unknown")
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
        if self.focus != Focus::Input || self.vim != VimMode::Insert {
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
        let files = find_project_files(4000);
        let q = self.mention.query.to_lowercase();
        let mut scored: Vec<(usize, &String)> = files
            .iter()
            .filter_map(|f| fuzzy_score(&f.to_lowercase(), &q).map(|s| (s, f)))
            .collect();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.len().cmp(&b.1.len())).then(a.1.cmp(b.1)));
        self.mention.matches = scored.into_iter().take(50).map(|(_, f)| f.clone()).collect();
        if self.mention.selected >= self.mention.matches.len() {
            self.mention.selected = 0;
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

/// List entries of a directory for the file picker (dirs first, with `../`).
pub fn list_dir_entries(dir: &PathBuf) -> Vec<String> {
    let mut entries: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if e.path().is_dir() {
                format!("{}/", name)
            } else {
                name
            }
        })
        .collect();
    entries.sort_by(|a, b| b.ends_with('/').cmp(&a.ends_with('/')).then(a.cmp(b)));
    if dir.parent().is_some() {
        entries.insert(0, "../".into());
    }
    entries
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
