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
        Self { line, plain, msg, toggle: None, role_start: None }
    }
    fn with_toggle(mut self, key: (usize, usize)) -> Self {
        self.toggle = Some(key);
        self
    }
}

/// A message ready to render: its role and parsed blocks.
pub struct DocMessage {
    pub role: String,
    pub blocks: Vec<Block>,
}

/// Braille spinner frames, driven by wall-clock time so streaming animation
/// works without explicit frame tracking.
const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

fn spinner_for(time_ms: u128) -> &'static str {
    SPINNER[((time_ms / 100) as usize) % SPINNER.len()]
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
    // Reserve columns for the nested gutter bars + a trailing space that
    // `mark_gutter` adds (deepest lineage is tool = 2 bars, so 3 columns).
    let inner = width.saturating_sub(MAX_GUTTER_COLS + 1).max(1);

    for (mi, msg) in messages.iter().enumerate() {
        let start = out.len();
        render_role_header(&msg.role, mi, theme, &mut out);

        for (bi, block) in msg.blocks.iter().enumerate() {
            match block {
                Block::Markdown(text) => render_markdown(text, mi, inner, theme, &mut out),
                Block::Code { lang, code } => render_code(lang, code, mi, inner, theme, &mut out),
                Block::Thinking(text) => {
                    render_thinking(text, mi, bi, inner, theme, toggled, streaming, &mut out)
                }
                Block::ToolCall(call) => render_tool_call(call, mi, inner, theme, &mut out),
                Block::ToolResult { ok, summary, output } => {
                    render_tool_result(*ok, summary, output, mi, bi, inner, theme, toggled, show_output, &mut out)
                }
            }
        }

        // A coloured left gutter bar marks the whole turn so roles read as
        // distinct blocks — using only the terminal's own palette (no custom bg),
        // so it follows the terminal's light/dark theme.
        mark_gutter(&mut out[start..], &role_gutters(&msg.role, theme));

        // A blank, gutter-less line separates turns.
        out.push(RenderedLine::new(Line::raw(""), String::new(), mi));
    }

    out
}

/// Max number of nested gutter bars a turn can carry (tool = assistant + tool).
const MAX_GUTTER_COLS: usize = 2;

/// Nested gutter-bar colours for a message role, outermost first. A tool turn is
/// a child of the assistant, so it carries the assistant bar *and* its own bar
/// nested inside it; user/assistant/system are siblings with a single bar.
fn role_gutters(role: &str, theme: &Theme) -> Vec<Color> {
    match role {
        "user" => vec![theme.gutter_user],
        "system" => vec![theme.gutter_system],
        "tool" => vec![theme.gutter_assistant, theme.gutter_tool],
        _ => vec![theme.gutter_assistant],
    }
}

/// Prefix each row of a turn with its nested coloured gutter bars (outermost
/// first) plus a trailing space, so child turns nest inside their parent's bar.
fn mark_gutter(rows: &mut [RenderedLine], colors: &[Color]) {
    for r in rows {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(r.line.spans.len() + colors.len() + 1);
        for &color in colors {
            spans.push(Span::styled("▎".to_string(), Style::default().fg(color)));
        }
        spans.push(Span::styled(" ".to_string(), Style::default()));
        spans.extend(r.line.spans.iter().cloned());
        r.line = Line::from(spans);
        let bars: String = "▎".repeat(colors.len());
        r.plain = format!("{} {}", bars, r.plain);
    }
}

fn render_role_header(role: &str, mi: usize, theme: &Theme, out: &mut Vec<RenderedLine>) {
    let (label, marker): (&str, &'static str) = match role {
        "user" => ("you", "user"),
        "assistant" => ("assistant", "assistant"),
        "system" => ("system", "system"),
        "tool" => ("tool", "tool"),
        _ => ("?", "assistant"),
    };
    let mut row = RenderedLine::new(
        Line::from(Span::styled(label.to_string(), Style::default().fg(theme.muted))),
        label.to_string(),
        mi,
    );
    row.role_start = Some(marker);
    out.push(row);
}

