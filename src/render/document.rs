//! Builds a flat list of screen rows (`RenderedLine`) from parsed message blocks.
//! Each row is exactly one terminal line (text is pre-wrapped to the viewport
//! width), so the chat view can scroll, place a cursor, and virtualize by simple
//! integer indexing. The result is cached by the chat view and only rebuilt when
//! the content, width, or collapse-state changes.

use std::collections::HashSet;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use crate::domain::blocks::Block;
use crate::render::highlight::{self, Segment};
use crate::render::theme::Theme;
use crate::render::wrap::{hard_chunks, wrap_words};

/// One rendered screen row.
#[derive(Clone)]
pub struct RenderedLine {
    pub line: Line<'static>,
    /// The plain (unstyled) text of this row. Asserted on in tests (wrap width)
    /// and the basis for the planned in-TUI transcript search; not read by the
    /// renderer, which draws `line`.
    #[allow(dead_code)]
    pub plain: String,
    /// Owning message index, for context-aware actions (search/jump). Retained
    /// with `plain`; see above.
    #[allow(dead_code)]
    pub msg: usize,
    /// If this row is a collapsible header, the (msg, block) it toggles.
    pub toggle: Option<(usize, usize)>,
    /// Set on the first row of each message to its role ("user"/"assistant"/…),
    /// so the scrollbar can place a coloured marker per turn.
    pub role_start: Option<&'static str>,
}

impl RenderedLine {
    fn new(line: Line<'static>, plain: String, msg: usize) -> Self {
        Self {
            line,
            plain,
            msg,
            toggle: None,
            role_start: None,
        }
    }
    fn with_toggle(mut self, key: (usize, usize)) -> Self {
        self.toggle = Some(key);
        self
    }
}

/// What a responder is currently doing, used by role-header animations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadingKind {
    /// Request started but no content token has arrived yet.
    Network,
    /// Tokens are streaming from the model.
    Streaming,
    /// A local tool is executing.
    Tool,
}

/// A message ready to render: its role, parsed blocks, and optional timing state.
pub struct DocMessage {
    pub role: String,
    pub blocks: Vec<Block>,
    pub duration_ms: Option<u64>,
    /// Time-to-first-result in ms (once known), shown next to the total time.
    pub first_ms: Option<u64>,
    pub loading: Option<LoadingKind>,
    pub started_at: Option<std::time::Instant>,
}

/// Braille spinner frames, driven by wall-clock time so streaming animation
/// works without explicit frame tracking.
const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

fn spinner_for(time_ms: u128) -> &'static str {
    SPINNER[((time_ms / 100) as usize) % SPINNER.len()]
}

const STREAM_FRAMES: [&str; 4] = ["◐", "◓", "◑", "◒"];
const NET_FRAMES: [&str; 4] = ["∙  ", "∙∙ ", "∙∙∙", " ∙∙"];
const TOOL_FRAMES: [&str; 4] = ["⚙◜", "⚙◝", "⚙◞", "⚙◟"];

fn loading_frame(kind: LoadingKind, time_ms: u128) -> &'static str {
    match kind {
        LoadingKind::Network => NET_FRAMES[((time_ms / 180) as usize) % NET_FRAMES.len()],
        LoadingKind::Streaming => STREAM_FRAMES[((time_ms / 120) as usize) % STREAM_FRAMES.len()],
        LoadingKind::Tool => TOOL_FRAMES[((time_ms / 120) as usize) % TOOL_FRAMES.len()],
    }
}

fn fmt_duration_ms(ms: u64) -> String {
    if ms < 1_000 {
        format!("{}ms", ms.max(1))
    } else {
        let secs = ms as f64 / 1_000.0;
        if secs < 10.0 {
            format!("{:.1}s", secs)
        } else {
            format!("{:.0}s", secs)
        }
    }
}

/// Build the full document. `toggled` holds (msg, block) keys the user has
/// explicitly flipped from their default collapse state.
/// `streaming` controls whether a loader animation is shown on thinking blocks.
pub fn build(
    messages: &[DocMessage],
    width: usize,
    theme: &Theme,
    toggled: &HashSet<(usize, usize)>,
    show_output: bool,
    streaming: bool,
) -> Vec<RenderedLine> {
    let mut out: Vec<RenderedLine> = Vec::new();
    for (mi, msg) in messages.iter().enumerate() {
        out.extend(build_message(
            msg,
            mi,
            width,
            theme,
            toggled,
            show_output,
            streaming,
        ));
    }
    out
}

/// Render a single message (index `mi`) into its screen rows: role header, its
/// blocks, the coloured left gutter bar, and a trailing blank separator. Factored
/// out so `render::chat` can cache each message's rows independently and rebuild
/// only the ones that actually changed (see `ChatState`'s doc cache).
pub fn build_message(
    msg: &DocMessage,
    mi: usize,
    width: usize,
    theme: &Theme,
    toggled: &HashSet<(usize, usize)>,
    show_output: bool,
    streaming: bool,
) -> Vec<RenderedLine> {
    // No left gutter bars: turns run flush to the left edge and use the full width.
    let inner = width.max(1);

    let mut out: Vec<RenderedLine> = Vec::new();
    render_role_header(
        &msg.role,
        mi,
        theme,
        msg.duration_ms,
        msg.first_ms,
        msg.loading,
        msg.started_at,
        &mut out,
    );

    // While streaming, once this turn is producing a tool call, hide the assistant's
    // interstitial prose so only the animated "generating tool" chip + reasoning show
    // — the raw generation around the call is noise until the tool runs.
    let hide_prose = streaming && msg.blocks.iter().any(is_tool_ish);

    for (bi, block) in msg.blocks.iter().enumerate() {
        match block {
            Block::Markdown(_) if hide_prose => {}
            Block::Markdown(text) => render_markdown(text, mi, inner, theme, &mut out),
            // While streaming, a partial ```tool block (JSON not yet complete) is
            // shown as an animated "preparing tool call" chip rather than raw JSON.
            Block::Code { lang, code } if streaming && lang == "tool" => {
                render_preparing_tool(code, mi, inner, theme, &mut out)
            }
            Block::Code { lang, code } => render_code(lang, code, mi, inner, theme, &mut out),
            Block::Thinking(text) => {
                render_thinking(text, mi, bi, inner, theme, toggled, streaming, &mut out)
            }
            Block::ToolCall(call) => {
                render_tool_call(call, mi, bi, inner, theme, toggled, &mut out)
            }
            Block::ToolResult {
                ok,
                name,
                summary,
                output,
            } => render_tool_result(
                *ok,
                name.as_deref(),
                summary,
                output,
                mi,
                bi,
                inner,
                theme,
                toggled,
                show_output,
                &mut out,
            ),
        }
    }

    // A blank line separates turns.
    out.push(RenderedLine::new(Line::raw(""), String::new(), mi));

    out
}

