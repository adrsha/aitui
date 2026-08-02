use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::state::App;
use crate::render::theme::Theme;
use crate::ui::layout;

pub(crate) const BUFFER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
const DOTS: [&str; 4] = ["·  ", "·· ", "···", " ··"];
const WAITING_PHRASES: [&str; 10] = [
    "waiting for the first word",
    "listening for the response",
    "holding the line open",
    "waiting on the model",
    "watching for the first token",
    "giving the model a moment",
    "keeping the request warm",
    "waiting for the reply to begin",
    "standing by for an answer",
    "checking the response channel",
];
const GENERATING_PHRASES: [&str; 10] = [
    "shaping the response",
    "writing the next part",
    "following the thread",
    "turning thoughts into words",
    "building the answer",
    "working through the details",
    "connecting the useful pieces",
    "finishing the explanation",
    "keeping the answer coherent",
    "bringing the response together",
];
const PREPARING_TOOL_PHRASES: [&str; 10] = [
    "assembling the tool request",
    "checking the tool arguments",
    "preparing the next operation",
    "lining up the tool call",
    "making the tool request precise",
    "filling in the operation details",
    "choosing the right tool action",
    "getting the call into shape",
    "verifying the requested operation",
    "packing the tool arguments",
];
const TOOL_PHRASES: [&str; 10] = [
    "working through the tool call",
    "letting the tool do its part",
    "checking the operation output",
    "running the requested operation",
    "waiting for the tool result",
    "following the operation through",
    "collecting the tool output",
    "keeping an eye on the command",
    "finishing the current operation",
    "bringing the tool result back",
];
const IMAGE_PHRASES: [&str; 10] = [
    "composing the image",
    "laying out the visual",
    "rendering the requested scene",
    "working through the visual details",
    "building the image layer by layer",
    "balancing the composition",
    "turning the prompt into pixels",
    "refining the generated image",
    "painting in the remaining details",
    "waiting for the image renderer",
];
const REVIEW_PHRASES: [&str; 10] = [
    "asking the review model",
    "checking the access policy",
    "reviewing the requested operations",
    "weighing the permission rules",
    "matching calls against the policy",
    "double-checking the access scope",
    "waiting for the review verdict",
    "examining the requested permissions",
    "testing the calls against the rules",
    "letting the reviewer inspect the batch",
];
const MODEL_LOADING_PHRASES: [&str; 10] = [
    "loading the model list",
    "checking available models",
    "refreshing model choices",
    "asking the endpoint for models",
    "waiting on model metadata",
    "collecting available model names",
    "syncing the model catalog",
    "checking model availability",
    "updating the model menu",
    "reading the endpoint capabilities",
];
const QUEUED_TOOL_PHRASES: [&str; 10] = [
    "organizing the next tool steps",
    "working through the tool queue",
    "lining up approved operations",
    "moving to the next tool call",
    "keeping the agent round moving",
    "sorting the remaining operations",
    "preparing the next approved step",
    "advancing through the tool batch",
    "checking what runs next",
    "coordinating the pending operations",
];
const LOOP_PHRASES: [&str; 10] = [
    "continuing the autonomous pass",
    "moving the goal forward",
    "checking the loop criteria",
    "working through the next loop step",
    "keeping the autonomous run focused",
    "making another concrete pass",
    "verifying progress toward the goal",
    "following the loop plan",
    "closing in on the stop criteria",
    "continuing the agent loop",
];
const DECISION_PHRASES: [&str; 10] = [
    "waiting for your decision",
    "holding at the approval step",
    "waiting on your selection",
    "keeping the pending choice ready",
    "pausing for your review",
    "waiting for confirmation",
    "standing by at the decision point",
    "keeping the options open",
    "waiting for your direction",
    "ready when you choose",
];
const RETRY_PHRASES: [&str; 10] = [
    "retrying the quiet connection",
    "asking the endpoint again",
    "reopening the response stream",
    "giving the request another try",
    "recovering the model response",
    "waiting through a retry",
    "reconnecting to the reply",
    "nudging the request back into motion",
    "trying the response channel again",
    "restoring the interrupted request",
];
const SUGGESTION_PHRASES: [&str; 10] = [
    "drafting possible follow-ups",
    "thinking of useful next replies",
    "preparing response suggestions",
    "finding a sensible next question",
    "sketching a few follow-up options",
    "looking for the natural next step",
    "building quick reply choices",
    "considering what you might ask next",
    "shaping optional follow-up prompts",
    "collecting a few next-message ideas",
];
const BACKGROUND_PHRASES: [&str; 10] = [
    "keeping a background session moving",
    "listening to another session",
    "watching background work finish",
    "letting another session continue",
    "tracking work in the background",
    "checking a background response",
    "keeping parallel work in motion",
    "waiting on another active session",
    "following the background stream",
    "bringing parallel work along",
];
const GENERAL_PHRASES: [&str; 10] = [
    "following the breadcrumbs",
    "checking the sharp edges",
    "asking the code nicely",
    "untangling the interesting bit",
    "reading between the lines",
    "keeping the gremlins supervised",
    "making the pieces agree",
    "double-checking the clever part",
    "working through the next detail",
    "keeping everything coordinated",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivityState {
    Waiting,
    Generating,
    PreparingTool,
    Tool,
    Image,
    Review,
    ModelLoading,
    QueuedTool,
    Loop,
    Decision,
    Retry,
    Suggestion,
    Background,
    General,
}

pub(crate) fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

pub(crate) fn frame<'a>(frames: &'a [&'a str], ms: u128, speed: u128) -> &'a str {
    frames[((ms / speed) as usize) % frames.len()]
}

fn chip_fg(bg: Color) -> Color {
    match bg {
        Color::Black | Color::DarkGray | Color::Red | Color::Magenta => Color::White,
        _ => Color::Black,
    }
}

fn chip(text: impl Into<String>, bg: Color) -> Span<'static> {
    Span::styled(
        format!(" {} ", text.into()),
        Style::default()
            .bg(bg)
            .fg(crate::render::theme::fg_guard(chip_fg(bg)))
            .add_modifier(Modifier::BOLD),
    )
}

fn distinct_bg(preferred: Color, previous: Color) -> Color {
    if preferred != previous {
        return preferred;
    }
    [Color::Red, Color::Blue, Color::Yellow, Color::DarkGray]
        .into_iter()
        .find(|candidate| *candidate != previous)
        .unwrap_or(Color::Black)
}

#[cfg(test)]
fn agent_chip(agent_mode: bool) -> (&'static str, Color) {
    if agent_mode {
        ("agent on", Color::Red)
    } else {
        ("agent off", Color::DarkGray)
    }
}

fn cwd_label(app: &App) -> String {
    let cwd = app
        .sessions
        .active()
        .cwd
        .as_ref()
        .cloned()
        .or_else(|| std::env::current_dir().ok());
    let Some(cwd) = cwd else {
        return "--".to_string();
    };
    crate::render::path::display_path(&cwd)
}

fn activity_phrases(state: ActivityState) -> &'static [&'static str] {
    match state {
        ActivityState::Waiting => &WAITING_PHRASES,
        ActivityState::Generating => &GENERATING_PHRASES,
        ActivityState::PreparingTool => &PREPARING_TOOL_PHRASES,
        ActivityState::Tool => &TOOL_PHRASES,
        ActivityState::Image => &IMAGE_PHRASES,
        ActivityState::Review => &REVIEW_PHRASES,
        ActivityState::ModelLoading => &MODEL_LOADING_PHRASES,
        ActivityState::QueuedTool => &QUEUED_TOOL_PHRASES,
        ActivityState::Loop => &LOOP_PHRASES,
        ActivityState::Decision => &DECISION_PHRASES,
        ActivityState::Retry => &RETRY_PHRASES,
        ActivityState::Suggestion => &SUGGESTION_PHRASES,
        ActivityState::Background => &BACKGROUND_PHRASES,
        ActivityState::General => &GENERAL_PHRASES,
    }
}

