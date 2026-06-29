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
    pub keybinds: KeybindConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    pub endpoint: String,
    pub api_key: String,
    pub default_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    pub sidebar_width: u16,
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
}

fn default_theme() -> String {
    "midnight".to_string()
}

fn default_true() -> bool {
    true
}

/// Customizable keybindings. Each field is a key sequence string.
/// Supported formats: single chars like "j", ctrl combos like "ctrl-c",
/// special keys like "esc", "enter", "tab", "backspace".
/// Multi-key sequences like "jk" are supported for insert->normal escape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeybindConfig {
    /// Key(s) to exit insert mode to normal mode (default: "esc", can also set "jk")
    pub insert_to_normal: String,
    /// Key to enter insert mode (default: "i")
    pub normal_insert: String,
    /// Key to send message (default: ":w" via command, but also ctrl-enter)
    pub send_message: String,
    /// Key to toggle help (default: "?")
    pub toggle_help: String,
    /// Key to cycle focus panels (default: "tab")
    pub cycle_focus: String,
    /// Key to open file picker (default: "ctrl-f")
    pub open_file_picker: String,
    /// Key to open model picker (default: "ctrl-m")
    pub open_model_picker: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            sidebar_width: 28,
            input_height: 6,
            theme: default_theme(),
            agent_default: true,
            auto_approve_reads: true,
        }
    }
}

impl Default for KeybindConfig {
    fn default() -> Self {
        Self {
            insert_to_normal: "esc".to_string(),
            normal_insert: "i".to_string(),
            send_message: "ctrl-enter".to_string(),
            toggle_help: "?".to_string(),
            cycle_focus: "tab".to_string(),
            open_file_picker: "ctrl-f".to_string(),
            open_model_picker: "ctrl-m".to_string(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api: ApiConfig {
                endpoint: "http://192.168.1.75:8317".to_string(),
                api_key: String::from("cpa_zO3cW3GHwwAPoTf6hOuUF1G02ZvEj2Kx"),
                default_model: "gemini-2.5-flash".to_string(),
            },
            ui: UiConfig::default(),
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

        let cfg: Config = toml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("Failed to parse config at {}: {}", path.display(), e))?;

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
    base.join("aichat-tui").join("config.toml")
}
