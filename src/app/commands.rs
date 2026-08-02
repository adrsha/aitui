use crate::app::action::Action;

#[derive(Debug, Clone, Copy)]
pub struct SlashCommand {
    pub name: &'static str,
    pub icon: &'static str,
    pub desc: &'static str,
    pub run: &'static str,
}

pub struct CommandSpec {
    pub palette: SlashCommand,
    pub aliases: &'static [&'static str],
    pub action: Option<fn() -> Action>,
}

fn quit() -> Action {
    Action::Quit
}
fn submit() -> Action {
    Action::Submit
}
fn new_session() -> Action {
    Action::NewSession
}
fn fork_session() -> Action {
    Action::ForkSession
}
fn delete_session() -> Action {
    Action::DeleteSession
}
fn open_models() -> Action {
    Action::OpenModelPicker
}
fn reload_models() -> Action {
    Action::ReloadModels
}
fn open_files() -> Action {
    Action::OpenFilePicker
}
fn clear_attachment() -> Action {
    Action::ClearAttachment
}
fn open_setup() -> Action {
    Action::OpenApiSetup
}
fn open_settings() -> Action {
    Action::OpenSettings
}
fn open_sessions() -> Action {
    Action::OpenSessionPicker
}
fn open_skills() -> Action {
    Action::OpenSkillPicker
}
fn retry_last() -> Action {
    Action::RetryLast
}
fn edit_last() -> Action {
    Action::EditLast
}
fn copy_last_reply() -> Action {
    Action::CopyLastReply
}
fn copy_last_code() -> Action {
    Action::CopyLastCode
}
fn open_editor() -> Action {
    Action::OpenEditor
}
fn open_edit_picker() -> Action {
    Action::OpenEditPicker
}
fn open_shell() -> Action {
    Action::OpenShell
}
fn toggle_help() -> Action {
    Action::ToggleHelp
}
fn clear_system_prompt() -> Action {
    Action::SetSystemPrompt(None)
}

pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        palette: SlashCommand {
            name: "send",
            icon: "▸",
            desc: "Send the message",
            run: "w",
        },
        aliases: &["w", "write", "send"],
        action: Some(submit),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "review",
            icon: "=",
            desc: "Set permission review (strict/lenient/off)",
            run: "review",
        },
        aliases: &["review", "permission-review", "access-review"],
        action: None,
    },
    CommandSpec {
        palette: SlashCommand {
            name: "mock",
            icon: "~",
            desc: "Toggle offline mock/test mode",
            run: "mock",
        },
        aliases: &["mock", "test", "offline"],
        action: None,
    },
    CommandSpec {
        palette: SlashCommand {
            name: "model",
            icon: "◆",
            desc: "Pick the model",
            run: "models",
        },
        aliases: &["models", "model"],
        action: Some(open_models),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "reload-models",
            icon: "↺",
            desc: "Retry loading models from the API",
            run: "reload-models",
        },
        aliases: &[
            "reload-models",
            "models-reload",
            "refresh-models",
            "model-reload",
        ],
        action: Some(reload_models),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "attach",
            icon: "▤",
            desc: "Attach a file",
            run: "files",
        },
        aliases: &["files", "attach"],
        action: Some(open_files),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "new",
            icon: "+",
            desc: "Start a new session",
            run: "new",
        },
        aliases: &["new", "n"],
        action: Some(new_session),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "fork",
            icon: "⑂",
            desc: "Fork this session into a parallel branch",
            run: "fork",
        },
        aliases: &["fork", "branch"],
        action: Some(fork_session),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "retry",
            icon: "↻",
            desc: "Regenerate the last reply",
            run: "retry",
        },
        aliases: &["retry", "r", "regen", "regenerate"],
        action: Some(retry_last),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "edit-last",
            icon: "✎",
            desc: "Edit your last message and resend",
            run: "edit-last",
        },
        aliases: &["edit-last", "el", "redo"],
        action: Some(edit_last),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "copy",
            icon: "⧉",
            desc: "Copy the last reply to the clipboard",
            run: "copy",
        },
        aliases: &["copy", "y", "yank"],
        action: Some(copy_last_reply),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "copy-code",
            icon: "⧉",
            desc: "Copy the last code block to the clipboard",
            run: "copy-code",
        },
        aliases: &["copy-code", "yc", "code"],
        action: Some(copy_last_code),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "suggestions",
            icon: "›",
            desc: "Toggle generated follow-up replies",
            run: "suggestions",
        },
        aliases: &["suggestions", "response-suggestions", "followups"],
        action: None,
    },
    CommandSpec {
        palette: SlashCommand {
            name: "reasoning-effort",
            icon: "r",
            desc: "Set any reasoning effort value",
            run: "reasoning-effort ",
        },
        aliases: &["reasoning-effort", "effort"],
        action: None,
    },
    CommandSpec {
        palette: SlashCommand {
            name: "reasoning-mode",
            icon: "m",
            desc: "Set any reasoning mode value",
            run: "reasoning-mode ",
        },
        aliases: &["reasoning-mode", "mode"],
        action: None,
    },
    CommandSpec {
        palette: SlashCommand {
            name: "sessions",
            icon: "≡",
            desc: "Switch session",
            run: "sessions",
        },
        aliases: &["sessions", "ls"],
        action: Some(open_sessions),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "skills",
            icon: "✦",
            desc: "Toggle skills (personas / instructions)",
            run: "skills",
        },
        aliases: &["skill", "skills"],
        action: Some(open_skills),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "editor",
            icon: "⌨",
            desc: "Open conversation in $EDITOR",
            run: "editor",
        },
        aliases: &["editor", "history"],
        action: Some(open_editor),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "edit",
            icon: "✎",
            desc: "Open a file in $EDITOR (edited files first)",
            run: "edit",
        },
        aliases: &["edit", "e", "edited"],
        action: Some(open_edit_picker),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "shell",
            icon: "▮",
            desc: "Drop into a shell, then return",
            run: "shell",
        },
        aliases: &["shell", "term", "terminal", "sh"],
        action: Some(open_shell),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "rename",
            icon: "✎",
            desc: "Rename the current session",
            run: "rename ",
        },
        aliases: &["rename"],
        action: None,
    },
    CommandSpec {
        palette: SlashCommand {
            name: "clear",
            icon: "⌫",
            desc: "Clear the conversation",
            run: "clear",
        },
        aliases: &["clear"],
        action: None,
    },
    CommandSpec {
        palette: SlashCommand {
            name: "setup",
            icon: "key",
            desc: "Set API endpoint URL + key",
            run: "setup",
        },
        aliases: &["setup", "apikey", "endpoint"],
        action: Some(open_setup),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "settings",
            icon: "⚙",
            desc: "Open settings",
            run: "settings",
        },
        aliases: &["settings", "config", "set"],
        action: Some(open_settings),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "system",
            icon: "✦",
            desc: "Edit the system prompt",
            run: "settings",
        },
        aliases: &["nosystem", "system"],
        action: Some(clear_system_prompt),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "help",
            icon: "?",
            desc: "Keybinding help",
            run: "help",
        },
        aliases: &["?", "help"],
        action: Some(toggle_help),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "quit",
            icon: "⏻",
            desc: "Quit",
            run: "quit",
        },
        aliases: &["q", "quit"],
        action: Some(quit),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "delete-session",
            icon: "⌫",
            desc: "Delete the current session",
            run: "delete",
        },
        aliases: &["delete", "rm", "ds"],
        action: Some(delete_session),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "detach",
            icon: "▤",
            desc: "Clear the attached file",
            run: "detach",
        },
        aliases: &["detach", "noattach"],
        action: Some(clear_attachment),
    },
];

/// A unified help entry for any keybind, mode, or command.
#[derive(Debug, Clone, Copy)]
pub struct HelpEntry {
    pub section: &'static str,
    pub icon: &'static str,
    pub key: &'static str,
    pub summary: &'static str,
    pub details: &'static [&'static str],
}

