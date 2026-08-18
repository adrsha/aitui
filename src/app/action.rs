//! Every state change flows through this `Action` enum and the reducer in
//! `reducer.rs`. The input handler translates key/mouse events into actions; the
//! main loop translates channel events (stream tokens, tool results) into
//! actions. Side effects (spawning a request) are returned as follow-up actions.

use std::path::PathBuf;
use tokio::sync::mpsc;

use crate::api::StreamEvent;
use crate::app::state::SidebarTab;

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
    /// Read the system clipboard directly. Images become pending chat attachments;
    /// text falls through the normal smart-paste pipeline.
    PasteClipboard,
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
    /// Resolve a submit attempted while the main agent is still working.
    PromptDuringRunUp,
    PromptDuringRunDown,
    PromptDuringRunResolve,
    /// Send the oldest queued prompt for a session once that session becomes idle.
    SendQueuedPrompt(usize),
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
    #[allow(
        dead_code,
        reason = "retained as the action API for explicit stream cancellation"
    )]
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
    /// A generated image has been saved and is ready for inline display.
    StreamImageReady(usize, PathBuf),
    /// A non-streaming image-generation request failed.
    StreamImageError(usize, String),
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
    /// Scroll the active task/agent list in the sidebar when the pointer is over it.
    SidebarListScroll(i32),
    /// Switch the tabbed lower sidebar between checklist tasks and child agents.
    SelectSidebarTab(SidebarTab),
    /// Expand / collapse the full output of executed tools.
    ToggleOutput,
    /// Left mouse press at (column, row) — arms transcript text selection without
    /// activating any clickable control until the button is released.
    ChatPress(u16, u16),
    /// Activate the clickable control at (column, row). Mouse input dispatches this
    /// only from an undragged release; tests and keyboard-like callers may use it directly.
    ChatClick(u16, u16),
    /// Mouse drag in the transcript — updates the text selection.
    ChatDrag(u16, u16),
    /// Mouse button release at (column, row) — copies an active selection, or
    /// activates the released control when no drag occurred.
    ChatRelease(u16, u16),
    /// Dismiss a notice or child-agent detail dialog.
    DismissNotice,
    PrevSubtask,
    NextSubtask,
    SubtaskDetailUp,
    SubtaskDetailDown,
    /// Enter a child agent: chat + sidebar switch to its own content.
    InspectSubtask(u64),
    /// Leave the currently viewed agent, going up to its parent (or root).
    NavigateBack,

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

    // Sessions
    /// Reconcile session creations/removals written by another AiTUI client.
    SyncSessions,
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
    /// Optional model-generated follow-up replies for a completed session turn.
    ResponseSuggestionsReady(usize, u64, Vec<String>),
    /// Validated memory operations extracted from one completed session turn.
    SessionMemoryExtracted {
        session_id: usize,
        source_turn: u64,
        result: Result<Vec<crate::app::memory::MemoryOperation>, String>,
    },
    /// The parallel task-tracker agent revised the session checklist after a
    /// completed turn (per-item status/percent + overall progress).
    TodoUpdateReady(usize, u64, Result<crate::app::state::TodoUpdate, String>),
    /// Insert one displayed suggestion into the empty composer for editing.
    AcceptResponseSuggestion(usize),
    /// Enable or disable non-blocking follow-up suggestions.
    SetResponseSuggestions(bool),

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
    OpenCommandLine,
    OpenSettings,
    PickerUp,
    PickerDown,
    PickerConfirm,
    CommandLineNext,
    CommandLinePrev,
    CommandLineAccept,
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

    // Agent (always on)
    /// Ask the automated review model to judge the pending batch against the
    /// access-rule phrases currently selected in the permission overlay.
    AgentReviewPermission,
    /// Apply the currently-highlighted option in the permission menu directly.
    AgentResolvePermission,
    /// Quick keys: allow the current operation, allow every operation, or deny
    /// the current operation without opening the full menu. A deny opens the
    /// optional-reason box rather than resolving immediately for single calls.
    AgentQuickAllow,
    AgentQuickAllowAll,
    AgentQuickDeny,
    /// Back out of the deny reason box, returning to the permission menu.
    AgentDenyCancel,
    /// Scroll the command list in the permission prompt (independent of the
    /// allow/deny option selection).
    AgentPermScrollUp,
    AgentPermScrollDown,
    AgentPermScrollLeft,
    AgentPermScrollRight,
    /// Move between concrete operations in a multi-operation permission request.
    AgentPermissionOperationPrev,
    AgentPermissionOperationNext,
    /// Toggle typing for a custom directory, duration, or request count.
    /// Open or accept the popup selector for the highlighted access-rule phrase.
    AgentPermissionSelector,
    /// Navigate to the parent directory in the access-rule folder picker.
    AgentPermissionFolderParent,
    /// Close the access-rule popup selector without changing the current value.
    AgentPermissionSelectorCancel,
    /// Toggle direct editing for custom directory/count values.
    AgentPermissionCustom,
    /// Open the pending permission batch in `$EDITOR` to edit the commands.
    AgentPermissionEdit,
    /// The edited permission buffer came back from `$EDITOR`; apply it.
    AgentPermissionEdited(String),
    /// Open the exact session access entries; selected entries can be crossed off.
    OpenAccessManager,
    /// Edit the selected remembered access entry in the structured rule form.
    EditAccessEntry(usize),
    /// Remove the selected entry from the access manager.
    RemoveAccessEntry(usize),
    /// Disable automated permission review. If a review is currently in flight,
    /// cancel it and ask the user about that pending batch immediately.
    DisableAccessReview,
    /// Set (or clear, when empty) the natural-language session access policy the
    /// judge model uses to auto-allow/deny tool calls.
    SetAccessPolicy(String),
    /// Select the default automated permission-review strictness.
    SetAccessReviewMode(crate::config::AccessReviewMode),
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
    AgentDecisionCustom,
    AgentDecisionEdit,
    AgentDecisionEdited(String),
    AgentResolveDecision,
    AgentPlanEdit,
    AgentPlanAccept,
    AgentPlanDeny,
    AgentToolResult(crate::agent::ToolResult),
    AgentToolBatchResult(Vec<crate::agent::ToolResult>),
    SubtaskEvent(crate::app::state::SubtaskEvent),
    AgentCancel,
    /// The model emitted tool calls while agent mode is off: enable agent mode
    /// and run them, or decline and let the model answer without tools.
    AgentEnableTools,
    AgentDeclineTools,

    // System prompt
    SetSystemPrompt(Option<String>),

    // UI / misc
    /// Action selected from an OS desktop notification.
    DesktopNotification(crate::app::notify::DesktopResponse),
    FocusGained,
    FocusLost,
    ToggleHelp,
    HelpUp,
    HelpDown,
    HelpPageUp,
    HelpPageDown,
    HelpSelect,
    HelpBack,
    Resize,
    Quit,
}
