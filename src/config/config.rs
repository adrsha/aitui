use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Top-level application configuration, loaded from ~/.config/aichat-tui/config.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub api: ApiConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub keybinds: KeybindConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    pub endpoint: String,
    pub api_key: String,
    pub default_model: String,
    /// Offline test mode: interpret messages locally and drive the agent tools
    /// without any network. Also auto-enabled when `endpoint` is empty, or via
    /// the `AITUI_MOCK` environment variable. Toggle at runtime with `:mock`.
    #[serde(default)]
    pub mock: bool,
    /// A global system prompt prepended to every request (house style / persona).
    /// Edit it directly in `config.toml`. Per-session prompts (Settings overlay /
    /// `:system`) are added on top of this. Empty = none.
    #[serde(default)]
    pub system_prompt: String,
    /// Reasoning effort for reasoning-capable models: "low" | "medium" | "high"
    /// (empty = don't send one). Cycle at runtime with `:effort`. Sent as the
    /// OpenAI `reasoning_effort` request field.
    #[serde(default)]
    pub reasoning_effort: String,
    /// Use native OpenAI function-calling (`tools`/`tool_calls`) instead of parsing
    /// ```` ```tool ```` fences from the reply. Toggle at runtime with `:native`;
    /// auto-disabled if the endpoint rejects the `tools` field. Default on.
    #[serde(default = "default_true")]
    pub native_tools: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    /// Default web-search backend. `searxng` is preferred; `duckduckgo` and `bing`
    /// are kept as fallbacks when SearxNG instances reject automated requests.
    #[serde(default = "default_search_provider")]
    pub provider: String,
    /// Optional SearxNG base URL, e.g. `https://searx.example.com/`. Empty means
    /// try a small built-in list of public SearxNG instances plus AITUI_SEARXNG_URL.
    #[serde(default)]
    pub searxng_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    pub input_height: u16,
    /// Name of the active colour theme (see `ui::theme::Theme::all()`).
    #[serde(default = "default_theme")]
    pub theme: String,
    /// New sessions start with agent (tool-using) mode enabled.
    #[serde(default = "default_true")]
    pub agent_default: bool,
    /// Auto-approve low-risk read-only tools (read_file/list_dir/search_files)
    /// so the coding agent flows without a permission prompt for safe reads.
    #[serde(default = "default_true")]
    pub auto_approve_reads: bool,
    /// Sticky skills: remember which skills are active across restarts (persisted
    /// to `~/.config/aitui/active_skills.json`). Toggle with `:sticky`.
    #[serde(default = "default_true")]
    pub sticky_skills: bool,
}

fn default_search_provider() -> String {
    "searxng".to_string()
}

fn default_theme() -> String {
    "midnight".to_string()
}

fn default_true() -> bool {
    true
}