fn render_role_header(
    role: &str,
    mi: usize,
    theme: &Theme,
    duration_ms: Option<u64>,
    first_ms: Option<u64>,
    loading: Option<LoadingKind>,
    started_at: Option<std::time::Instant>,
    out: &mut Vec<RenderedLine>,
) {
    // Each role gets an icon + its own denoting colour (matching the gutter bar),
    // bold — so "you" / "assistant" read as distinct speakers, not muted text.
    let (label, marker, icon, color): (&str, &'static str, &str, Color) = match role {
        "user" => ("you", "user", "❯", theme.gutter_user),
        "assistant" => ("assistant", "assistant", "✦", theme.gutter_assistant),
        "system" => ("system", "system", "◆", theme.gutter_system),
        "tool" => ("tool", "tool", "⚙", theme.gutter_tool),
        _ => ("?", "assistant", "✦", theme.gutter_assistant),
    };
    let mut text = format!("{} {}", icon, label);
    let mut spans = vec![Span::styled(
        text.clone(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )];
    if matches!(role, "assistant" | "tool") {
        // The timing reads as a small badge: the dark `faint` colour as the
        // background with a bright foreground, so it stands out instead of blending
        // into the terminal bg the way dim `faint` foreground text did.
        let time_style = Style::default()
            .bg(theme.faint)
            .fg(theme.text)
            .add_modifier(Modifier::BOLD);
        let badge_text = if let Some(kind) = loading {
            // Live: the total keeps climbing every frame. Before the first byte the
            // total IS the time-to-first-result ("waiting"); after it, show the
            // frozen first-result time alongside the still-growing total.
            let elapsed = started_at
                .map(|t| t.elapsed().as_millis() as u64)
                .unwrap_or(0);
            let anim = loading_frame(kind, elapsed as u128);
            match first_ms {
                None => Some(format!(" {} waiting {} ", anim, fmt_duration_ms(elapsed))),
                Some(f) => Some(format!(
                    " {} {}  ·first {} ",
                    anim,
                    fmt_duration_ms(elapsed),
                    fmt_duration_ms(f)
                )),
            }
        } else {
            // Finalized: total time, plus the time-to-first-result when it differs.
            duration_ms.map(|ms| match first_ms {
                Some(f) if f + 50 < ms => {
                    format!(" {}  ·first {} ", fmt_duration_ms(ms), fmt_duration_ms(f))
                }
                _ => format!(" {} ", fmt_duration_ms(ms)),
            })
        };
        if let Some(badge) = badge_text {
            text.push_str(&format!("  {}", badge));
            spans.push(Span::raw("  "));
            spans.push(Span::styled(badge, time_style));
        }
    }
    let mut row = RenderedLine::new(Line::from(spans), text, mi);
    row.role_start = Some(marker);
    out.push(row);
}

fn render_markdown(
    text: &str,
    mi: usize,
    width: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    for raw in text.split('\n') {
        // Thematic break (`---`, `***`, `___`) → a full-width horizontal rule.
        if is_hr(raw) {
            let rule = "─".repeat(width.max(1));
            out.push(RenderedLine::new(
                Line::from(Span::styled(rule.clone(), Style::default().fg(theme.muted))),
                rule,
                mi,
            ));
            continue;
        }
        // Block-level prefixes handled before wrapping.
        let (prefix, body, base_style, bullet) = classify_line(raw, theme);
        let avail = width.saturating_sub(prefix.chars().count()).max(1);
        let wrapped = wrap_words(&body, avail);
        for (i, wline) in wrapped.iter().enumerate() {
            let lead = if i == 0 {
                prefix.clone()
            } else {
                " ".repeat(prefix.chars().count())
            };
            let mut spans: Vec<Span<'static>> = Vec::new();
            if !lead.is_empty() {
                let lead_style = if bullet {
                    Style::default().fg(theme.accent)
                } else {
                    base_style
                };
                spans.push(Span::styled(lead.clone(), lead_style));
            }
            spans.extend(style_inline(wline, base_style, theme));
            let plain = format!("{}{}", lead, wline);
            out.push(RenderedLine::new(Line::from(spans), plain, mi));
        }
    }
}

/// Whether a line is a Markdown thematic break: 3+ of `-`, `*`, or `_` only
/// (ignoring surrounding spaces).
fn is_hr(raw: &str) -> bool {
    let t: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    t.len() >= 3
        && (t.chars().all(|c| c == '-')
            || t.chars().all(|c| c == '*')
            || t.chars().all(|c| c == '_'))
}

/// Returns (prefix, remaining-body, base style, is_bullet) for a markdown line.
fn classify_line(raw: &str, theme: &Theme) -> (String, String, Style, bool) {
    if let Some(rest) = raw.strip_prefix("# ") {
        return (
            "".into(),
            rest.to_string(),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            false,
        );
    }
    if let Some(rest) = raw
        .strip_prefix("## ")
        .or_else(|| raw.strip_prefix("### "))
        .or_else(|| raw.strip_prefix("#### "))
        .or_else(|| raw.strip_prefix("##### "))
    {
        return (
            "".into(),
            rest.to_string(),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
            false,
        );
    }
    if let Some(rest) = raw
        .strip_prefix("- ")
        .or_else(|| raw.strip_prefix("* "))
        .or_else(|| raw.strip_prefix("+ "))
    {
        return (
            "    • ".into(),
            rest.to_string(),
            Style::default().fg(theme.text),
            true,
        );
    }
    // Numbered list: leading "N. " or "N) ".
    if let Some((prefix, rest)) = ordered_list_item(raw) {
        return (prefix, rest, Style::default().fg(theme.text), true);
    }
    if let Some(rest) = raw.strip_prefix("> ") {
        return (
            "▌ ".into(),
            rest.to_string(),
            Style::default().fg(theme.muted),
            false,
        );
    }
    (
        "".into(),
        raw.to_string(),
        Style::default().fg(theme.text),
        false,
    )
}

/// Detect an ordered-list item (`1. text` / `12) text`); returns its aligned
/// prefix and body.
fn ordered_list_item(raw: &str) -> Option<(String, String)> {
    let trimmed = raw.trim_start();
    let indent = raw.len() - trimmed.len();
    let digits: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 3 {
        return None;
    }
    let after = &trimmed[digits.len()..];
    let body = after
        .strip_prefix(". ")
        .or_else(|| after.strip_prefix(") "))?;
    let prefix = format!("{}  {}. ", " ".repeat(indent), digits);
    Some((prefix, body.to_string()))
}

/// Solid dark background for code blocks (ANSI bright-black / grey, index 8) — a
/// touch darker than most terminal backgrounds, so code reads as a distinct panel
/// without needing a coloured border. Index 16 (pure black) read too dark.
const CODE_BG: Color = Color::Indexed(8);

fn render_code(
    lang: &str,
    code: &str,
    mi: usize,
    width: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    let start = out.len();
    // A coloured left border bar (▌) + a dim lang label, on a dark panel background
    // — the border and the background go together.
    let lang_disp = if lang.is_empty() { "code" } else { lang };
    let border = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let header = format!("▌ {} ", lang_disp);
    let hspans = vec![
        Span::styled("▌ ".to_string(), border),
        Span::styled(
            format!("{} ", lang_disp),
            Style::default()
                .fg(theme.faint)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    out.push(RenderedLine::new(Line::from(hspans), header, mi));
    let avail = width.saturating_sub(2).max(1);
    push_code(
        code,
        lang,
        "▌ ",
        "▌ ",
        border,
        Style::default().fg(theme.text),
        avail,
        mi,
        theme,
        out,
    );
    // Paint the whole block (header + code) onto the dark panel background.
    paint_bg(&mut out[start..], width, CODE_BG);
}

/// Give every row a solid background and pad it out to `width` columns so the
/// background reads as a continuous panel rather than only colouring the glyphs.
fn paint_bg(rows: &mut [RenderedLine], width: usize, bg: Color) {
    for r in rows {
        let used: usize = r
            .plain
            .chars()
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum();
        let mut spans: Vec<Span<'static>> = r
            .line
            .spans
            .iter()
            .map(|s| {
                let mut st = s.style;
                st.bg = Some(bg);
                Span::styled(s.content.clone().into_owned(), st)
            })
            .collect();
        if width > used {
            spans.push(Span::styled(
                " ".repeat(width - used),
                Style::default().bg(bg),
            ));
        }
        r.line = Line::from(spans);
    }
}

/// Emit code rows for `code`, syntax-highlighted with tree-sitter when the
/// language is recognised, falling back to plain hard-wrapped text otherwise.
/// The first visual row of each source line is prefixed with `prefix`; wrapped
/// continuation rows use `cont_prefix`. `width` is the space for code after the
/// prefix. Unhighlighted text (and every fallback row) uses `fallback_style`.
#[allow(clippy::too_many_arguments)]
fn push_code(
    code: &str,
    lang: &str,
    prefix: &str,
    cont_prefix: &str,
    prefix_style: Style,
    fallback_style: Style,
    width: usize,
    mi: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    // Drop a single trailing newline so we don't render a spurious blank row.
    let code = code.strip_suffix('\n').unwrap_or(code);
    match highlight::highlight(code, lang, theme) {
        Some(hl_lines) => {
            for segs in &hl_lines {
                let rows = wrap_segments(segs, width);
                for (ri, (spans, plain)) in rows.into_iter().enumerate() {
                    let lead = if ri == 0 { prefix } else { cont_prefix };
                    let mut row_spans = Vec::with_capacity(spans.len() + 1);
                    row_spans.push(Span::styled(lead.to_string(), prefix_style));
                    row_spans.extend(spans);
                    out.push(RenderedLine::new(
                        Line::from(row_spans),
                        format!("{}{}", lead, plain),
                        mi,
                    ));
                }
            }
        }
        None => {
            for src in code.split('\n') {
                let chunks = if src.is_empty() {
                    vec![String::new()]
                } else {
                    hard_chunks(src, width)
                };
                for (ci, chunk) in chunks.into_iter().enumerate() {
                    let lead = if ci == 0 { prefix } else { cont_prefix };
                    let plain = format!("{}{}", lead, chunk);
                    out.push(RenderedLine::new(
                        Line::from(vec![
                            Span::styled(lead.to_string(), prefix_style),
                            Span::styled(chunk, fallback_style),
                        ]),
                        plain,
                        mi,
                    ));
                }
            }
        }
    }
}

/// Break a line of styled segments into visual rows no wider than `width`,
/// returning `(spans, plain_text)` per row. Splits happen at the display-width
/// boundary; each segment keeps its own style across the split.
fn wrap_segments(segments: &[Segment], width: usize) -> Vec<(Vec<Span<'static>>, String)> {
    let w = width.max(1);
    let mut rows: Vec<(Vec<Span<'static>>, String)> = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut plain = String::new();
    let mut col = 0usize;

    for (text, style) in segments {
        let mut run = String::new();
        for ch in text.chars() {
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if col + cw > w {
                if !run.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut run), *style));
                }
                rows.push((std::mem::take(&mut spans), std::mem::take(&mut plain)));
                col = 0;
            }
            run.push(ch);
            plain.push(ch);
            col += cw;
        }
        if !run.is_empty() {
            spans.push(Span::styled(run, *style));
        }
    }
    rows.push((spans, plain));
    rows
}

