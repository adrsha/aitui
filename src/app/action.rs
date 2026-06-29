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
    SetPendingEscape(char),

    // Input editing
    InsertChar(char),
    Newline,
    Backspace,
    DeleteAt,
    DeleteLine,
    DeleteWordForward,
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

    // Submission / streaming
    Submit,
    CancelStream,
    AttachStream(mpsc::Receiver<StreamEvent>),
    StreamToken(String),
    StreamReasoning(String),
    StreamDone,
    StreamError(String),

    // Chat navigation
    ChatDown,
    ChatUp,
    ChatLeft,
    ChatRight,
    ChatWordForward,
    ChatWordBackward,
    ChatLineStart,
    ChatLineEnd,
    ChatTop,
    ChatBottom,
    ChatPageDown,
    ChatPageUp,
    ChatScroll(i32),
    ChatToggle,
    ChatYank,
    ChatOpenLink,

    // Focus
    FocusChat,
    FocusSidebar,
    FocusInput,
    CycleFocus,

    // Sessions
    NewSession,
    DeleteSession,
    NextSession,
    PrevSession,
    SelectSession(usize),
    RenameSession(String),

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
    ToggleSidebar,
    ClearStatus,
    MouseClick(u16, u16),
    Resize,
    Quit,
}