fn activity_phrase(state: ActivityState, ms: u128) -> &'static str {
    let phrases = activity_phrases(state);
    phrases[((ms / 2_600) as usize) % phrases.len()]
}

fn activity_state(app: &App) -> Option<ActivityState> {
    let active = app.sessions.active_id();
    if app
        .judging
        .as_ref()
        .is_some_and(|batch| batch.session_id == active)
    {
        return Some(ActivityState::Review);
    }
    if app.agent_session == Some(active)
        && (app.active_tool.is_some() || app.agent_tool_rx.is_some())
    {
        return Some(ActivityState::Tool);
    }
    if app
        .preparing_tool
        .as_ref()
        .is_some_and(|(session_id, _, _)| *session_id == active)
    {
        return Some(ActivityState::PreparingTool);
    }
    let active_stream = app
        .streams
        .iter()
        .find(|stream| stream.session_id == active);
    if active_stream.is_some() && crate::api::is_image_model(app.current_model()) {
        return Some(ActivityState::Image);
    }
    if active_stream.is_some_and(|stream| stream.cold_retries > 0) {
        return Some(ActivityState::Retry);
    }
    if app.sessions.active().loop_state.is_some() && app.is_busy() {
        return Some(ActivityState::Loop);
    }
    if app.sessions.active().is_streaming() || active_stream.is_some() {
        let has_output = app
            .sessions
            .active()
            .streaming_display()
            .is_some_and(|text| !text.trim().is_empty());
        return Some(if has_output {
            ActivityState::Generating
        } else {
            ActivityState::Waiting
        });
    }
    if app.agent_session == Some(active)
        && (!app.pending_tools.is_empty() || !app.approved.is_empty())
    {
        return Some(ActivityState::QueuedTool);
    }
    if matches!(
        app.overlay,
        crate::app::overlay::Overlay::Permission(_)
            | crate::app::overlay::Overlay::Decision(_)
            | crate::app::overlay::Overlay::Plan(_)
    ) {
        return Some(ActivityState::Decision);
    }
    if app
        .suggestion_inflight
        .iter()
        .any(|(session_id, _)| *session_id == active)
    {
        return Some(ActivityState::Suggestion);
    }
    if app.model_load == crate::app::state::ModelLoad::Loading {
        return Some(ActivityState::ModelLoading);
    }
    if app
        .task_barrier
        .as_ref()
        .is_some_and(|barrier| barrier.session_id == active)
    {
        return Some(ActivityState::Waiting);
    }
    if !app.streams.is_empty() {
        return Some(ActivityState::Background);
    }
    app.is_busy().then_some(ActivityState::General)
}