fn render_thinking(
    text: &str,
    mi: usize,
    bi: usize,
    width: usize,
    theme: &Theme,
    toggled: &HashSet<(usize, usize)>,
    streaming: bool,
    out: &mut Vec<RenderedLine>,
) {
    let expanded = toggled.contains(&(mi, bi));
    let n = text.lines().count().max(1);
    let spinner = if streaming {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!(" {} ", spinner_for(ms))
    } else {
        String::new()
    };
    let arrow = if expanded { "▾" } else { "▸" };
    let header = format!(" {}{} thinking ({} lines) ", arrow, spinner, n);
    let chip_style = Style::default()
        .bg(Color::Green)
        .fg(Color::Gray)
        .add_modifier(Modifier::BOLD);
    out.push(
        RenderedLine::new(
            Line::from(Span::styled(header.clone(), chip_style)),
            header,
            mi,
        )
        .with_toggle((mi, bi)),
    );
    if expanded {
        let bg = Color::Indexed(8);
        let avail = width.saturating_sub(4).max(1);
        for raw in text.split('\n') {
            for wline in wrap_words(raw, avail) {
                let plain = format!("    {}", wline);
                out.push(RenderedLine::new(
                    Line::from(Span::styled(
                        plain.clone(),
                        Style::default().fg(theme.thinking).bg(bg),
                    )),
                    plain,
                    mi,
                ));
            }
        }
    }
}