/// All help entries, grouped by section. The index in this array is
/// `help_selected` in the UI.
pub const HELP_ENTRIES: &[HelpEntry] = &[
    // ── Input (vim) ──
    HelpEntry {
        section: "Input (vim)",
        icon: "⌨",
        key: "h j k l",
        summary: "Move cursor left / down / up / right",
        details: &[
            "Vim-style cursor movement in the message composer.",
            "",
            "  h / ←     Left one character",
            "  j / ↓     Down one row (or next logical line)",
            "  k / ↑     Up one row (or previous logical line)",
            "  l / →     Right one character",
        ],
    },
    HelpEntry {
        section: "Input (vim)",
        icon: "⌨",
        key: "w / b",
        summary: "Word forward / backward",
        details: &[
            "Jump the cursor by whole words.",
            "",
            "  w     Jump to the start of the next word",
            "  b     Jump to the start of the current / previous word",
        ],
    },
    HelpEntry {
        section: "Input (vim)",
        icon: "⌨",
        key: "0 / $",
        summary: "Line start / end",
        details: &[
            "Jump the cursor to the beginning or end of the line.",
            "",
            "  0     Jump to the first column of the line",
            "  $     Jump after the last character of the line",
            "  ^     Jump to the first non-whitespace character",
        ],
    },
    HelpEntry {
        section: "Input (vim)",
        icon: "⌨",
        key: "a A · o O · I",
        summary: "Append / open line / insert at start",
        details: &[
            "Enter insert mode at a specific position.",
            "",
            "  a     Append after the cursor",
            "  A     Append at the end of the line",
            "  o     Open a new line below and insert",
            "  O     Open a new line above and insert",
            "  I     Insert at the first non-whitespace character",
        ],
    },
    HelpEntry {
        section: "Input (vim)",
        icon: "⌨",
        key: "x · dd · yy · p",
        summary: "Delete char / line · yank · paste",
        details: &[
            "Delete, yank (copy), and paste operations.",
            "",
            "  x         Delete the character under the cursor",
            "  dd        Delete the entire current line",
            "  D         Delete from cursor to end of line",
            "  yy / Y    Yank (copy) the current line",
            "  p         Paste after the cursor",
            "  P         Paste before the cursor",
        ],
    },
    // ── Mode (configurable) ──
    HelpEntry {
        section: "Mode",
        icon: "⚙",
        key: "",
        summary: "Insert / Normal / Visual / Command mode — details",
        details: &[
            "The composer has four vim-style modes.",
            "",
            "  Insert   Type and edit text freely",
            "  Normal   Navigate with motion keys (h/j/k/l etc.)",
            "  Visual   Select text for yank / delete / change",
            "  Command  Enter :command syntax",
            "",
            "Modes shown below are bound to configurable keys.",
        ],
    },
    HelpEntry {
        section: "Mode",
        icon: "✏",
        key: "",
        summary: "Enter keybindings — send, newline, palette, help",
        details: &[
            "Special actions triggered by key chords.",
            "",
            "  Enter               Send message (plain Enter)",
            "  Shift+Enter         Insert newline (terminal-dependent)",
            "  Ctrl+Enter          Insert newline (fallback)",
            "  / (Command palette)  Open the /-command palette",
            "  ? (Help)             Toggle this help screen",
            "",
            "Some keys are configurable in config.toml.",
        ],
    },
    // ── Global (configurable) ──
    HelpEntry {
        section: "Global",
        icon: "🌐",
        key: "",
        summary: "Editor, sessions, navigation, pickers — details",
        details: &[
            "Global keybindings accessible from any mode.",
            "These are all configurable in config.toml.",
            "",
            "Editor:        Open conversation in $EDITOR",
            "File browser:  Toggle file tree, open in $EDITOR",
            "Shell:         Drop into a subshell",
            "Session pick:  Switch between sessions",
            "Fork session:  Create a parallel branch",
            "Next/prev ses: Cycle through recent sessions",
            "Child agents:  Switch between parallel agents",
        ],
    },
    // ── File browser ──
    HelpEntry {
        section: "File browser",
        icon: "📁",
        key: "h j k l",
        summary: "Navigate file tree",
        details: &[
            "File browser navigation (Ctrl-E / Ctrl-F).",
            "",
            "  h         Go to parent directory",
            "  j / ↓     Move down the file list",
            "  k / ↑     Move up the file list",
            "  l / →     Enter selected directory",
            "  Space     Select / deselect a file",
            "  Enter     Open all selected files",
        ],
    },
    // ── Scroll / navigation ──
    HelpEntry {
        section: "Scroll & navigation",
        icon: "↕",
        key: "",
        summary: "Transcript scrolling — page, half-page, top, bottom",
        details: &[
            "Scroll the conversation transcript.",
            "",
            "  Page up / down     Scroll by one full page",
            "  Half up / down     Scroll by half a page",
            "  Top / bottom       Jump to start or end",
            "  Toggle output      Show/hide tool call output",
            "",
            "All scroll keys are configurable in config.toml.",
        ],
    },
    HelpEntry {
        section: "Scroll & navigation",
        icon: "≡",
        key: "",
        summary: "Pickers: model, file, session, skill",
        details: &[
            "Quick-access pickers for common actions.",
            "",
            "  Model picker     Switch between models",
            "  File picker      Attach a file",
            "  Session picker   Switch sessions",
            "  Skill picker     Enable/disable skills",
        ],
    },
    HelpEntry {
        section: "Scroll & navigation",
        icon: "⏻",
        key: "",
        summary: "Quit — cancel & exit",
        details: &[
            "Quit the application.",
            "",
            "  Press once to cancel an in-progress response.",
            "  Press again to exit.",
        ],
    },
];

