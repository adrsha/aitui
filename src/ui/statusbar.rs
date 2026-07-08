use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::agent::tools::{PermissionDecision, PermissionScope};
use crate::app::state::App;
use crate::render::theme::Theme;

const DOTS: [&str; 4] = ["·  ", "·· ", "···", " ··"];
const MOON: [&str; 8] = ["○", "◔", "◑", "◕", "●", "◕", "◑", "◔"];
const PIPES: [&str; 4] = ["┤", "┘", "┴", "└"];
const BALL: [&str; 5] = ["●    ", " ●   ", "  ●  ", "   ● ", "    ●"];
const RIPPLE: [&str; 6] = ["·∙●∙·", "∙●○●∙", "●○·○●", "∙●○●∙", "·∙●∙·", "  ·  "];
const FLIP: [&str; 4] = ["-", "`", "'", ","];
const NINE_DOTS: [&str; 4] = ["⠁⠂⠄", "⠂⠄⡀", "⠄⡀⢀", "⡀⢀⠁"];

#[derive(Clone, Copy)]
enum Motion {
    Dots,
    Moon,
    Pipes,
    Ball,
    Ripple,
    Flip,
    NineDots,
}

impl Motion {
    fn frames(self) -> &'static [&'static str] {
        match self {
            Motion::Dots => &DOTS,
            Motion::Moon => &MOON,
            Motion::Pipes => &PIPES,
            Motion::Ball => &BALL,
            Motion::Ripple => &RIPPLE,
            Motion::Flip => &FLIP,
            Motion::NineDots => &NINE_DOTS,
        }
    }

    fn speed(self) -> u128 {
        match self {
            Motion::Dots => 260,
            Motion::Moon => 180,
            Motion::Pipes => 150,
            Motion::Ball => 140,
            Motion::Ripple => 170,
            Motion::Flip => 130,
            Motion::NineDots => 160,
        }
    }

    fn frame(self, ms: u128) -> &'static str {
        frame(self.frames(), ms, self.speed())
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn frame<'a>(frames: &'a [&'a str], ms: u128, speed: u128) -> &'a str {
    frames[((ms / speed) as usize) % frames.len()]
}

fn flavour<'a>(lines: &'a [&'a str], ms: u128) -> &'a str {
    lines[((ms / 2_600) as usize) % lines.len()]
}

fn secs(ms: u64) -> String {
    format!("{:.1}s", ms as f64 / 1000.0)
}

fn tool_activity(summary: &str) -> (&'static str, Motion, &'static [&'static str]) {
    if summary.starts_with("read(") {
        (
            "reading",
            Motion::Dots,
            &[
                "checking the actual file, rude but effective",
                "eyes on bytes",
                "file said something; verifying",
                "opening it like a responsible adult",
                "not trusting memory on this one",
                "consulting the source of alleged truth",
                "scrolling with professional suspicion",
                "byte archaeology, very glamorous",
                "letting the file explain itself",
            ],
        )
    } else if summary.starts_with("list(") {
        (
            "listing",
            Motion::Ball,
            &[
                "counting folders without judging them",
                "small directory census",
                "seeing what lives there",
                "checking the neighborhood",
                "peeking into the file cabinet",
                "directory roll call",
                "making the tree introduce itself",
                "folders are standing in line",
                "asking the path who it knows",
            ],
        )
    } else if summary.starts_with("search(") {
        (
            "searching",
            Motion::Ripple,
            &[
                "asking grep to be normal",
                "looking for the suspicious string",
                "pattern net is out",
                "following the regex crumbs",
                "interrogating text at scale",
                "needle, meet haystack",
                "checking who said the thing",
                "letting ripgrep do cardio",
                "looking busy, actually useful",
            ],
        )
    } else if summary.starts_with("shell(") {
        (
            "running",
            Motion::Flip,
            &[
                "subprocess has the wheel",
                "waiting on an exit code",
                "terminal is doing terminal things",
                "command is off leash, briefly",
                "letting stdout finish its sentence",
                "shell has entered the chat",
                "build gods are being consulted",
                "process is thinking in monospace",
                "watching for the return code",
            ],
        )
    } else if summary.starts_with("edit(") {
        (
            "editing",
            Motion::Pipes,
            &[
                "one careful cut",
                "diff is on the table",
                "measuring twice",
                "tiny scalpel, steady hand",
                "moving only the requested furniture",
                "patching without redecorating",
                "keeping the blast radius small",
                "making the smallest honest change",
                "line surgery in progress",
            ],
        )
    } else if summary.starts_with("write(") {
        (
            "writing",
            Motion::Pipes,
            &[
                "putting bytes where bytes go",
                "making the file real",
                "saving, not improvising",
                "committing words to disk, emotionally",
                "laying down fresh text",
                "file is getting its new outfit",
                "placing content carefully",
                "writing it down so nobody has to remember",
                "bytes are moving in",
            ],
        )
    } else if summary.starts_with("web_search(") {
        (
            "searching web",
            Motion::NineDots,
            &[
                "asking the internet politely",
                "opening a few tabs in spirit",
                "checking what changed since forever",
                "consulting the chaos index",
                "looking for current humans saying things",
                "search engine is pretending to be helpful",
                "checking the outside world",
                "fishing links out of the soup",
                "web is being mildly cooperative",
            ],
        )
    } else if summary.starts_with("web_fetch(") {
        (
            "fetching page",
            Motion::NineDots,
            &[
                "reading the page, not the vibes",
                "extracting the useful bits",
                "loading words from elsewhere",
                "bringing receipts",
                "page is unfolding",
                "turning HTML into something civilized",
                "checking the cited source",
                "pulling text out of the web drawer",
                "waiting for the page to stop being coy",
            ],
        )
    } else if summary.starts_with("download(") {
        (
            "downloading",
            Motion::Ball,
            &[
                "bringing the file home",
                "packet commute in progress",
                "saving the thing",
                "bits are taking the scenic route",
                "catching bytes with a bucket",
                "file is in transit",
                "download goblin is employed",
                "network is passing notes",
                "making a local copy of the universe",
            ],
        )
    } else if summary.starts_with("todo(") {
        (
            "updating list",
            Motion::Dots,
            &[
                "tiny project manager moment",
                "moving the little boxes",
                "keeping score",
                "checkbox bureaucracy, but cute",
                "turning chaos into bullet points",
                "list is receiving attention",
                "tasks are being herded",
                "status board is getting its haircut",
                "making the plan look less haunted",
            ],
        )
    } else {
        (
            "using tool",
            Motion::Moon,
            &[
                "letting the tool answer",
                "waiting for the useful part",
                "checking the machinery",
                "tool is doing its little job",
                "outsourcing the boring bit",
                "machinery sounds healthy enough",
                "waiting with tasteful restraint",
                "small lever, hopefully large outcome",
                "tool has the conch",
            ],
        )
    }
}

fn busy_label(app: &App, ms: u128) -> Option<String> {
    let session = app.sessions.active();

    if let Some((summary, started)) = &app.active_tool {
        let elapsed = secs(started.elapsed().as_millis() as u64);
        let (verb, motion, notes) = tool_activity(summary);
        return Some(format!(
            "{} {} · {} · {} · {}",
            motion.frame(ms),
            verb,
            summary,
            elapsed,
            flavour(notes, ms)
        ));
    }

    if app.agent_tool_rx.is_some() {
        let motion = Motion::Flip;
        return Some(format!(
            "{} executing tool · {}",
            motion.frame(ms),
            flavour(
                &[
                    "stdout is thinking about it",
                    "waiting on the exit code",
                    "tool has not texted back yet",
                    "process is still typing",
                    "command has the floor",
                    "stderr is being dramatic, maybe",
                    "waiting for the shell to blink",
                    "tool is finishing its monologue",
                    "collecting the final line",
                ],
                ms
            )
        ));
    }

    if !app.pending_tools.is_empty() {
        let motion = Motion::Dots;
        return Some(format!(
            "{} staging {} tool{} · {}",
            motion.frame(ms),
            app.pending_tools.len(),
            if app.pending_tools.len() == 1 {
                ""
            } else {
                "s"
            },
            flavour(
                &[
                    "permission slip on the clipboard",
                    "waiting for the nod",
                    "holding before touching anything",
                    "hands visible, no sudden moves",
                    "tool is queued, politely",
                    "approval bouncer is at the door",
                    "paused at the do-not-touch line",
                    "waiting for the human checksum",
                    "action is staged, not sprung",
                ],
                ms
            )
        ));
    }

    let active_streaming = app.streams.iter().any(|s| s.session_id == session.id);
    if active_streaming || session.is_streaming() {
        let elapsed_ms = session
            .pending_started_at
            .map(|t| t.elapsed().as_millis() as u64);
        let elapsed = elapsed_ms.map(secs).unwrap_or_else(|| "0.0s".to_string());
        return match session.pending_first_ms() {
            None => {
                let motion = Motion::Moon;
                Some(format!(
                    "{} waiting for assistant · {} · {}",
                    motion.frame(ms),
                    elapsed,
                    flavour(
                        &[
                            "first token is putting on shoes",
                            "model is reading the room",
                            "no words yet; suspiciously quiet",
                            "assistant is choosing an entrance",
                            "thoughts are in the loading dock",
                            "waiting for the first useful syllable",
                            "model is checking its pockets",
                            "silence, but with intent",
                            "first token is fashionably late",
                            "neural gears are not decorative",
                            "response is finding the on-ramp",
                            "the blank stare is temporary",
                            "warming up without the bar chart",
                        ],
                        ms
                    )
                ))
            }
            Some(first) => {
                let motion = Motion::Ripple;
                Some(format!(
                    "{} generating response · {} · first {} · {}",
                    motion.frame(ms),
                    elapsed,
                    secs(first),
                    flavour(
                        &[
                            "sentences are filing in",
                            "paragraphs found the door",
                            "tokens are behaving, mostly",
                            "words are arriving with snacks",
                            "draft is becoming less imaginary",
                            "syntax is putting on a tie",
                            "response is taking shape",
                            "clauses are forming a committee",
                            "text is landing in pieces",
                            "the answer machine is awake",
                            "paragraphs are learning to walk",
                            "tokens are clocking in",
                            "output is no longer theoretical",
                        ],
                        ms
                    )
                ))
            }
        };
    }

    if !app.streams.is_empty() {
        let motion = Motion::Moon;
        return Some(format!(
            "{} background session generating · {}",
            motion.frame(ms),
            flavour(
                &[
                    "another chat is still cooking",
                    "back room has a model in it",
                    "keeping tabs on the other tab",
                    "side quest is still rendering",
                    "background thought has not retired",
                    "other session is muttering productively",
                    "parallel tab is doing homework",
                    "somewhere else, tokens happen",
                    "keeping one ear on the hallway",
                ],
                ms
            )
        ));
    }

    None
}

fn chip_fg(bg: Color) -> Color {
    match bg {
        Color::Black | Color::Blue | Color::DarkGray | Color::Red | Color::Magenta => Color::White,
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

fn cwd_label(app: &App) -> String {
    let cwd = app
        .sessions
        .active()
        .cwd
        .as_ref()
        .cloned()
        .or_else(|| std::env::current_dir().ok());
    let Some(cwd) = cwd else {
        return "cwd —".to_string();
    };
    let display = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .map(|name| format!("…/{}", name))
        .unwrap_or_else(|| cwd.display().to_string());
    format!("cwd {}", display)
}

fn permission_summary(app: &App) -> String {
    let mut parts: Vec<String> = Vec::new();
    // A ⚖ marks that a natural-language access policy is judging tool calls.
    if app.permissions.policy.is_some() {
        parts.push("⚖policy".to_string());
    }
    for kind in &app.permissions.always_allow {
        parts.push(format!("✓{}", kind.name()));
    }
    for kind in &app.permissions.always_deny {
        parts.push(format!("✕{}", kind.name()));
    }
    for rule in &app.permissions.rules {
        let mark = match rule.decision {
            PermissionDecision::Allow => "✓",
            PermissionDecision::Deny => "✕",
        };
        let scope = match &rule.scope {
            PermissionScope::Kind(kind) => kind.name().to_string(),
            PermissionScope::Directory(dir) => dir
                .file_name()
                .and_then(|n| n.to_str())
                .map(|name| format!("…/{}", name))
                .unwrap_or_else(|| dir.display().to_string()),
            PermissionScope::Timed => "all".to_string(),
        };
        let timed = if rule.expires_at.is_some() { "⏱" } else { "" };
        parts.push(format!("{}{}{}", mark, scope, timed));
    }
    if parts.is_empty() {
        "access ask".to_string()
    } else {
        format!("access {}", parts.join(" "))
    }
}

pub fn render_activity(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let ms = now_ms();
    let Some(label) = busy_label(app, ms) else {
        return;
    };

    f.render_widget(
        Paragraph::new(Line::from(vec![chip(label, theme.accent)])),
        area,
    );
}

pub fn render(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let session = app.sessions.active();
    let agent_mode = session.agent_mode;
    let ms = now_ms();

    let model = app.current_model();
    let status = app.status.as_deref().unwrap_or("");

    let mut left: Vec<Span<'static>> = Vec::new();

    use crate::input::vim::VimMode;
    let (mode_label, mode_bg) = match app.vim {
        VimMode::Normal => ("NORMAL", Color::DarkGray),
        VimMode::Insert => ("INSERT", Color::Green),
        VimMode::Visual if app.input.visual_line => ("V-LINE", Color::Magenta),
        VimMode::Visual => ("VISUAL", Color::Magenta),
        VimMode::Operator(_) => ("OP", Color::Cyan),
    };
    left.push(chip(mode_label, mode_bg));
    left.push(Span::raw(" "));

    left.push(Span::styled(cwd_label(app), theme.subtle_pill()));
    left.push(Span::raw(" "));
    left.push(Span::styled(permission_summary(app), theme.subtle_pill()));

    let mut chips: Vec<(String, Color)> = Vec::new();
    if let Some(l) = session.loop_state.as_ref() {
        chips.push((format!("⟳ loop {}/{}", l.iteration, l.max), theme.accent));
    }
    if agent_mode {
        chips.push(("agent".into(), theme.warning));
    }
    if app.show_output {
        chips.push(("output".into(), theme.accent));
    }
    if !app.edited_files.is_empty() {
        chips.push((format!("✎{}", app.edited_files.len()), theme.success));
    }
    let active_skills = app.skills.iter().filter(|s| s.active).count();
    if active_skills > 0 {
        chips.push((format!("✦{}", active_skills), theme.link));
    }
    if let Some(effort) = app.reasoning_effort.as_deref() {
        chips.push((format!("🧠{}", effort), theme.warning));
    }
    for (text, bg) in chips {
        left.push(Span::raw(" "));
        left.push(chip(text, bg));
    }

    if !status.is_empty() {
        left.push(Span::raw("  "));
        left.push(Span::styled(
            status.to_string(),
            Style::default().fg(theme.muted),
        ));
    }

    use crate::app::state::ModelLoad;
    let (right, right_bg) = match app.model_load {
        ModelLoad::Loading => (
            format!(" {} loading models ", Motion::Dots.frame(ms)),
            theme.warning,
        ),
        ModelLoad::Failed => (" ⚠ models unavailable ".to_string(), theme.danger),
        ModelLoad::Loaded => (format!(" {} ", model), theme.accent),
    };
    let left_len: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let right_len = right.chars().count();
    let pad = area.width.saturating_sub((left_len + right_len) as u16) as usize;

    let mut spans = left;
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(
        right,
        Style::default()
            .bg(right_bg)
            .fg(chip_fg(right_bg))
            .add_modifier(Modifier::BOLD),
    ));

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::{frame, secs, DOTS};

    #[test]
    fn secs_formats_tenths() {
        assert_eq!(secs(2500), "2.5s");
    }

    #[test]
    fn frame_cycles_by_time() {
        assert_eq!(frame(&DOTS, 0, 100), DOTS[0]);
        assert_eq!(frame(&DOTS, 100, 100), DOTS[1]);
    }
}