/// An animated placeholder shown while the assistant is still emitting a tool
/// call (the JSON isn't closed yet). Hides the raw partial JSON and shows a
/// spinner + the tool name as it resolves, on a dark chip.
fn render_preparing_tool(
    partial: &str,
    mi: usize,
    width: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    let start = out.len();
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let spinner = spinner_for(ms);
    let name = extract_partial_name(partial);
    let label = match &name {
        Some(n) if !n.is_empty() => format!("  {} Preparing  {} …", spinner, n),
        _ => format!("  {} Preparing tool call…", spinner),
    };
    out.push(RenderedLine::new(
        Line::from(Span::styled(
            label.clone(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        label,
        mi,
    ));
    // A dim, truncated peek at what's forming inside, so it's clear it's live.
    let peek: String = partial.split_whitespace().collect::<Vec<_>>().join(" ");
    if !peek.trim().is_empty() {
        let avail = width.saturating_sub(6).max(1);
        let shown: String = peek.chars().take(avail).collect();
        let line = format!("      {}", shown);
        out.push(RenderedLine::new(
            Line::from(Span::styled(line.clone(), Style::default().fg(theme.faint))),
            line,
            mi,
        ));
    }
    paint_bg(&mut out[start..], width, CODE_BG);
}

/// Whether a block is a tool call — complete (`ToolCall`) or a mid-stream partial
/// (`Code` fenced as `tool`). Used to hide interstitial prose while a call forms.
fn is_tool_ish(b: &Block) -> bool {
    matches!(b, Block::ToolCall(_)) || matches!(b, Block::Code { lang, .. } if lang == "tool")
}

/// Best-effort extraction of the `"name"` value from a (possibly partial) tool JSON.
fn extract_partial_name(s: &str) -> Option<String> {
    let i = s.find("\"name\"")?;
    let rest = &s[i + 6..];
    let q1 = rest.find('"')?;
    let after = &rest[q1 + 1..];
    let q2 = after.find('"')?;
    Some(after[..q2].to_string())
}

fn render_tool_call(
    call: &crate::agent::ToolCall,
    mi: usize,
    bi: usize,
    width: usize,
    theme: &Theme,
    toggled: &HashSet<(usize, usize)>,
    out: &mut Vec<RenderedLine>,
) {
    use crate::agent::ToolKind;
    // Single-visibility rule: show EITHER the call procedure OR the result — never
    // both. `edit`/`write` are "call-side" (the change is best shown as it is made,
    // as a diff / preview), so they render here. Every other tool is "result-side":
    // its output confirmation is what matters, so the call renders nothing and the
    // matching `render_tool_result` shows it.
    let kind = call.kind();
    let is_edit = kind == Some(ToolKind::Edit);
    let is_write = kind == Some(ToolKind::Write);
    if !is_edit && !is_write {
        return;
    }

    let icon = kind.map(|k| k.icon()).unwrap_or("⚙");
    let color = theme.warning; // edit/write are Medium risk
    let expanded = is_write && toggled.contains(&(mi, bi));
    let arrow = if is_write {
        if expanded {
            "▾ "
        } else {
            "▸ "
        }
    } else {
        "▸ "
    };
    let head = format!("  {} {}", icon, call.summary());
    let mut row = RenderedLine::new(
        Line::from(vec![
            Span::styled(
                format!("    {}", arrow),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{} ", icon), Style::default().fg(color)),
            Span::styled(
                call.summary(),
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
        ]),
        head,
        mi,
    );
    if is_write {
        row = row.with_toggle((mi, bi));
    }
    out.push(row);

    // For `edit`, preview the diff inline (accept `old`/`new` and legacy `*_string`).
    if is_edit {
        let path = call.args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let old = call
            .args
            .get("old")
            .or_else(|| call.args.get("old_string"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let new = call
            .args
            .get("new")
            .or_else(|| call.args.get("new_string"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        render_diff(old, new, path, mi, width, theme, out);
    }
    // For `write`, preview the (syntax-highlighted) content — only when expanded.
    if is_write {
        let path = call.args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let content = call
            .args
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if expanded {
            render_write_preview(content, path, mi, width, theme, out);
        } else {
            let n = content.lines().count();
            let hint = format!("      … {} line(s) written · click to view", n);
            out.push(RenderedLine::new(
                Line::from(Span::styled(hint.clone(), Style::default().fg(theme.faint))),
                hint,
                mi,
            ));
        }
    }
}

/// How many lines of a `write_file` body to preview inline before eliding.
const WRITE_PREVIEW_LINES: usize = 40;

/// Preview the content of a `write_file` call, syntax-highlighted, capped so a
/// large write doesn't flood the transcript (the full text is on disk anyway).
fn render_write_preview(
    content: &str,
    path: &str,
    mi: usize,
    width: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    let start = out.len();
    let avail = width.saturating_sub(4).max(1);
    let total = content.lines().count();
    let shown: String = content
        .lines()
        .take(WRITE_PREVIEW_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    let gutter = Style::default().fg(theme.accent);
    push_code(
        &shown,
        path,
        "▌ ",
        "▌ ",
        gutter,
        Style::default().fg(theme.muted),
        avail,
        mi,
        theme,
        out,
    );
    if total > WRITE_PREVIEW_LINES {
        let more = format!("▌ … {} more line(s)", total - WRITE_PREVIEW_LINES);
        out.push(RenderedLine::new(
            Line::from(Span::styled(more.clone(), Style::default().fg(theme.faint))),
            more,
            mi,
        ));
    }
    paint_bg(&mut out[start..], width, CODE_BG);
}

/// Render a context diff with line numbers, soft add/remove markers, and syntax
/// highlighting for changed code when the edited path has a known language.
fn render_diff(
    old: &str,
    new: &str,
    path: &str,
    mi: usize,
    width: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    if old == new {
        return;
    }
    let o: Vec<&str> = old.lines().collect();
    let n: Vec<&str> = new.lines().collect();

    let mut p = 0;
    while p < o.len() && p < n.len() && o[p] == n[p] {
        p += 1;
    }
    let mut s = 0;
    while s < o.len().saturating_sub(p)
        && s < n.len().saturating_sub(p)
        && o[o.len() - 1 - s] == n[n.len() - 1 - s]
    {
        s += 1;
    }

    let removed_end = o.len() - s;
    let added_end = n.len() - s;
    let ctx: usize = 3;
    let num_width = n.len().max(o.len()).to_string().len().max(3);
    let gutter_w = num_width + 3;
    let avail = width.saturating_sub(gutter_w + 1).max(1);
    let ctx_style = Style::default().fg(theme.faint);
    let removed_style = Style::default()
        .fg(theme.danger)
        .bg(Color::Indexed(88));
    let added_style = Style::default()
        .fg(theme.success)
        .bg(Color::Indexed(28));

    // Context before change
    let before_start = p.saturating_sub(ctx);
    for i in before_start..p {
        push_diff_line(o[i], i + 1, num_width, " │", ctx_style, avail, mi, out);
    }

    // Removed lines
    for i in p..removed_end {
        push_diff_line(o[i], i + 1, num_width, " -", removed_style, avail, mi, out);
    }

    // Added lines
    for i in p..added_end {
        push_diff_line(n[i], i + 1, num_width, " +", added_style, avail, mi, out);
    }

    // Context after change
    let after_end = (removed_end + ctx).min(o.len());
    for i in removed_end..after_end {
        push_diff_line(o[i], i + 1, num_width, " │", ctx_style, avail, mi, out);
    }
}

/// Push one diff line with line number gutter and hard-wrapping.
fn push_diff_line(
    src: &str,
    line_num: usize,
    num_width: usize,
    marker: &str,
    style: Style,
    avail: usize,
    mi: usize,
    out: &mut Vec<RenderedLine>,
) {
    if src.is_empty() {
        let gutter = format!("{:>width$}{} ", line_num, marker, width = num_width);
        let plain = format!("{} ", gutter);
        out.push(RenderedLine::new(
            Line::from(Span::styled(plain.clone(), style)),
            plain,
            mi,
        ));
        return;
    }
    for (ci, chunk) in hard_chunks(src, avail).into_iter().enumerate() {
        let gutter = if ci == 0 {
            format!("{:>width$}{} ", line_num, marker, width = num_width)
        } else {
            format!("{:>width$}  ", "", width = num_width + 1)
        };
        let plain = format!("{}{}", gutter, chunk);
        out.push(RenderedLine::new(
            Line::from(Span::styled(plain.clone(), style)),
            plain,
            mi,
        ));
    }
}

/// Push a single coloured status line (used for confirmation-only results and
/// suppressed-call errors).
fn push_status(icon: &str, text: &str, color: Color, mi: usize, out: &mut Vec<RenderedLine>) {
    let line = format!("    {} {}", icon, text);
    out.push(RenderedLine::new(
        Line::from(Span::styled(
            line.clone(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )),
        line,
        mi,
    ));
}

/// Extract the parenthesised argument of a function-style summary, e.g.
/// `read(src/main.rs)` → `src/main.rs`.
fn summary_arg(summary: &str) -> Option<&str> {
    let open = summary.find('(')?;
    let close = summary.rfind(')')?;
    (close > open + 1).then(|| summary[open + 1..close].trim())
}

#[allow(clippy::too_many_arguments)]
fn render_tool_result(
    ok: bool,
    name: Option<&str>,
    summary: &str,
    output: &str,
    mi: usize,
    bi: usize,
    width: usize,
    theme: &Theme,
    toggled: &HashSet<(usize, usize)>,
    show_output: bool,
    out: &mut Vec<RenderedLine>,
) {
    use crate::agent::ToolKind;
    let kind = name.and_then(ToolKind::from_name);

    // `todo` lives entirely in the sticky panel above the input — it never appears
    // in the scrollable transcript.
    if kind == Some(ToolKind::Todo) {
        return;
    }

    // Single-visibility: `edit`/`write` already rendered on the call side (diff /
    // preview). Their success result is redundant — show nothing. A failure must
    // still surface, so render the error only.
    if matches!(kind, Some(ToolKind::Edit) | Some(ToolKind::Write)) {
        if ok {
            return;
        }
        push_status(
            "✗",
            &format!(
                "{} failed: {}",
                summary,
                output.lines().next().unwrap_or("")
            ),
            theme.danger,
            mi,
            out,
        );
        return;
    }

    // Confirmation-only tools: a single line, no expandable output dump. The
    // executor's first output line is the confirmation ("Removed …", "Moved …").
    if matches!(
        kind,
        Some(ToolKind::Delete)
            | Some(ToolKind::Move)
            | Some(ToolKind::Copy)
            | Some(ToolKind::Download)
    ) {
        let icon = if ok {
            kind.map(|k| k.icon()).unwrap_or("✓")
        } else {
            "✗"
        };
        let color = if ok { theme.success } else { theme.danger };
        let msg = output
            .lines()
            .next()
            .filter(|l| !l.is_empty())
            .unwrap_or(summary);
        push_status(icon, msg, color, mi, out);
        return;
    }

    // Result-side tools (read/list/search/shell/web_*): the output is the payload.
    let lines: Vec<&str> = output.lines().collect();
    // Short output is always shown; long output collapses unless the global
    // "show output" toggle is on (or this block was individually flipped).
    let default_expanded = lines.len() <= 6;
    let expanded = show_output || (toggled.contains(&(mi, bi)) != default_expanded);
    let collapsible = lines.len() > 6 && !show_output;

    let icon = if ok { "✓" } else { "✗" };
    let color = if ok { theme.success } else { theme.danger };
    let arrow = if collapsible {
        if expanded {
            "▾ "
        } else {
            "▸ "
        }
    } else {
        ""
    };
    let header = format!("    {}{} {} ({} lines)", arrow, icon, summary, lines.len());
    let mut row = RenderedLine::new(
        Line::from(Span::styled(
            header.clone(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )),
        header,
        mi,
    );
    if collapsible {
        row = row.with_toggle((mi, bi));
    }
    out.push(row);

    if expanded {
        let avail = width.saturating_sub(6).max(1);
        // A successful `read` result is file content — syntax-highlight it by the
        // language inferred from the path inside the `read(path)` summary.
        let read_lang = if ok && kind == Some(ToolKind::Read) {
            summary_arg(summary).and_then(|p| highlight::is_supported(p).then(|| p.to_string()))
        } else {
            None
        };
        // Web results carry markdown links — render as markdown so sources are clickable.
        let as_markdown =
            ok && matches!(kind, Some(ToolKind::WebSearch) | Some(ToolKind::WebFetch));
        if let Some(lang) = read_lang {
            let gutter = Style::default().fg(theme.accent);
            push_code(
                output,
                &lang,
                "    │ ",
                "    │ ",
                gutter,
                Style::default().fg(theme.muted),
                avail,
                mi,
                theme,
                out,
            );
        } else if as_markdown {
            render_markdown(output, mi, avail, theme, out);
        } else {
            for l in &lines {
                // Colour diff lines (`git diff`, patches): `+` added green, `-`
                // removed red, `@@` hunk headers accent.
                let color = diff_line_color(l, theme);
                for chunk in hard_chunks(l, avail) {
                    let plain = format!("    │ {}", chunk);
                    out.push(RenderedLine::new(
                        Line::from(Span::styled(plain.clone(), Style::default().fg(color))),
                        plain,
                        mi,
                    ));
                }
            }
        }
    }
}

/// Colour for a tool-output line by its leading diff marker.
fn diff_line_color(line: &str, theme: &Theme) -> Color {
    let t = line.trim_start();
    if t.starts_with("@@") {
        theme.accent
    } else if t.starts_with("+ ") || t == "+" {
        theme.success
    } else if t.starts_with("- ") || t == "-" {
        theme.danger
    } else {
        theme.muted
    }
}

/// Inline styling: `code`, **bold**, and http(s) links. Returns styled spans for
/// a single already-wrapped line.
pub fn style_inline(text: &str, base: Style, theme: &Theme) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut buf = String::new();

    let flush = |buf: &mut String, spans: &mut Vec<Span<'static>>| {
        if !buf.is_empty() {
            spans.push(Span::styled(std::mem::take(buf), base));
        }
    };

    while i < chars.len() {
        // Link
        if chars[i..].starts_with(&['h', 't', 't', 'p']) && is_url_at(&chars, i) {
            flush(&mut buf, &mut spans);
            let mut j = i;
            while j < chars.len() && !chars[j].is_whitespace() {
                j += 1;
            }
            let url: String = chars[i..j].iter().collect();
            spans.push(Span::styled(
                url,
                Style::default()
                    .fg(theme.link)
                    .add_modifier(Modifier::UNDERLINED),
            ));
            i = j;
            continue;
        }
        // Inline code
        if chars[i] == '`' {
            flush(&mut buf, &mut spans);
            if let Some(end) = chars[i + 1..].iter().position(|&c| c == '`') {
                let code: String = chars[i + 1..i + 1 + end].iter().collect();
                spans.push(Span::styled(code, Style::default().fg(theme.success)));
                i = i + 1 + end + 1;
                continue;
            }
        }
        // Bold
        if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '*' {
            if let Some(end) = find_double_star(&chars, i + 2) {
                flush(&mut buf, &mut spans);
                let inner: String = chars[i + 2..end].iter().collect();
                spans.push(Span::styled(inner, base.add_modifier(Modifier::BOLD)));
                i = end + 2;
                continue;
            }
        }
        buf.push(chars[i]);
        i += 1;
    }
    flush(&mut buf, &mut spans);
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    spans
}

fn is_url_at(chars: &[char], i: usize) -> bool {
    let s: String = chars[i..].iter().take(8).collect();
    s.starts_with("http://") || s.starts_with("https://")
}

fn find_double_star(chars: &[char], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < chars.len() {
        if chars[i] == '*' && chars[i + 1] == '*' {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::blocks::Block;

    fn doc(role: &str, blocks: Vec<Block>) -> Vec<DocMessage> {
        vec![DocMessage {
            role: role.to_string(),
            blocks,
            duration_ms: None,
            first_ms: None,
            loading: None,
            started_at: None,
        }]
    }

    #[test]
    fn markdown_wraps_to_width() {
        let msgs = doc(
            "assistant",
            vec![Block::Markdown("aaaa bbbb cccc dddd".into())],
        );
        let rows = build(&msgs, 9, &Theme::default(), &HashSet::new(), false, false);
        // header + wrapped lines + separator + blank
        let texts: Vec<&str> = rows.iter().map(|r| r.plain.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("assistant")));
        for r in &rows {
            assert!(
                unicode_width::UnicodeWidthStr::width(r.plain.as_str()) <= 9
                    || r.plain.contains("assistant")
            );
        }
    }

    #[test]
    fn thinking_collapsed_by_default_hides_body() {
        let msgs = doc(
            "assistant",
            vec![Block::Thinking("secret\nreasoning".into())],
        );
        let rows = build(&msgs, 40, &Theme::default(), &HashSet::new(), false, false);
        assert!(rows.iter().any(|r| r.plain.contains("thinking (2 lines)")));
        assert!(!rows.iter().any(|r| r.plain.contains("secret")));
        // The header row is a toggle.
        assert!(rows.iter().any(|r| r.toggle.is_some()));
    }

    #[test]
    fn horizontal_rule_renders_as_line_not_dashes() {
        let rows = build(
            &doc("assistant", vec![Block::Markdown("a\n---\nb".into())]),
            20,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        // The `---` becomes a run of box-drawing chars, not three dashes.
        assert!(rows.iter().any(|r| r.plain.contains("─────")));
        assert!(!rows.iter().any(|r| r.plain.trim() == "---"));
    }

    #[test]
    fn ordered_list_items_get_number_prefix() {
        let rows = build(
            &doc(
                "assistant",
                vec![Block::Markdown("1. first\n2. second".into())],
            ),
            40,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows.iter().any(|r| r.plain.contains("1. first")));
        assert!(rows.iter().any(|r| r.plain.contains("2. second")));
    }

    #[test]
    fn hr_detects_common_forms() {
        assert!(is_hr("---"));
        assert!(is_hr("***"));
        assert!(is_hr("___"));
        assert!(is_hr("- - -"));
        assert!(!is_hr("--"));
        assert!(!is_hr("text"));
    }

    #[test]
    fn thinking_expands_when_toggled() {
        let msgs = doc("assistant", vec![Block::Thinking("secret".into())]);
        let mut toggled = HashSet::new();
        toggled.insert((0usize, 0usize));
        let rows = build(&msgs, 40, &Theme::default(), &toggled, false, false);
        assert!(rows.iter().any(|r| r.plain.contains("secret")));
    }

    #[test]
    fn short_tool_result_shown_long_collapsed() {
        let short = Block::ToolResult {
            ok: true,
            name: Some("list".into()),
            summary: "list(.)".into(),
            output: "l1\nl2".into(),
        };
        let rows = build(
            &doc("tool", vec![short]),
            40,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows.iter().any(|r| r.plain.contains("l1")));

        let long_out = (0..20)
            .map(|i| format!("line{}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let long = Block::ToolResult {
            ok: true,
            name: Some("list".into()),
            summary: "list(.)".into(),
            output: long_out,
        };
        let rows = build(
            &doc("tool", vec![long]),
            40,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(!rows.iter().any(|r| r.plain.contains("line5")));
        assert!(rows.iter().any(|r| r.toggle.is_some()));
    }

    #[test]
    fn show_output_expands_long_tool_result() {
        let long_out = (0..20)
            .map(|i| format!("line{}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let long = Block::ToolResult {
            ok: true,
            name: Some("list".into()),
            summary: "list(.)".into(),
            output: long_out,
        };
        // With show_output = true the full output is rendered and not collapsible.
        let rows = build(
            &doc("tool", vec![long]),
            40,
            &Theme::default(),
            &HashSet::new(),
            true,
            false,
        );
        assert!(rows.iter().any(|r| r.plain.contains("line19")));
        assert!(!rows.iter().any(|r| r.toggle.is_some()));
    }

    /// Whether any rendered row contains a span in the keyword highlight colour.
    fn has_keyword_colour(rows: &[RenderedLine]) -> bool {
        let kw = Theme::default().hl_keyword;
        rows.iter()
            .any(|r| r.line.spans.iter().any(|s| s.style.fg == Some(kw)))
    }

    #[test]
    fn rust_code_block_is_syntax_highlighted() {
        let msgs = doc(
            "assistant",
            vec![Block::Code {
                lang: "rust".into(),
                code: "fn a() {}".into(),
            }],
        );
        let rows = build(&msgs, 60, &Theme::default(), &HashSet::new(), false, false);
        assert!(rows.iter().any(|r| r.plain.contains("fn a()")));
        assert!(
            has_keyword_colour(&rows),
            "the `fn` keyword should be highlighted"
        );
    }

    #[test]
    fn unknown_language_falls_back_to_plain() {
        let msgs = doc(
            "assistant",
            vec![Block::Code {
                lang: "nonesuch".into(),
                code: "fn a() {}".into(),
            }],
        );
        let rows = build(&msgs, 60, &Theme::default(), &HashSet::new(), false, false);
        assert!(rows.iter().any(|r| r.plain.contains("fn a()")));
        assert!(!has_keyword_colour(&rows));
    }

    #[test]
    fn write_file_call_previews_highlighted_content() {
        let call = crate::agent::ToolCall {
            name: "write_file".into(),
            args: serde_json::json!({"path": "a.rs", "content": "fn a() {}\n"}),
            id: None,
        };
        // Write previews are collapsed by default (click to expand); collapsed
        // shows a hint, not the content.
        let collapsed = build(
            &doc("assistant", vec![Block::ToolCall(call.clone())]),
            60,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(collapsed.iter().any(|r| r.plain.contains("click to view")));
        assert!(
            collapsed.iter().any(|r| r.toggle == Some((0, 0))),
            "header is a toggle"
        );
        assert!(!collapsed.iter().any(|r| r.plain.contains("fn a()")));

        // Expanded (the block toggled open) → the syntax-highlighted content shows.
        let mut toggled = HashSet::new();
        toggled.insert((0usize, 0usize));
        let rows = build(
            &doc("assistant", vec![Block::ToolCall(call)]),
            60,
            &Theme::default(),
            &toggled,
            false,
            false,
        );
        assert!(rows.iter().any(|r| r.plain.contains("fn a()")));
        assert!(has_keyword_colour(&rows));
    }

    #[test]
    fn streaming_partial_tool_shows_preparing_chip() {
        // Mid-stream, an unclosed ```tool block renders as the animated placeholder.
        let block = Block::Code {
            lang: "tool".into(),
            code: "{\"name\":\"read_file\",\"args\":{\"pa".into(),
        };
        let rows = build(
            &doc("assistant", vec![block]),
            60,
            &Theme::default(),
            &HashSet::new(),
            false,
            true,
        );
        assert!(rows.iter().any(|r| r.plain.contains("Preparing")));
        assert!(
            rows.iter().any(|r| r.plain.contains("read_file")),
            "tool name shows as it resolves"
        );

        // Not streaming → a `tool` code block renders normally (no placeholder).
        let block2 = Block::Code {
            lang: "tool".into(),
            code: "half".into(),
        };
        let rows2 = build(
            &doc("assistant", vec![block2]),
            60,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(!rows2.iter().any(|r| r.plain.contains("Preparing")));
    }

    #[test]
    fn streaming_hides_interstitial_prose_around_tool_call() {
        let blocks = vec![
            Block::Markdown("let me edit the file".into()),
            // `edit` is a call-side tool, so it renders during streaming (as a diff).
            Block::ToolCall(crate::agent::ToolCall {
                name: "edit".into(),
                args: serde_json::json!({"path": "a.rs", "old": "x", "new": "y"}),
                id: None,
            }),
        ];
        // Streaming → prose hidden, only the tool call shows.
        let rows = build(
            &doc("assistant", blocks.clone()),
            60,
            &Theme::default(),
            &HashSet::new(),
            false,
            true,
        );
        assert!(!rows.iter().any(|r| r.plain.contains("let me edit")));
        // The tool call itself still renders (its summary shows the path).
        assert!(rows.iter().any(|r| r.plain.contains("a.rs")));
        // Finalized (not streaming) → prose shows normally.
        let rows2 = build(
            &doc("assistant", blocks),
            60,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows2.iter().any(|r| r.plain.contains("let me edit")));
    }

    #[test]
    fn extract_partial_name_reads_name_when_terminated() {
        assert_eq!(
            extract_partial_name("{\"name\":\"read_file\",\"args\":{}}").as_deref(),
            Some("read_file")
        );
        assert_eq!(extract_partial_name("{\"name\":\"read_f"), None); // value not closed yet
        assert!(extract_partial_name("{\"args\":{}}").is_none());
    }

    #[test]
    fn read_result_highlights_by_extension() {
        let block = Block::ToolResult {
            ok: true,
            name: Some("read".into()),
            summary: "read(a.rs)".into(),
            output: "fn a() {}".into(),
        };
        let rows = build(
            &doc("tool", vec![block]),
            60,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(has_keyword_colour(&rows));
    }

    #[test]
    fn non_read_result_is_not_highlighted() {
        let block = Block::ToolResult {
            ok: true,
            name: Some("shell".into()),
            summary: "shell(ls)".into(),
            output: "fn a() {}".into(),
        };
        let rows = build(
            &doc("tool", vec![block]),
            60,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(!has_keyword_colour(&rows));
    }

    #[test]
    fn summary_arg_extracts_parenthesised_path() {
        assert_eq!(summary_arg("read(src/main.rs)"), Some("src/main.rs"));
        assert_eq!(summary_arg("web_search(\"q\")"), Some("\"q\""));
        assert_eq!(summary_arg("no parens here"), None);
    }

    #[test]
    fn todo_result_renders_nothing_in_transcript() {
        // The todo tool lives in the sticky panel; its result must not clutter the log.
        let block = Block::ToolResult {
            ok: true,
            name: Some("todo".into()),
            summary: "todo(3 items)".into(),
            output: "Todo panel updated (3 items)".into(),
        };
        let rows = build(
            &doc("tool", vec![block]),
            60,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        // Only the role header + trailing blank separator; no result content rows.
        assert!(!rows.iter().any(|r| r.plain.contains("Todo panel updated")));
        assert!(!rows.iter().any(|r| r.plain.contains("todo(")));
    }

    #[test]
    fn delete_result_is_single_removed_line() {
        let block = Block::ToolResult {
            ok: true,
            name: Some("delete".into()),
            summary: "delete(old.rs)".into(),
            output: "Removed old.rs".into(),
        };
        let rows = build(
            &doc("tool", vec![block]),
            60,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows.iter().any(|r| r.plain.contains("Removed old.rs")));
        // No expandable dump / toggle for a confirmation-only result.
        assert!(!rows.iter().any(|r| r.toggle.is_some()));
    }

    #[test]
    fn edit_file_call_renders_diff() {
        let call = crate::agent::ToolCall {
            name: "edit_file".into(),
            args: serde_json::json!({"path":"a.rs","old_string":"foo","new_string":"bar"}),
            id: None,
        };
        let rows = build(
            &doc("assistant", vec![Block::ToolCall(call)]),
            40,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows.iter().any(|r| r.plain.contains("- foo")));
        assert!(rows.iter().any(|r| r.plain.contains("+ bar")));
    }
}