fn idle_title() -> (&'static str, &'static str) {
    ("✓", "IDLE")
}

/// A short per-state status word + icon for the ACTIVITY bar chip.
fn activity_title(state: ActivityState) -> (&'static str, &'static str) {
    match state {
        ActivityState::Waiting => ("·", "WAITING"),
        ActivityState::Generating => ("✦", "GENERATING"),
        ActivityState::PreparingTool => ("⚙", "PREPARING"),
        ActivityState::Tool => ("⧉", "TOOL"),
        ActivityState::Image => ("▣", "IMAGE"),
        ActivityState::Review => ("⚖", "REVIEW"),
        ActivityState::ModelLoading => ("☁", "MODELS"),
        ActivityState::QueuedTool => ("▤", "QUEUED"),
        ActivityState::Loop => ("⟳", "LOOP"),
        ActivityState::Decision => ("❔", "DECISION"),
        ActivityState::Retry => ("↻", "RETRY"),
        ActivityState::Suggestion => ("✎", "SUGGESTIONS"),
        ActivityState::Background => ("⧗", "BACKGROUND"),
        ActivityState::General => ("✦", "WORKING"),
    }
}

/// The concrete status text for the ACTIVITY bar: the tool being prepared/run,
/// explicit "Waiting for response", or a rotating phrase when nothing more
/// specific is known.
fn activity_label(state: ActivityState, app: &App, ms: u128, max_cols: usize) -> String {
    let fallback = |label: String| -> String {
        let phrase = activity_phrase(state, ms);
        if label.len() < max_cols / 2 {
            format!("{} · {}", label, phrase)
        } else {
            label
        }
    };
    match state {
        ActivityState::Waiting => {
            if app
                .task_barrier
                .as_ref()
                .is_some_and(|barrier| barrier.session_id == app.sessions.active_id())
            {
                "Waiting for delegated work…".to_string()
            } else {
                "Waiting for response…".to_string()
            }
        }
        ActivityState::Generating => fallback("Shaping the reply…".to_string()),
        ActivityState::PreparingTool => {
            let name = app
                .preparing_tool
                .as_ref()
                .map(|(_, name, _)| name.as_str())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("tool call");
            format!("Preparing {}…", name)
        }
        ActivityState::Tool => {
            let summary = app
                .active_tool
                .as_ref()
                .map(|(summary, _)| summary.clone())
                .unwrap_or_else(|| "tool".to_string());
            let summary = crate::render::path::abbreviate_home(&summary);
            let summary: String = summary.chars().take(max_cols.saturating_sub(12)).collect();
            format!("Running {}…", summary)
        }
        ActivityState::Image => fallback("Generating image…".to_string()),
        ActivityState::Review => fallback("Reviewing access…".to_string()),
        ActivityState::ModelLoading => fallback("Loading models…".to_string()),
        ActivityState::QueuedTool => fallback("Queueing approved tools…".to_string()),
        ActivityState::Loop => {
            let iter = app
                .sessions
                .active()
                .loop_state
                .as_ref()
                .map(|l| format!("{} / {}", l.iteration, l.max))
                .unwrap_or_default();
            fallback(format!("Loop pass {}…", iter))
        }
        ActivityState::Decision => "Waiting for your decision…".to_string(),
        ActivityState::Retry => fallback("Retrying the request…".to_string()),
        ActivityState::Suggestion => fallback("Preparing suggestions…".to_string()),
        ActivityState::Background => fallback("Background work…".to_string()),
        ActivityState::General => fallback("Working…".to_string()),
    }
}

