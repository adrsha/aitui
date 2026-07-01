//! Every state change flows through this `Action` enum and the reducer in
//! `reducer.rs`. The input handler translates key/mouse events into actions; the
//! main loop translates channel events (stream tokens, tool results) into
//! actions. Side effects (spawning a request) are returned as follow-up actions.

use std::path::PathBuf;
use tokio::sync::mpsc;

use crate::api::StreamEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
    WordForward,
    WordBackward,
}

#[derive(Debug)]
pub enum Action {
    // Modes
    EnterInsert,
    EnterNormal,
    EnterVisual,
    EnterCommand,
    EnterOperator(char),

    // Input editing
    InsertChar(char),
    Newline,
    Backspace,
    DeleteWordBack,
    DeleteWordForward,
    DeleteAt,
    /// Visual-mode: yank the selection and return to normal.
    VisualYank,
    /// Visual-mode: delete the selection (→ normal).
    VisualDelete,
    /// Visual-mode: delete the selection and enter insert.
    VisualChange,
    DeleteLine,
    YankLine,
    Paste,
    Move(Dir),
    LineStart,
    LineEnd,

    // Command line
    CommandChar(char),
    CommandBackspace,
    RunCommand(String),
    CommandHistoryPrev,
    CommandHistoryNext,

    // Sent-message history (shell-style up/down in the composer)
    InputHistoryPrev,
    InputHistoryNext,

    // Submission / streaming
    Submit,
    /// Cancel the active session's in-flight stream.
    CancelStream,
    /// Attach a new stream for the given session id.
    AttachStream(usize, mpsc::Receiver<StreamEvent>),
    /// Stream events, each tagged with the session id they belong to.
    StreamToken(usize, String),
    StreamReasoning(usize, String),
    StreamUsage(usize, crate::api::Usage),
    StreamDone(usize),
    StreamError(usize, String),

    // Transcript scrolling (no cursor — read it in $EDITOR for motions)
    ChatTop,
    ChatBottom,
    ChatPageDown,
    ChatPageUp,
    ChatHalfDown,
    ChatHalfUp,
    ChatScroll(i32),
    /// Expand / collapse the full output of executed tools.
    ToggleOutput,
    /// A left-click in the transcript at (column, row) — toggles the individual
    /// tool output whose collapsible header sits on that row.
    ChatClick(u16, u16),
    /// Dismiss the transient `Notice` dialog.
    DismissNotice,

    // Open the current conversation in $EDITOR (read/search with real vim)
    OpenEditor,
    /// Toggle the file browser (open in $EDITOR), with edited files pre-selected.
    OpenEditPicker,
    /// Open one or more files in $EDITOR.
    OpenFilesInEditor(Vec<PathBuf>),
    /// Drop into an interactive shell, then return.
    OpenShell,

    // File browser (vim navigation + multi-select)
    BrowserDown,
    BrowserUp,
    BrowserParent,
    BrowserOpen,
    BrowserSelect,
    BrowserClose,

    // Startup launcher (resume a session or start new)
    StartupUp,
    StartupDown,
    StartupNew,
    StartupConfirm,

    // Sessions
    NewSession,
    /// Duplicate the active session into a new one and switch to it (branch the
    /// conversation to explore in parallel).
    ForkSession,
    DeleteSession,
    NextSession,
    PrevSession,
    OpenSessionPicker,
    SelectSession(usize),
    RenameSession(String),

    // Skills (toggleable instruction snippets)
    OpenSkillPicker,
    ToggleSkill(usize),

    // Models
    OpenModelPicker,
    SelectModel(String),
    NextModel,
    PrevModel,
    ModelsLoaded(Vec<String>),

    // Files / attachment
    OpenFilePicker,
    AttachFile(PathBuf),
    ClearAttachment,

    // Overlays (generic)
    OpenCommandPalette,
    OpenSettings,
    PickerUp,
    PickerDown,
    PickerConfirm,
    PickerCancel,
    PickerChar(char),
    PickerBackspace,
    SettingsLeft,
    SettingsRight,

    // @ mentions
    MentionUp,
    MentionDown,
    MentionAccept,
    MentionCancel,

    // Agent
    ToggleAgentMode,
    AgentPermitOnce,
    AgentPermitAll,
    AgentDenyOnce,
    AgentDenyAll,
    AgentToolResult(crate::agent::ToolResult),
    AgentCancel,

    // System prompt
    SetSystemPrompt(Option<String>),

    // UI / misc
    ToggleHelp,
    Resize,
    Quit,
}