fn render_markdown(text: &str, mi: usize, width: usize, theme: &Theme, out: &mut Vec<RenderedLine>) {
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
            let lead = if i == 0 { prefix.clone() } else { " ".repeat(prefix.chars().count()) };
            let mut spans: Vec<Span<'static>> = Vec::new();
            if !lead.is_empty() {
                let lead_style = if bullet { Style::default().fg(theme.accent) } else { base_style };
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
        && (t.chars().all(|c| c == '-') || t.chars().all(|c| c == '*') || t.chars().all(|c| c == '_'))
}

/// Returns (prefix, remaining-body, base style, is_bullet) for a markdown line.
fn classify_line(raw: &str, theme: &Theme) -> (String, String, Style, bool) {
    if let Some(rest) = raw.strip_prefix("# ") {
        return ("".into(), rest.to_string(), Style::default().fg(theme.warning).add_modifier(Modifier::BOLD | Modifier::UNDERLINED), false);
    }
    if let Some(rest) = raw.strip_prefix("## ")
        .or_else(|| raw.strip_prefix("### "))
        .or_else(|| raw.strip_prefix("#### "))
        .or_else(|| raw.strip_prefix("##### "))
    {
        return ("".into(), rest.to_string(), Style::default().fg(theme.warning).add_modifier(Modifier::BOLD), false);
    }
    if let Some(rest) = raw.strip_prefix("- ").or_else(|| raw.strip_prefix("* ")).or_else(|| raw.strip_prefix("+ ")) {
        return ("    • ".into(), rest.to_string(), Style::default().fg(theme.text), true);
    }
    // Numbered list: leading "N. " or "N) ".
    if let Some((prefix, rest)) = ordered_list_item(raw) {
        return (prefix, rest, Style::default().fg(theme.text), true);
    }
    if let Some(rest) = raw.strip_prefix("> ") {
        return ("▌ ".into(), rest.to_string(), Style::default().fg(theme.muted), false);
    }
    ("".into(), raw.to_string(), Style::default().fg(theme.text), false)
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
    let body = after.strip_prefix(". ").or_else(|| after.strip_prefix(") "))?;
    let prefix = format!("{}  {}. ", " ".repeat(indent), digits);
    Some((prefix, body.to_string()))
}

/// Solid dark background for code blocks (pure ANSI-256 black, index 16) — darker
/// than most terminal backgrounds, so code reads as a distinct panel without
/// needing a coloured border.
const CODE_BG: Color = Color::Indexed(16);

fn render_code(lang: &str, code: &str, mi: usize, width: usize, theme: &Theme, out: &mut Vec<RenderedLine>) {
    let start = out.len();
    // A coloured left border bar (▌) + a dim lang label, on a dark panel background
    // — the border and the background go together.
    let lang_disp = if lang.is_empty() { "code" } else { lang };
    let border = Style::default().fg(theme.accent).add_modifier(Modifier::BOLD);
    let header = format!("▌ {} ", lang_disp);
    let hspans = vec![
        Span::styled("▌ ".to_string(), border),
        Span::styled(format!("{} ", lang_disp), Style::default().fg(theme.faint).add_modifier(Modifier::BOLD)),
    ];
    out.push(RenderedLine::new(Line::from(hspans), header, mi));
    let avail = width.saturating_sub(2).max(1);
    push_code(code, lang, "▌ ", "▌ ", border, Style::default().fg(theme.text), avail, mi, theme, out);
    // Paint the whole block (header + code) onto the dark panel background.
    paint_bg(&mut out[start..], width, CODE_BG);
}

/// Give every row a solid background and pad it out to `width` columns so the
/// background reads as a continuous panel rather than only colouring the glyphs.
fn paint_bg(rows: &mut [RenderedLine], width: usize, bg: Color) {
    for r in rows {
        let used: usize = r.plain.chars().map(|c| UnicodeWidthChar::width(c).unwrap_or(0)).sum();
        let mut spans: Vec<Span<'static>> = r.line.spans.iter().map(|s| {
            let mut st = s.style;
            st.bg = Some(bg);
            Span::styled(s.content.clone().into_owned(), st)
        }).collect();
        if width > used {
            spans.push(Span::styled(" ".repeat(width - used), Style::default().bg(bg)));
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
                    out.push(RenderedLine::new(Line::from(row_spans), format!("{}{}", lead, plain), mi));
                }
            }
        }
        None => {
            for src in code.split('\n') {
                let chunks = if src.is_empty() { vec![String::new()] } else { hard_chunks(src, width) };
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

/// Guess a highlighting language from a tool-result summary such as
/// `"📖 Read  src/main.rs"` — the first whitespace token with a known grammar.
fn lang_from_summary(summary: &str) -> Option<String> {
    summary
        .split_whitespace()
        .find(|tok| highlight::is_supported(tok))
        .map(|s| s.to_string())
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
    let chip_style = Style::default().bg(Color::Indexed(22)).fg(Color::Green).add_modifier(Modifier::BOLD);
    out.push(
        RenderedLine::new(
            Line::from(Span::styled(header.clone(), chip_style)),
            header,
            mi,
        )
        .with_toggle((mi, bi)),
    );
    if expanded {
        let bg = Color::Indexed(16);
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

fn render_tool_call(call: &crate::agent::ToolCall, mi: usize, width: usize, theme: &Theme, out: &mut Vec<RenderedLine>) {
    let icon = call.kind().map(|k| k.icon()).unwrap_or("⚙");
    let color = match call.kind().map(|k| k.risk()) {
        Some(crate::agent::ToolRisk::Low) => theme.success,
        Some(crate::agent::ToolRisk::Medium) => theme.warning,
        Some(crate::agent::ToolRisk::High) => theme.danger,
        None => theme.tool,
    };
    let head = format!("  {} {}", icon, call.summary());
    out.push(RenderedLine::new(
        Line::from(vec![
            Span::styled("    ▸ ".to_string(), Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{} ", icon), Style::default().fg(color)),
            Span::styled(call.summary(), Style::default().fg(theme.text).add_modifier(Modifier::BOLD)),
        ]),
        head,
        mi,
    ));
    // For edit_file, preview the diff inline (structural editing, agent-style).
    if call.name == "edit_file" {
        let path = call.args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let old = call.args.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
        let new = call.args.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
        render_diff(old, new, path, mi, width, theme, out);
    }
    // For write_file, preview the (syntax-highlighted) content being written.
    if call.name == "write_file" {
        let path = call.args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let content = call.args.get("content").and_then(|v| v.as_str()).unwrap_or("");
        render_write_preview(content, path, mi, width, theme, out);
    }
}

/// How many lines of a `write_file` body to preview inline before eliding.
const WRITE_PREVIEW_LINES: usize = 40;

/// Preview the content of a `write_file` call, syntax-highlighted, capped so a
/// large write doesn't flood the transcript (the full text is on disk anyway).
fn render_write_preview(content: &str, path: &str, mi: usize, width: usize, theme: &Theme, out: &mut Vec<RenderedLine>) {
    let start = out.len();
    let avail = width.saturating_sub(4).max(1);
    let total = content.lines().count();
    let shown: String = content.lines().take(WRITE_PREVIEW_LINES).collect::<Vec<_>>().join("\n");
    let gutter = Style::default().fg(theme.accent);
    push_code(&shown, path, "▌ ", "▌ ", gutter, Style::default().fg(theme.muted), avail, mi, theme, out);
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

/// Render a line-wise diff (old removed, new added) with a coloured `-`/`+`
/// gutter marker and syntax-highlighted code (language inferred from `path`).
fn render_diff(old: &str, new: &str, path: &str, mi: usize, width: usize, theme: &Theme, out: &mut Vec<RenderedLine>) {
    let avail = width.saturating_sub(6).max(1);
    push_code(old, path, "    - ", "      ", Style::default().fg(theme.danger), Style::default().fg(theme.danger), avail, mi, theme, out);
    push_code(new, path, "    + ", "      ", Style::default().fg(theme.success), Style::default().fg(theme.success), avail, mi, theme, out);
}

#[allow(clippy::too_many_arguments)]
fn render_tool_result(
    ok: bool,
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
    let lines: Vec<&str> = output.lines().collect();
    // Short output is always shown; long output collapses unless the global
    // "show output" toggle is on (or this block was individually flipped).
    let default_expanded = lines.len() <= 6;
    let expanded = show_output || (toggled.contains(&(mi, bi)) != default_expanded);
    let collapsible = lines.len() > 6 && !show_output;

    let icon = if ok { "✓" } else { "✗" };
    let color = if ok { theme.success } else { theme.danger };
    let arrow = if collapsible { if expanded { "▾ " } else { "▸ " } } else { "" };
    let header = format!("    {}{} {} ({} lines)", arrow, icon, summary, lines.len());
    let mut row = RenderedLine::new(
        Line::from(Span::styled(header.clone(), Style::default().fg(color).add_modifier(Modifier::BOLD))),
        header,
        mi,
    );
    if collapsible {
        row = row.with_toggle((mi, bi));
    }
    out.push(row);

    if expanded {
        let avail = width.saturating_sub(6).max(1);
        // A successful `read_file` result is file content — syntax-highlight it
        // by the language inferred from the summary's path.
        let read_lang = if ok && summary.contains("Read") { lang_from_summary(summary) } else { None };
        if let Some(lang) = read_lang {
            let gutter = Style::default().fg(theme.accent);
            push_code(output, &lang, "    │ ", "    │ ", gutter, Style::default().fg(theme.muted), avail, mi, theme, out);
        } else {
            for l in &lines {
                for chunk in hard_chunks(l, avail) {
                    let plain = format!("    │ {}", chunk);
                    out.push(RenderedLine::new(
                        Line::from(Span::styled(plain.clone(), Style::default().fg(theme.muted))),
                        plain,
                        mi,
                    ));
                }
            }
        }
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
                Style::default().fg(theme.link).add_modifier(Modifier::UNDERLINED),
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
        vec![DocMessage { role: role.to_string(), blocks }]
    }

    #[test]
    fn markdown_wraps_to_width() {
        let msgs = doc("assistant", vec![Block::Markdown("aaaa bbbb cccc dddd".into())]);
        let rows = build(&msgs, 9, &Theme::default(), &HashSet::new(), false, false);
        // header + wrapped lines + separator + blank
        let texts: Vec<&str> = rows.iter().map(|r| r.plain.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("assistant")));
        for r in &rows {
            assert!(unicode_width::UnicodeWidthStr::width(r.plain.as_str()) <= 9 || r.plain.contains("assistant"));
        }
    }

    #[test]
    fn thinking_collapsed_by_default_hides_body() {
        let msgs = doc("assistant", vec![Block::Thinking("secret\nreasoning".into())]);
        let rows = build(&msgs, 40, &Theme::default(), &HashSet::new(), false, false);
        assert!(rows.iter().any(|r| r.plain.contains("thinking (2 lines)")));
        assert!(!rows.iter().any(|r| r.plain.contains("secret")));
        // The header row is a toggle.
        assert!(rows.iter().any(|r| r.toggle.is_some()));
    }

    #[test]
    fn horizontal_rule_renders_as_line_not_dashes() {
        let rows = build(&doc("assistant", vec![Block::Markdown("a\n---\nb".into())]), 20, &Theme::default(), &HashSet::new(), false, false);
        // The `---` becomes a run of box-drawing chars, not three dashes.
        assert!(rows.iter().any(|r| r.plain.contains("─────")));
        assert!(!rows.iter().any(|r| r.plain.trim() == "---"));
    }

    #[test]
    fn ordered_list_items_get_number_prefix() {
        let rows = build(&doc("assistant", vec![Block::Markdown("1. first\n2. second".into())]), 40, &Theme::default(), &HashSet::new(), false, false);
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
        let short = Block::ToolResult { ok: true, summary: "Read a".into(), output: "l1\nl2".into() };
        let rows = build(&doc("tool", vec![short]), 40, &Theme::default(), &HashSet::new(), false, false);
        assert!(rows.iter().any(|r| r.plain.contains("l1")));

        let long_out = (0..20).map(|i| format!("line{}", i)).collect::<Vec<_>>().join("\n");
        let long = Block::ToolResult { ok: true, summary: "Big".into(), output: long_out };
        let rows = build(&doc("tool", vec![long]), 40, &Theme::default(), &HashSet::new(), false, false);
        assert!(!rows.iter().any(|r| r.plain.contains("line5")));
        assert!(rows.iter().any(|r| r.toggle.is_some()));
    }

    #[test]
    fn show_output_expands_long_tool_result() {
        let long_out = (0..20).map(|i| format!("line{}", i)).collect::<Vec<_>>().join("\n");
        let long = Block::ToolResult { ok: true, summary: "Big".into(), output: long_out };
        // With show_output = true the full output is rendered and not collapsible.
        let rows = build(&doc("tool", vec![long]), 40, &Theme::default(), &HashSet::new(), true, false);
        assert!(rows.iter().any(|r| r.plain.contains("line19")));
        assert!(!rows.iter().any(|r| r.toggle.is_some()));
    }

    /// Whether any rendered row contains a span in the keyword highlight colour.
    fn has_keyword_colour(rows: &[RenderedLine]) -> bool {
        let kw = Theme::default().hl_keyword;
        rows.iter().any(|r| r.line.spans.iter().any(|s| s.style.fg == Some(kw)))
    }

    #[test]
    fn rust_code_block_is_syntax_highlighted() {
        let msgs = doc("assistant", vec![Block::Code { lang: "rust".into(), code: "fn a() {}".into() }]);
        let rows = build(&msgs, 60, &Theme::default(), &HashSet::new(), false, false);
        assert!(rows.iter().any(|r| r.plain.contains("fn a()")));
        assert!(has_keyword_colour(&rows), "the `fn` keyword should be highlighted");
    }

    #[test]
    fn unknown_language_falls_back_to_plain() {
        let msgs = doc("assistant", vec![Block::Code { lang: "nonesuch".into(), code: "fn a() {}".into() }]);
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
        let rows = build(&doc("assistant", vec![Block::ToolCall(call)]), 60, &Theme::default(), &HashSet::new(), false, false);
        assert!(rows.iter().any(|r| r.plain.contains("fn a()")));
        assert!(has_keyword_colour(&rows));
    }

    #[test]
    fn read_result_highlights_by_extension() {
        let block = Block::ToolResult { ok: true, summary: "📖 Read  a.rs".into(), output: "fn a() {}".into() };
        let rows = build(&doc("tool", vec![block]), 60, &Theme::default(), &HashSet::new(), false, false);
        assert!(has_keyword_colour(&rows));
    }

    #[test]
    fn non_read_result_is_not_highlighted() {
        let block = Block::ToolResult { ok: true, summary: "▮ Shell ls".into(), output: "fn a() {}".into() };
        let rows = build(&doc("tool", vec![block]), 60, &Theme::default(), &HashSet::new(), false, false);
        assert!(!has_keyword_colour(&rows));
    }

    #[test]
    fn lang_from_summary_finds_supported_path() {
        assert_eq!(lang_from_summary("📖 Read  src/main.rs").as_deref(), Some("src/main.rs"));
        assert!(lang_from_summary("Search TODO in project").is_none());
    }

    #[test]
    fn edit_file_call_renders_diff() {
        let call = crate::agent::ToolCall {
            name: "edit_file".into(),
            args: serde_json::json!({"path":"a.rs","old_string":"foo","new_string":"bar"}),
            id: None,
        };
        let rows = build(&doc("assistant", vec![Block::ToolCall(call)]), 40, &Theme::default(), &HashSet::new(), false, false);
        assert!(rows.iter().any(|r| r.plain.contains("- foo")));
        assert!(rows.iter().any(|r| r.plain.contains("+ bar")));
    }
}