pub fn slash_commands() -> impl ExactSizeIterator<Item = &'static SlashCommand> {
    COMMANDS.iter().map(|spec| &spec.palette)
}

pub fn exact_command_action(cmd: &str) -> Option<Action> {
    COMMANDS.iter().find_map(|spec| {
        if spec.aliases.contains(&cmd) {
            spec.action.map(|action| action())
        } else {
            None
        }
    })
}

/// Detailed help page for a slash command.
/// Only `name`/`icon` are rendered today; the remaining fields feed the planned
/// discoverable help overlay (ROADMAP Phase 5), so keep them populated.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct CommandDoc {
    pub name: &'static str,
    pub icon: &'static str,
    pub summary: &'static str,
    pub aliases: &'static str,
    pub usage: &'static str,
    pub details: &'static [&'static str],
}

pub const COMMAND_DOCS: &[CommandDoc] = &[
    CommandDoc {
        name: "send",
        icon: "▸",
        summary: "Send the current message to the model",
        aliases: ":w, :write, :send",
        usage: ":w           Send the message buffer",
        details: &[
            "Sends the message you've typed in the input area to the",
            "active model. Works like Enter (but can be bound to a",
            "different key combination).",
            "",
            ":w is the vim-style alias.",
        ],
    },
    CommandDoc {
        name: "review",
        icon: "=",
        summary: "Set permission review mode (strict/lenient/off)",
        aliases: ":review, :permission-review, :access-review",
        usage: ":review                 Show current mode",
        details: &[
            ":review strict   Always prompt before every tool call",
            ":review lenient  Auto-allow reads, prompt for writes",
            ":review off      Skip prompts (manual review only)",
            "",
            "Controls how the agent handles tool-call permissions.",
            "Strict is safest for untrusted environments.",
        ],
    },
    CommandDoc {
        name: "mock",
        icon: "~",
        summary: "Switch to offline mock/test mode",
        aliases: ":mock, :test, :offline",
        usage: ":mock        Toggle mock mode",
        details: &[
            "Switches to a mock model that simulates responses",
            "without calling any API. Useful for testing UI",
            "interactions or developing offline.",
            "",
            "Type :mock again or select a real model to exit",
            "mock mode.",
        ],
    },
    CommandDoc {
        name: "model",
        icon: "◆",
        summary: "Pick a model from the available list",
        aliases: ":models, :model",
        usage: ":model       Open the model picker",
        details: &[
            "Opens the interactive model picker overlay.",
            "Available models are listed from the API endpoint.",
            "Use ↑↓ to navigate and Enter to select.",
        ],
    },
    CommandDoc {
        name: "reload-models",
        icon: "↺",
        summary: "Retry loading models from the API",
        aliases: ":reload-models, :refresh-models, :model-reload",
        usage: ":reload-models",
        details: &[
            "Forces a fresh fetch of the model list from the API.",
            "Useful after changing the API endpoint or key.",
        ],
    },
    CommandDoc {
        name: "attach",
        icon: "▤",
        summary: "Attach a file to the message context",
        aliases: ":files, :attach",
        usage: ":attach <path>   Attach a specific file",
        details: &[
            ":files              Open the file browser to pick",
            ":attach /path/to/file",
            "",
            "Attached files are included in the context sent to",
            "the model. Use @file in the message to mention",
            "a specific file.",
        ],
    },
    CommandDoc {
        name: "new",
        icon: "+",
        summary: "Start a new chat session",
        aliases: ":new, :n",
        usage: ":new         Start a fresh session",
        details: &[
            "Creates a new empty session and switches to it.",
            "The previous session is preserved in the session",
            "list and can be revisited with :sessions.",
        ],
    },
    CommandDoc {
        name: "fork",
        icon: "⑂",
        summary: "Fork this session into a parallel branch",
        aliases: ":fork, :branch",
        usage: ":fork        Fork the current session",
        details: &[
            "Creates a copy of the current session history",
            "as a new session. Useful for exploring different",
            "approaches without losing the original path.",
        ],
    },
    CommandDoc {
        name: "retry",
        icon: "↻",
        summary: "Regenerate the last model reply",
        aliases: ":retry, :r, :regen, :regenerate",
        usage: ":retry       Retry the last turn",
        details: &[
            "Deletes the last assistant reply and re-sends",
            "the last user message to the model. Useful when",
            "the response was cut off or unsatisfactory.",
        ],
    },
    CommandDoc {
        name: "edit-last",
        icon: "✎",
        summary: "Edit your last message and resend",
        aliases: ":edit-last, :el, :redo",
        usage: ":edit-last   Open last message for editing",
        details: &[
            "Opens your most recent user message in $EDITOR.",
            "After saving and closing the editor, the edited",
            "message replaces the original and is re-sent to",
            "the model along with the conversation history.",
        ],
    },
    CommandDoc {
        name: "copy",
        icon: "⧉",
        summary: "Copy the last reply to clipboard",
        aliases: ":copy, :y, :yank",
        usage: ":copy        Copy last reply to clipboard",
        details: &[
            "Copies the full text of the most recent assistant",
            "reply to the system clipboard (via OSC 52 escape",
            "sequence, or an external clipboard tool).",
        ],
    },
    CommandDoc {
        name: "copy-code",
        icon: "⧉",
        summary: "Copy the last code block to clipboard",
        aliases: ":copy-code, :yc, :code",
        usage: ":copy-code   Copy code block to clipboard",
        details: &[
            "Copies only the last code block (fenced with ```)",
            "from the most recent assistant reply to the",
            "system clipboard.",
        ],
    },
    CommandDoc {
        name: "suggestions",
        icon: "›",
        summary: "Toggle generated follow-up suggestions",
        aliases: ":suggestions, :followups",
        usage: ":suggestions on|off   Toggle follow-ups",
        details: &[
            ":suggestions         Show current state",
            ":suggestions on      Enable follow-ups",
            ":suggestions off     Disable follow-ups",
            "",
            "When enabled, the model suggests follow-up",
            "questions or actions after each response.",
        ],
    },
    CommandDoc {
        name: "reasoning-effort",
        icon: "r",
        summary: "Set the reasoning effort value",
        aliases: ":reasoning-effort, :effort",
        usage: ":reasoning-effort <value>",
        details: &[
            ":reasoning-effort        Prompt for a value",
            ":reasoning-effort high   Set effort to 'high'",
            "",
            "Sets the reasoning effort parameter for supported",
            "models. Any string value is accepted — common",
            "values are 'low', 'medium', or 'high'.",
        ],
    },
    CommandDoc {
        name: "reasoning-mode",
        icon: "m",
        summary: "Set the reasoning mode value",
        aliases: ":reasoning-mode, :mode",
        usage: ":reasoning-mode <value>",
        details: &[
            ":reasoning-mode         Prompt for a value",
            ":reasoning-mode high    Set mode to 'high'",
            "",
            "Sets the reasoning mode parameter. Any string",
            "value is accepted.",
        ],
    },
    CommandDoc {
        name: "sessions",
        icon: "≡",
        summary: "Switch to a different session",
        aliases: ":sessions, :ls",
        usage: ":sessions    Open session picker",
        details: &[
            "Opens the session picker overlay showing all",
            "saved sessions. Use ↑↓ to navigate and Enter",
            "to switch to a session.",
        ],
    },
    CommandDoc {
        name: "skills",
        icon: "✦",
        summary: "Toggle skills (personas / instructions)",
        aliases: ":skill, :skills",
        usage: ":skills      Open skill picker",
        details: &[
            "Skills are modular instruction sets that modify",
            "the model's behavior. Place .md files in",
            "~/.config/aitui/skills/ to add custom skills.",
        ],
    },
    CommandDoc {
        name: "editor",
        icon: "⌨",
        summary: "Open conversation in $EDITOR",
        aliases: ":editor, :history",
        usage: ":editor      Open in $EDITOR",
        details: &[
            "Exports the current conversation to a temp file",
            "and opens it in $EDITOR. Saving and closing the",
            "editor imports the content back.",
        ],
    },
    CommandDoc {
        name: "edit",
        icon: "✎",
        summary: "Open recently edited files in $EDITOR",
        aliases: ":edit, :e, :edited",
        usage: ":edit        Open file picker for edited files",
        details: &[
            "Shows a picker of files that have been recently",
            "edited by the agent. Select one to open it in",
            "$EDITOR.",
        ],
    },
    CommandDoc {
        name: "shell",
        icon: "▮",
        summary: "Drop into an interactive shell",
        aliases: ":shell, :term, :sh",
        usage: ":shell       Drop to shell",
        details: &[
            "Suspends the TUI and drops you into a shell.",
            "Exit the shell (usually Ctrl-D or 'exit') to",
            "return to the application.",
        ],
    },
    CommandDoc {
        name: "rename",
        icon: "✎",
        summary: "Rename the current session",
        aliases: ":rename",
        usage: ":rename <name>",
        details: &[
            ":rename my-project   Rename to 'my-project'",
            "",
            "Sets a human-readable name for the current",
            "session, making it easier to find later in",
            "the session picker (:sessions).",
        ],
    },
    CommandDoc {
        name: "clear",
        icon: "⌫",
        summary: "Clear the current conversation",
        aliases: ":clear",
        usage: ":clear       Clear all messages",
        details: &[
            "Removes all messages from the current session",
            "while preserving the session metadata (name,",
            "model, settings). Useful for a fresh start",
            "without losing context.",
        ],
    },
    CommandDoc {
        name: "setup",
        icon: "key",
        summary: "Configure API endpoint and key",
        aliases: ":setup, :apikey, :endpoint",
        usage: ":setup       Open API config",
        details: &[
            "Opens the API configuration overlay where you",
            "can set the endpoint URL and API key.",
            "After saving, models are reloaded from the",
            "new endpoint.",
        ],
    },
    CommandDoc {
        name: "settings",
        icon: "⚙",
        summary: "Open the settings panel",
        aliases: ":settings, :config, :set",
        usage: ":settings    Open settings",
        details: &[
            "Opens the settings overlay where you can",
            "adjust various application settings like",
            "auto-approve, input height, reasoning",
            "parameters, and the system prompt.",
        ],
    },
    CommandDoc {
        name: "system",
        icon: "✦",
        summary: "Clear the system prompt",
        aliases: ":nosystem, :system",
        usage: ":system      Clear system prompt",
        details: &[
            "Removes the custom system prompt, reverting",
            "to the default system instructions sent to",
            "the model.",
        ],
    },
    CommandDoc {
        name: "help",
        icon: "?",
        summary: "Show this keybinding help",
        aliases: ":?, :help",
        usage: ":help        Toggle help",
        details: &[
            "Toggles the keybinding reference overlay.",
            "Press ? again, q, or Escape to close.",
        ],
    },
    CommandDoc {
        name: "quit",
        icon: "⏻",
        summary: "Quit the application",
        aliases: ":q, :quit",
        usage: ":q           Quit",
        details: &[
            "Exits the application. If a response is in",
            "progress, it is cancelled first.",
        ],
    },
    CommandDoc {
        name: "delete-session",
        icon: "⌫",
        summary: "Delete the current session",
        aliases: ":delete, :rm, :ds",
        usage: ":delete      Delete current session",
        details: &[
            "Permanently removes the current session.",
            "This action cannot be undone — consider",
            "using :new to start fresh instead.",
        ],
    },
    CommandDoc {
        name: "detach",
        icon: "▤",
        summary: "Clear the attached file",
        aliases: ":detach, :noattach",
        usage: ":detach      Remove attached file",
        details: &[
            "Removes any file that was attached with",
            ":attach or the file picker.",
        ],
    },
];
