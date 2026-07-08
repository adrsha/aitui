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
fn toggle_agent() -> Action {
    Action::ToggleAgentMode
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
            name: "agent",
            icon: "◇",
            desc: "Toggle agent (tool-using) mode",
            run: "agent",
        },
        aliases: &["agent", "agentmode"],
        action: Some(toggle_agent),
    },
    CommandSpec {
        palette: SlashCommand {
            name: "mock",
            icon: "⚗",
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
            name: "effort",
            icon: "🧠",
            desc: "Cycle reasoning effort (low/medium/high/off)",
            run: "effort",
        },
        aliases: &["effort", "reasoning"],
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
            icon: "🔑",
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