/// Customizable keybindings. Every field is a key spec string, e.g. `"ctrl-n"`,
/// `"i"`, `"?"`, `"esc"`, `"pageup"`, `"ctrl-home"`. Modifiers `ctrl-`, `alt-`,
/// and `shift-` may be combined; named keys: esc, enter, tab, space, backspace,
/// delete, pageup, pagedown, home, end, up, down, left, right.
///
/// These cover the application *actions* and mode switches. The vim editing
/// motions inside the input box (h/j/k/l, w, b, 0, $, x, dd, yy, p, …) follow
/// standard vim and are not remapped here.
///
/// Note: global bindings fire in every mode, so prefer modifier combos for them
/// — binding a global action to a bare letter would shadow typing it in insert
/// mode.
///
/// Each field carries a `#[serde(default)]` so a hand-edited config may omit any
/// binding and still load.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeybindConfig {
    // ── Global (fire in any mode) ──────────────────────────────────────────
    /// Cancel an in-flight response, or quit when idle.
    #[serde(default = "kb_quit")]
    pub quit: String,
    #[serde(default = "kb_next_session", alias = "session_next")]
    pub next_session: String,
    #[serde(default = "kb_prev_session", alias = "session_prev")]
    pub prev_session: String,
    #[serde(default = "kb_session_picker", alias = "open_session_picker")]
    pub session_picker: String,
    /// Fork the current session into a parallel branch.
    #[serde(default = "kb_fork_session", alias = "branch_session")]
    pub fork_session: String,
    /// Open the conversation in $EDITOR.
    #[serde(default = "kb_open_editor", alias = "open_transcript")]
    pub open_editor: String,
    /// Open a file in $EDITOR (edited files first, then browse).
    #[serde(default = "kb_open_file", alias = "open_edit_picker")]
    pub open_file: String,
    /// Drop into an interactive shell.
    #[serde(default = "kb_open_shell", alias = "open_terminal")]
    pub open_shell: String,
    #[serde(default = "kb_file_picker", alias = "open_file_picker")]
    pub file_picker: String,
    #[serde(default = "kb_model_picker", alias = "open_model_picker")]
    pub model_picker: String,
    #[serde(default = "kb_next_model", alias = "model_next")]
    pub next_model: String,
    #[serde(default = "kb_prev_model", alias = "model_prev")]
    pub prev_model: String,
    #[serde(default = "kb_toggle_agent", alias = "toggle_agent_mode")]
    pub toggle_agent: String,
    #[serde(default = "kb_redraw")]
    pub redraw: String,
    #[serde(default = "kb_scroll_up")]
    pub scroll_up: String,
    #[serde(default = "kb_scroll_down")]
    pub scroll_down: String,
    #[serde(default = "kb_scroll_top")]
    pub scroll_top: String,
    #[serde(default = "kb_scroll_bottom")]
    pub scroll_bottom: String,
    /// Half-page scroll down / up the transcript (vim Ctrl-D / Ctrl-U).
    #[serde(default = "kb_scroll_half_down")]
    pub scroll_half_down: String,
    #[serde(default = "kb_scroll_half_up")]
    pub scroll_half_up: String,
    /// Show / hide the full output of executed tools (collapsed by default).
    #[serde(default = "kb_toggle_output", alias = "toggle_tool_output")]
    pub toggle_output: String,

    // ── Normal mode (input box) ────────────────────────────────────────────
    #[serde(default = "kb_insert", alias = "normal_insert", alias = "enter_insert")]
    pub insert: String,
    #[serde(default = "kb_command", alias = "enter_command")]
    pub command: String,
    #[serde(default = "kb_palette", alias = "open_palette")]
    pub palette: String,
    #[serde(default = "kb_help", alias = "toggle_help")]
    pub help: String,
    #[serde(default = "kb_submit", alias = "send_message")]
    pub submit: String,
    #[serde(default = "kb_visual", alias = "enter_visual")]
    pub visual: String,

    // ── Insert mode ────────────────────────────────────────────────────────
    /// Leave insert mode back to normal. May be a single key (`esc`, `ctrl-[`)
    /// or a two-character chord like `jk` — with a chord, Esc still works too.
    #[serde(
        default = "kb_normal",
        alias = "insert_to_normal",
        alias = "escape_insert"
    )]
    pub normal: String,
}

fn kb_quit() -> String {
    "ctrl-c".into()
}
fn kb_next_session() -> String {
    "ctrl-n".into()
}
fn kb_prev_session() -> String {
    "ctrl-p".into()
}
fn kb_session_picker() -> String {
    "ctrl-s".into()
}
fn kb_fork_session() -> String {
    "ctrl-y".into()
}
fn kb_open_editor() -> String {
    "ctrl-o".into()
}
fn kb_open_file() -> String {
    "ctrl-e".into()
}
fn kb_open_shell() -> String {
    "ctrl-g".into()
}
fn kb_file_picker() -> String {
    "ctrl-f".into()
}
fn kb_model_picker() -> String {
    "ctrl-m".into()
}
fn kb_next_model() -> String {
    "ctrl-]".into()
}
fn kb_prev_model() -> String {
    "ctrl-[".into()
}
fn kb_toggle_agent() -> String {
    "ctrl-a".into()
}
fn kb_redraw() -> String {
    "ctrl-l".into()
}
fn kb_scroll_up() -> String {
    "pageup".into()
}
fn kb_scroll_down() -> String {
    "pagedown".into()
}
fn kb_scroll_top() -> String {
    "ctrl-home".into()
}
fn kb_scroll_bottom() -> String {
    "ctrl-end".into()
}
fn kb_scroll_half_down() -> String {
    "ctrl-d".into()
}
fn kb_scroll_half_up() -> String {
    "ctrl-u".into()
}
fn kb_toggle_output() -> String {
    "ctrl-t".into()
}
fn kb_insert() -> String {
    "i".into()
}
fn kb_command() -> String {
    ":".into()
}
fn kb_palette() -> String {
    "/".into()
}
fn kb_help() -> String {
    "?".into()
}
fn kb_submit() -> String {
    "enter".into()
}
fn kb_visual() -> String {
    "v".into()
}
fn kb_normal() -> String {
    "esc".into()
}

