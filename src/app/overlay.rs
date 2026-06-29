//! Overlay (modal) state: fuzzy pickers, the slash-command palette, the settings
//! panel, the agent permission prompt, and the inline `@file` mention popup.

use std::path::PathBuf;

use crate::agent::{Permission, PermissionMemory, ToolCall, ToolKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    File,
    Model,
}

/// A fuzzy-filtered list picker (files or models).
#[derive(Debug, Clone)]
pub struct Picker {
    pub kind: PickerKind,
    pub query: String,
    pub items: Vec<String>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub dir: PathBuf,
}

impl Picker {
    pub fn models(items: Vec<String>) -> Self {
        let filtered = (0..items.len()).collect();
        Self { kind: PickerKind::Model, query: String::new(), items, filtered, selected: 0, dir: PathBuf::new() }
    }

    pub fn files(dir: PathBuf, items: Vec<String>) -> Self {
        let filtered = (0..items.len()).collect();
        Self { kind: PickerKind::File, query: String::new(), items, filtered, selected: 0, dir }
    }

    pub fn refilter(&mut self) {
        let q = self.query.to_lowercase();
        self.filtered = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| q.is_empty() || it.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }

    pub fn selected_item(&self) -> Option<&str> {
        self.filtered.get(self.selected).map(|&i| self.items[i].as_str())
    }
    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
    pub fn down(&mut self) {
        if self.selected + 1 < self.filtered.len() {
            self.selected += 1;
        }
    }
}

/// A discoverable slash command.
#[derive(Debug, Clone, Copy)]
pub struct SlashCommand {
    pub name: &'static str,
    pub icon: &'static str,
    pub desc: &'static str,
    pub run: &'static str,
}

pub fn slash_commands() -> &'static [SlashCommand] {
    &[
        SlashCommand { name: "agent", icon: "◇", desc: "Toggle agent (tool-using) mode", run: "agent" },
        SlashCommand { name: "model", icon: "◆", desc: "Pick the model", run: "models" },
        SlashCommand { name: "attach", icon: "▤", desc: "Attach a file", run: "files" },
        SlashCommand { name: "new", icon: "+", desc: "Start a new session", run: "new" },
        SlashCommand { name: "clear", icon: "⌫", desc: "Clear the conversation", run: "clear" },
        SlashCommand { name: "settings", icon: "⚙", desc: "Open settings", run: "settings" },
        SlashCommand { name: "system", icon: "✦", desc: "Edit the system prompt", run: "settings" },
        SlashCommand { name: "sidebar", icon: "▦", desc: "Show / hide the sidebar", run: "sidebar" },
        SlashCommand { name: "help", icon: "?", desc: "Keybinding help", run: "help" },
        SlashCommand { name: "quit", icon: "⏻", desc: "Quit", run: "quit" },
    ]
}

#[derive(Debug, Clone)]
pub struct Palette {
    pub query: String,
    pub filtered: Vec<usize>,
    pub selected: usize,
}

impl Palette {
    pub fn new() -> Self {
        let n = slash_commands().len();
        Self { query: String::new(), filtered: (0..n).collect(), selected: 0 }
    }
    pub fn refilter(&mut self) {
        let q = self.query.to_lowercase();
        self.filtered = slash_commands()
            .iter()
            .enumerate()
            .filter(|(_, c)| q.is_empty() || c.name.contains(&q) || c.desc.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }
    pub fn selected_cmd(&self) -> Option<&'static SlashCommand> {
        self.filtered.get(self.selected).map(|&i| &slash_commands()[i])
    }
    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
    pub fn down(&mut self) {
        if self.selected + 1 < self.filtered.len() {
            self.selected += 1;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsRow {
    AgentDefault,
    AutoApprove,
    SidebarWidth,
    InputHeight,
    SystemPrompt,
}

impl SettingsRow {
    pub fn all() -> [SettingsRow; 5] {
        [
            SettingsRow::AgentDefault,
            SettingsRow::AutoApprove,
            SettingsRow::SidebarWidth,
            SettingsRow::InputHeight,
            SettingsRow::SystemPrompt,
        ]
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub selected: usize,
    pub editing_prompt: bool,
    pub prompt_buf: String,
}

/// A pending tool call awaiting the user's permission decision.
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub call: ToolCall,
    pub selected: usize,
}

impl PermissionRequest {
    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
    pub fn down(&mut self) {
        if self.selected < 3 {
            self.selected += 1;
        }
    }
    pub fn permission(&self) -> Permission {
        match self.selected {
            0 => Permission::Allow,
            1 => Permission::AllowAll,
            2 => Permission::Deny,
            _ => Permission::DenyAll,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Overlay {
    None,
    Picker(Picker),
    Palette(Palette),
    Settings(Settings),
    Permission(PermissionRequest),
}

impl Overlay {
    pub fn is_active(&self) -> bool {
        !matches!(self, Overlay::None)
    }
}

/// Inline `@file` mention completion.
#[derive(Debug, Clone, Default)]
pub struct Mention {
    pub active: bool,
    pub query: String,
    pub anchor_row: usize,
    pub anchor_col: usize,
    pub matches: Vec<String>,
    pub selected: usize,
}

impl Mention {
    pub fn reset(&mut self) {
        *self = Mention::default();
    }
    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
    pub fn down(&mut self) {
        if self.selected + 1 < self.matches.len() {
            self.selected += 1;
        }
    }
}

/// Seed/clear the read-only auto-approvals based on config.
pub fn sync_auto_approvals(mem: &mut PermissionMemory, enabled: bool) {
    let reads = [ToolKind::ReadFile, ToolKind::ListDir, ToolKind::SearchFiles];
    if enabled {
        for k in reads {
            mem.remember_allow(k);
        }
    } else {
        mem.always_allow.retain(|k| !reads.contains(k));
    }
}