fn buffering(app: &App, ms: u128) -> &'static str {
    if app.is_busy() || !app.streams.is_empty() {
        frame(&BUFFER, ms, 110)
    } else {
        "·"
    }
}

/// One-line activity denoter: a colored state chip (icon + status word) on a
/// block background, then an animated spinner and the concrete status text.
pub fn render_activity(
    f: &mut Frame,
    app: &App,
    area: Rect,
    theme: &Theme,
) -> Vec<crate::app::state::SubtaskHitbox> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let ms = now_ms();
    let state = activity_state(app);
    let surface = theme.surface();
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::styled(" ", surface));
    if let Some(state) = state {
        let (icon, title) = activity_title(state);
        let chip_color = match state {
            ActivityState::Waiting => Color::DarkGray,
            ActivityState::Generating => theme.accent,
            ActivityState::PreparingTool => theme.accent,
            ActivityState::Tool => theme.accent,
            ActivityState::Image => theme.accent,
            ActivityState::Review => theme.warning,
            ActivityState::ModelLoading => theme.accent,
            ActivityState::QueuedTool => Color::DarkGray,
            ActivityState::Loop => theme.warning,
            ActivityState::Decision => theme.warning,
            ActivityState::Retry => theme.danger,
            ActivityState::Suggestion => theme.accent,
            ActivityState::Background => Color::DarkGray,
            ActivityState::General => theme.accent,
        };
        let chip_style = Style::default()
            .bg(chip_color)
            .fg(crate::render::theme::fg_guard(chip_fg(chip_color)))
            .add_modifier(Modifier::BOLD);
        spans.push(Span::styled(format!(" {}  {} ", icon, title), chip_style));
        spans.push(Span::styled(" ", surface));
        spans.push(Span::styled(
            frame(&BUFFER, ms, 110),
            Style::default().fg(chip_color).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(" ", surface));
        let budget = area.width.saturating_sub(20) as usize;
        let label = activity_label(state, app, ms, budget.max(8));
        let label: String = label.chars().take(budget.max(8)).collect();
        spans.push(Span::styled(label, Style::default().fg(theme.text)));
    } else {
        let (icon, title) = idle_title();
        let idle_bg = Color::DarkGray;
        spans.push(Span::styled(
            format!(" {} {} ", icon, title),
            Style::default()
                .bg(idle_bg)
                .fg(crate::render::theme::fg_guard(chip_fg(idle_bg)))
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled("  Ready", Style::default().fg(theme.muted)));
    }
    use crate::app::state::ModelLoad;
    let model = match app.model_load {
        ModelLoad::Loading => "loading…".to_string(),
        ModelLoad::Failed => "unavailable".to_string(),
        ModelLoad::Loaded => app.current_model().to_string(),
    };
    spans.push(Span::styled("   MODEL: ", Style::default().fg(theme.muted)));
    spans.push(Span::styled(
        model,
        Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
    ));
    f.render_widget(Paragraph::new(Line::from(spans)).style(surface), area);
    Vec::new()
}

pub fn render(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let session = app.sessions.active();
    let ms = now_ms();

    use crate::input::vim::VimMode;
    let (mode_label, mode_bg) = match app.vim {
        VimMode::Normal => ("NORMAL", Color::DarkGray),
        VimMode::Insert => ("INSERT", theme.accent),
        VimMode::Visual if app.input.visual_line => ("V-LINE", Color::Yellow),
        VimMode::Visual => ("VISUAL", Color::Yellow),
        VimMode::Operator(_) => ("OP", Color::Red),
    };

    let mut chips: Vec<Span<'static>> = vec![chip(mode_label, mode_bg)];
    let buffer_bg = distinct_bg(theme.subtle_pill, mode_bg);
    chips.push(chip(buffering(app, ms), buffer_bg));

    // Agent always on — compact symbol
    let agent_bg = distinct_bg(Color::Red, buffer_bg);
    chips.push(chip("◆", agent_bg));

    // Reasoning — compact symbols
    let mut reasoning = Vec::new();
    if let Some(loop_state) = session.loop_state.as_ref() {
        reasoning.push(format!("⟳{}/{}", loop_state.iteration, loop_state.max));
    }
    if let Some(effort) = app.reasoning_effort.as_deref() {
        let e: String = effort
            .chars()
            .take(1)
            .flat_map(|c| c.to_uppercase())
            .collect();
        reasoning.push(e);
    }
    if let Some(mode) = app.reasoning_mode.as_deref() {
        let m: String = mode
            .chars()
            .take(1)
            .flat_map(|c| c.to_uppercase())
            .collect();
        reasoning.push(m);
    }
    let reasoning_bg = distinct_bg(Color::Yellow, agent_bg);
    chips.push(chip(
        if reasoning.is_empty() {
            "S".to_string()
        } else {
            reasoning.join("·")
        },
        reasoning_bg,
    ));

    // Model name when side panel is hidden (narrow terminal)
    let show_model_cwd = layout::sidebar_width(area.width) == 0;
    if show_model_cwd {
        use crate::app::state::ModelLoad;
        let model_label = match app.model_load {
            ModelLoad::Loading => frame(&DOTS, ms, 260).to_string(),
            ModelLoad::Failed => "×".to_string(),
            ModelLoad::Loaded => app.current_model().to_string(),
        };
        chips.push(chip(model_label, theme.accent));
        chips.push(chip(cwd_label(app), Color::DarkGray));
    }

    f.render_widget(Paragraph::new(Line::from(chips)), area);
}

#[cfg(test)]
mod tests {
    use super::{
        activity_phrase, activity_phrases, activity_title, agent_chip, chip_fg, distinct_bg, frame,
        idle_title, ActivityState, DOTS,
    };
    use ratatui::style::Color;

    #[test]
    fn frame_cycles_by_time() {
        assert_eq!(frame(&DOTS, 0, 100), DOTS[0]);
        assert_eq!(frame(&DOTS, 100, 100), DOTS[1]);
    }

    #[test]
    fn activity_phrase_rotates_slowly() {
        assert_eq!(
            activity_phrase(ActivityState::Generating, 0),
            activity_phrase(ActivityState::Generating, 2_599)
        );
        assert_ne!(
            activity_phrase(ActivityState::Generating, 0),
            activity_phrase(ActivityState::Generating, 2_600)
        );
    }

    #[test]
    fn every_activity_state_has_a_large_phrase_pool() {
        for state in [
            ActivityState::Waiting,
            ActivityState::Generating,
            ActivityState::PreparingTool,
            ActivityState::Tool,
            ActivityState::Image,
            ActivityState::Review,
            ActivityState::ModelLoading,
            ActivityState::QueuedTool,
            ActivityState::Loop,
            ActivityState::Decision,
            ActivityState::Retry,
            ActivityState::Suggestion,
            ActivityState::Background,
            ActivityState::General,
        ] {
            assert!(activity_phrases(state).len() >= 10);
        }
    }

    #[test]
    fn every_status_state_has_a_readable_icon_and_title() {
        let (idle_icon, idle_label) = idle_title();
        assert!(!idle_icon.trim().is_empty());
        assert_eq!(idle_label, "IDLE");

        for state in [
            ActivityState::Waiting,
            ActivityState::Generating,
            ActivityState::PreparingTool,
            ActivityState::Tool,
            ActivityState::Image,
            ActivityState::Review,
            ActivityState::ModelLoading,
            ActivityState::QueuedTool,
            ActivityState::Loop,
            ActivityState::Decision,
            ActivityState::Retry,
            ActivityState::Suggestion,
            ActivityState::Background,
            ActivityState::General,
        ] {
            let (icon, title) = activity_title(state);
            assert!(!icon.trim().is_empty(), "missing icon for {state:?}");
            assert!(!title.trim().is_empty(), "missing title for {state:?}");
        }
    }

    #[test]
    fn light_blue_status_chips_use_dark_text() {
        assert_eq!(chip_fg(Color::Blue), Color::Black);
        assert_eq!(chip_fg(Color::DarkGray), Color::White);
    }

    #[test]
    fn repeated_status_backgrounds_are_replaced() {
        assert_eq!(distinct_bg(Color::Blue, Color::Red), Color::Blue);
        assert_ne!(distinct_bg(Color::Cyan, Color::Cyan), Color::Cyan);
    }

    #[test]
    fn agent_chip_reflects_active_session_mode() {
        assert_eq!(agent_chip(true), ("agent on", Color::Red));
        assert_eq!(agent_chip(false), ("agent off", Color::DarkGray));
    }
}