impl Default for KeybindConfig {
    fn default() -> Self {
        Self {
            quit: kb_quit(),
            next_session: kb_next_session(),
            prev_session: kb_prev_session(),
            session_picker: kb_session_picker(),
            fork_session: kb_fork_session(),
            open_editor: kb_open_editor(),
            open_file: kb_open_file(),
            open_shell: kb_open_shell(),
            file_picker: kb_file_picker(),
            model_picker: kb_model_picker(),
            next_model: kb_next_model(),
            prev_model: kb_prev_model(),
            toggle_agent: kb_toggle_agent(),
            redraw: kb_redraw(),
            scroll_up: kb_scroll_up(),
            scroll_down: kb_scroll_down(),
            scroll_top: kb_scroll_top(),
            scroll_bottom: kb_scroll_bottom(),
            scroll_half_down: kb_scroll_half_down(),
            scroll_half_up: kb_scroll_half_up(),
            toggle_output: kb_toggle_output(),
            insert: kb_insert(),
            command: kb_command(),
            palette: kb_palette(),
            help: kb_help(),
            submit: kb_submit(),
            visual: kb_visual(),
            normal: kb_normal(),
        }
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            provider: default_search_provider(),
            searxng_url: String::new(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            input_height: 6,
            theme: default_theme(),
            agent_default: true,
            auto_approve_reads: true,
            sticky_skills: true,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // No secrets baked into the binary. On first run this writes a
            // template to ~/.config/aitui/config.toml for the user to fill in.
            // The endpoint/key may also be supplied via the AITUI_ENDPOINT /
            // AITUI_API_KEY (or BIG_THINKER_URL / BIG_THINKER_API_KEY) environment
            // variables (see `Config::load`).
            api: ApiConfig {
                endpoint: String::new(),
                api_key: String::new(),
                default_model: "gpt-5.5".to_string(),
                mock: false,
                system_prompt: String::new(),
                reasoning_effort: String::new(),
                native_tools: true,
            },
            ui: UiConfig::default(),
            search: SearchConfig::default(),
            keybinds: KeybindConfig::default(),
        }
    }
}

impl Config {
    /// Load config from the standard path, falling back to defaults if missing.
    pub fn load() -> anyhow::Result<Self> {
        let path = config_path();

        if !path.exists() {
            let cfg = Config::default();
            cfg.save()?;
            return Ok(cfg);
        }

        let raw = fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read config at {}: {}", path.display(), e))?;

        let mut cfg: Config = toml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("Failed to parse config at {}: {}", path.display(), e))?;

        // Environment variables override the config file when present, so
        // secrets can stay out of disk entirely if the user prefers.
        if let Ok(endpoint) = std::env::var("AITUI_ENDPOINT") {
            if !endpoint.is_empty() {
                cfg.api.endpoint = endpoint;
            }
        }
        if let Ok(key) = std::env::var("AITUI_API_KEY") {
            if !key.is_empty() {
                cfg.api.api_key = key;
            }
        }
        // BIG_THINKER_* aliases take precedence over AITUI_* when set, so the
        // endpoint/key can be supplied under either name.
        if let Ok(endpoint) = std::env::var("BIG_THINKER_URL") {
            if !endpoint.is_empty() {
                cfg.api.endpoint = endpoint;
            }
        }
        if let Ok(key) = std::env::var("BIG_THINKER_API_KEY") {
            if !key.is_empty() {
                cfg.api.api_key = key;
            }
        }
        if let Ok(url) = std::env::var("AITUI_SEARXNG_URL") {
            if !url.is_empty() {
                cfg.search.searxng_url = url;
            }
        }
        if let Ok(provider) = std::env::var("AITUI_SEARCH_PROVIDER") {
            if !provider.is_empty() {
                cfg.search.provider = provider;
            }
        }

        Ok(cfg)
    }

    /// Write the current config back to disk.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path();

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                anyhow::anyhow!("Failed to create config dir {}: {}", parent.display(), e)
            })?;
        }

        let raw = toml::to_string_pretty(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize config: {}", e))?;

        fs::write(&path, raw)
            .map_err(|e| anyhow::anyhow!("Failed to write config to {}: {}", path.display(), e))?;

        Ok(())
    }
}

fn config_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    base.join("aitui").join("config.toml")
}
