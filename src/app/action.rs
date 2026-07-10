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
    WordEnd,
}

#[derive(Debug)]
pub enum Action {
    // Modes
    EnterInsert,
    EnterNormal,
    EnterVisual,
    /// Line-wise visual selection (`V`).
    EnterVisualLine,
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
    ChangeLine,
    DeleteTo(Dir),
    ChangeTo(Dir),
    DeleteToLineEnd,
    ChangeToLineEnd,
    YankToLineEnd,
    YankTo(Dir),
    OpenLineBelow,
    OpenLineAbove,
    UndoInput,
    RedoInput,
    YankLine,
    Paste,
    /// A bracketed paste from the terminal. Large → saved to a file and attached;
    /// medium → stored and shown as a compact `[PASTED#N-…]` chip; small → inserted.
    PasteText(String),
    Move(Dir),
    LineStart,
    FirstNonBlank,
    LineEnd,

    // Command palette — `:`/`/` open an overlay; RunCommand runs the typed line.
    RunCommand(String),

    // Sent-message history (shell-style up/down in the composer)
    InputHistoryPrev,
    InputHistoryNext,

    // Submission / streaming
    Submit,
    /// Regenerate the last assistant reply: drop it and resend the last user turn.
    RetryLast,
    /// Pull the last user message back into the composer for editing (removing that
    /// turn and its reply).
    EditLast,
    /// Copy the last assistant reply to the system clipboard.
    CopyLastReply,
    /// Copy the last fenced code block from the last assistant reply to the clipboard.
    CopyLastCode,
    /// Cancel the active session's in-flight stream.
    CancelStream,
    /// Attach a new stream for the given session id.
    AttachStream(usize, mpsc::Receiver<StreamEvent>),
    /// Attach a restarted stream for the same turn, preserving the cold retry count.
    RetryStream(usize, u8),
    /// Attach a restarted stream for the given session id.
    AttachRetriedStream(usize, mpsc::Receiver<StreamEvent>, u8),
    /// Stream events, each tagged with the session id they belong to.
    StreamToken(usize, String),
    StreamReasoning(usize, String),
    StreamUsage(usize, crate::api::Usage),
    /// Native tool-call metadata arrived before the complete runnable call.
    StreamToolCallStarted(usize, String),
    StreamDone(usize),
    StreamError(usize, String),
    /// Start (or queue) the agent tool round for a session whose stream was cut
    /// early because a complete tool call was detected mid-generation.
    StartAgentRound(usize),
    /// Open the API endpoint/key setup prompt (prefilled from config).
    OpenApiSetup,

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
    SessionTitleGenerated(usize, String),

    // Skills (toggleable instruction snippets)
    OpenSkillPicker,
    ToggleSkill(usize),

    // Models
    OpenModelPicker,
    SelectModel(String),
    NextModel,
    PrevModel,
    ReloadModels,
    ModelsLoaded(Vec<String>),
    /// The `/v1/models` fetch failed (connection/timeout) — fall back to mock.
    ModelsFailed,

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

    // Sessions (from the picker)
    /// Delete the session at the given index in the picker list.
    DeleteSessionAt(usize),

    // Agent
    ToggleAgentMode,
    /// Apply the currently-highlighted option in the permission menu.
    AgentResolvePermission,
    /// Quick keys: allow / deny this one call without opening the full menu.
    AgentQuickAllow,
    AgentQuickDeny,
    /// Scroll the command list in the permission prompt (independent of the
    /// allow/deny option selection).
    AgentPermScrollUp,
    AgentPermScrollDown,
    /// Open the pending permission batch in `$EDITOR` to edit the commands.
    AgentPermissionEdit,
    /// The edited permission buffer came back from `$EDITOR`; apply it.
    AgentPermissionEdited(String),
    /// Set (or clear, when empty) the natural-language session access policy the
    /// judge model uses to auto-allow/deny tool calls.
    SetAccessPolicy(String),
    /// Open `$EDITOR` to write/revise the session access policy (from the prompt).
    AgentEditPolicy,
    /// The judge model's verdicts for the in-flight batch came back (per-call).
    AccessJudged(usize, Vec<crate::agent::AccessVerdict>),
    /// Start an autonomous loop with the given goal (default stop criteria + cap).
    StartLoop(String),
    /// Open `$EDITOR` to specify a loop (goal / stop criteria / max iterations).
    AgentEditLoop,
    /// The loop spec came back from `$EDITOR`; parse its fields and start the loop.
    StartLoopSpec(String),
    /// Stop the active session's autonomous loop.
    StopLoop,
    AgentDecisionToggle,
    AgentResolveDecision,
    AgentPlanEdit,
    AgentPlanAccept,
    AgentPlanDeny,
    AgentToolResult(crate::agent::ToolResult),
    AgentCancel,
    /// The model emitted tool calls while agent mode is off: enable agent mode
    /// and run them, or decline and let the model answer without tools.
    AgentEnableTools,
    AgentDeclineTools,

    // System prompt
    SetSystemPrompt(Option<String>),

    // UI / misc
    FocusGained,
    FocusLost,
    ToggleHelp,
    Resize,
    Quit,
}
