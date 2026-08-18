//! Builds a flat list of screen rows (`RenderedLine`) from parsed message blocks.
//! Each row is exactly one terminal line (text is pre-wrapped to the viewport
//! width), so the chat view can scroll, place a cursor, and virtualize by simple
//! integer indexing. The result is cached by the chat view and only rebuilt when
//! the content, width, or collapse-state changes.

use std::collections::HashSet;

use crate::domain::blocks::Block;
use crate::render::highlight::{self, Segment};
use crate::render::search::{render_search_output, search_pattern_from_summary};
use crate::render::theme::Theme;
use crate::render::wrap::wrap_words;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

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
    /// If set, paint this transcript row edge-to-edge with the given background.
    /// Normal transcript rows, including edit diffs, leave this unset.
    pub background: Option<Color>,
    /// Set on the first row of each message to its role ("user"/"assistant"/…),
    /// so the scrollbar can place a coloured marker per turn.
    pub role_start: Option<&'static str>,
}

impl RenderedLine {
    pub(crate) fn new(line: Line<'static>, plain: String, msg: usize) -> Self {
        Self {
            line,
            plain,
            msg,
            toggle: None,
            background: None,
            role_start: None,
        }
    }
    fn with_toggle(mut self, key: (usize, usize)) -> Self {
        self.toggle = Some(key);
        self
    }
    pub(crate) fn with_background(mut self, background: Option<Color>) -> Self {
        self.background = background;
        self
    }
}

/// A message ready to render: its role, parsed blocks, and optional timing state.
pub struct DocMessage {
    pub role: String,
    pub blocks: Vec<Block>,
    pub duration_ms: Option<u64>,
    /// Wall-clock unix-timestamp seconds when the original ChatMessage was created.
    pub created_at: Option<u64>,
}

pub(crate) fn fmt_duration_ms(ms: u64) -> String {
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
/// `streaming` controls whether partial tool calls are rendered as placeholders.
#[cfg(test)]
pub fn build(
    messages: &[DocMessage],
    width: usize,
    theme: &Theme,
    toggled: &HashSet<(usize, usize)>,
    show_output: bool,
    streaming: bool,
) -> Vec<RenderedLine> {
    let mut out: Vec<RenderedLine> = Vec::new();
    let mut prev_role: Option<&str> = None;
    for (mi, msg) in messages.iter().enumerate() {
        let mut rows = build_message(msg, mi, width, theme, toggled, show_output, streaming);
        // No separator before assistant when it directly follows a tool message.
        if prev_role == Some("tool") && msg.role != "tool" {
            rows.retain(|r| r.role_start.is_none());
        }
        prev_role = Some(msg.role.as_str());
        out.extend(rows);
    }
    out
}

/// Render a single message (index `mi`) into its screen rows: role header, its
/// blocks, and a trailing blank separator. Factored out so `render::chat` can
/// cache each message's rows independently and rebuild only the ones that actually
/// changed (see `ChatState`'s doc cache).
pub fn build_message(
    msg: &DocMessage,
    mi: usize,
    width: usize,
    theme: &Theme,
    toggled: &HashSet<(usize, usize)>,
    show_output: bool,
    _streaming: bool,
) -> Vec<RenderedLine> {
    let indent_w = 2usize;
    let inner = width.saturating_sub(indent_w).max(1);

    let has_visible_block = msg
        .blocks
        .iter()
        .enumerate()
        .any(|(bi, block)| match block {
            Block::Markdown(text) => {
                let next_is_tool = block_followed_by_tool(&msg.blocks, bi);
                visible_prose_before_tool(text, next_is_tool).is_some()
            }
            Block::Thinking(text) => !text.trim().is_empty(),
            Block::Code { lang, code } => lang != "tool" && !code.trim().is_empty(),
            Block::ToolCall(_) => false,
            Block::ToolResult { .. } | Block::ToolFileResult { .. } => true,
        });
    if !has_visible_block {
        return Vec::new();
    }

    let first_visible_is_thinking =
        msg.blocks
            .iter()
            .enumerate()
            .find_map(|(bi, block)| match block {
                Block::Markdown(text)
                    if visible_prose_before_tool(text, block_followed_by_tool(&msg.blocks, bi))
                        .is_some() =>
                {
                    Some(false)
                }
                Block::Thinking(text) if !text.trim().is_empty() => Some(true),
                Block::Code { lang, code } if lang != "tool" && !code.trim().is_empty() => {
                    Some(false)
                }
                Block::ToolResult { .. } | Block::ToolFileResult { .. } => Some(false),
                _ => None,
            })
            == Some(true);

    let mut out: Vec<RenderedLine> = Vec::new();
    if msg.role != "tool" {
        render_message_separator(msg, mi, width, theme, &mut out);
        if !first_visible_is_thinking {
            out.push(RenderedLine::new(Line::raw(""), String::new(), mi));
        }
    }

    // Tool payloads are hidden, but surrounding assistant prose remains part of
    // the transcript. A plan or explanation before a call must not disappear
    // merely because the message also contains a tool block.

    for (bi, block) in msg.blocks.iter().enumerate() {
        match block {
            Block::Markdown(text) => {
                let next_is_tool = block_followed_by_tool(&msg.blocks, bi);
                if let Some(visible) = visible_prose_before_tool(text, next_is_tool) {
                    render_text_segment(visible, mi, inner, theme, &mut out);
                }
            }
            // Tool-call payloads remain in the document for session/API fidelity,
            // but are not user-facing transcript content. The matching result block
            // is the single visual representation of an executed tool.
            Block::Code { lang, .. } if lang == "tool" => {}
            Block::Code { lang, code } => render_code(lang, code, mi, inner, theme, &mut out),
            Block::Thinking(text) => {
                render_thinking(text, (mi, bi), inner, theme, toggled, &mut out)
            }
            Block::ToolCall(_) => {}
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
                None,
                mi,
                bi,
                inner,
                theme,
                toggled,
                show_output,
                &mut out,
            ),
            Block::ToolFileResult {
                ok,
                name,
                summary,
                output,
                call,
            } => render_tool_result(
                *ok,
                name.as_deref(),
                summary,
                output,
                Some(call),
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

    let trailing = RenderedLine::new(Line::raw(""), String::new(), mi);
    for row in &mut out {
        if row.role_start.is_none() {
            row.line.spans.insert(0, Span::raw("  "));
            row.plain = format!("  {}", row.plain);
        }
    }
    wrap_message_panel(&mut out, width, theme);
    out.push(trailing);

    out
}

fn render_message_separator(
    msg: &DocMessage,
    mi: usize,
    width: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    let role = msg.role.as_str();
    let (symbol, marker): (&str, &str) = match role {
        "user" => ("\u{275d}", "user"),
        "assistant" => ("\u{274b}", "assistant"),
        "system" => ("\u{2699}", "system"),
        "tool" => ("\u{26a1}", "tool"),
        _ => ("\u{274b}", "assistant"),
    };
    let color = match role {
        "user" => theme.gutter_user,
        "assistant" => theme.gutter_assistant,
        "system" => theme.gutter_system,
        "tool" => theme.gutter_tool,
        _ => theme.gutter_assistant,
    };
    let duration = msg.duration_ms.map(fmt_duration_ms);
    let time_str = msg.created_at.and_then(fmt_time);
    let label = match (role, duration.as_deref(), time_str) {
        ("user", _, Some(ref t)) => {
            format!(" {} {} {} ", symbol, t, duration.as_deref().unwrap_or(""))
        }
        ("user", _, None) => format!(" {} {} ", symbol, duration.as_deref().unwrap_or("")),
        (_, Some(d), _) => format!(" {} {} ", symbol, d),
        (_, None, _) => format!(" {} ", symbol),
    };
    let plain = if width > label.chars().count() {
        let pad = " ".repeat(width - label.chars().count());
        format!("{}{}", label, pad)
    } else {
        label.clone()
    };
    let mut row = RenderedLine::new(
        Line::from(Span::styled(
            label,
            Style::default().fg(Color::White).bg(color),
        )),
        plain,
        mi,
    );
    row.role_start = Some(marker);
    row.background = Some(color);
    out.push(row);
}

/// Format unix timestamp seconds as relative label + "H:MM am/pm".
///   today    → "2:30 pm"
///   yesterday → "yesterday 2:30 pm"
///   3 days ago → "3 days ago 2:30 pm"
///   last week → "last week 2:30 pm"
///   3 weeks ago → "3 weeks ago 2:30 pm"
///   last month → "last month 2:30 pm"
fn fmt_time(unix_secs: u64) -> Option<String> {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let day_secs = unix_secs % 86400;
    let total_minutes = day_secs / 60;
    let hours_24 = total_minutes / 60;
    let minutes = total_minutes % 60;
    let period = if hours_24 < 12 { "am" } else { "pm" };
    let hours_12 = if hours_24.is_multiple_of(12) {
        12
    } else {
        hours_24 % 12
    };
    let time = format!("{}:{:02} {}", hours_12, minutes, period);
    let msg_time = UNIX_EPOCH.checked_add(Duration::from_secs(unix_secs))?;
    let now = SystemTime::now();
    let diff = now.duration_since(msg_time).ok()?;
    let days = diff.as_secs() / 86400;
    let label = match days {
        0 => String::new(),
        1 => "yesterday ".into(),
        2..=6 => format!("{} days ago ", days),
        7..=13 => "last week ".into(),
        14..=20 => "2 weeks ago ".into(),
        21..=27 => "3 weeks ago ".into(),
        28..=59 => "last month ".into(),
        _ => {
            let months = days / 30;
            if months == 1 {
                "last month ".into()
            } else {
                format!("{} months ago ", months)
            }
        }
    };
    Some(format!("{}{}", label, time))
}

fn wrap_message_panel(rows: &mut [RenderedLine], width: usize, _theme: &Theme) {
    for row in rows {
        let role_start = row.role_start;
        let toggle = row.toggle;
        let background = row.background;
        let content_width = row
            .plain
            .chars()
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum::<usize>();
        let mut spans = Vec::with_capacity(row.line.spans.len() + 1);
        spans.extend(
            row.line
                .spans
                .iter()
                .map(|span| Span::styled(span.content.clone().into_owned(), span.style)),
        );
        if width > content_width {
            let padding = " ".repeat(width - content_width);
            if let Some(background) = background {
                spans.push(Span::styled(padding, Style::default().bg(background)));
            } else {
                spans.push(Span::raw(padding));
            }
        }
        row.line = Line::from(spans);
        row.plain = format!(
            "{}{}",
            row.plain,
            " ".repeat(width.saturating_sub(content_width))
        );
        row.role_start = role_start;
        row.toggle = toggle;
    }
}

fn block_followed_by_tool(blocks: &[Block], index: usize) -> bool {
    blocks.get(index + 1).is_some_and(|next| {
        matches!(next, Block::ToolCall(_))
            || matches!(next, Block::Code { lang, .. } if lang == "tool")
    })
}

fn visible_prose_before_tool(text: &str, next_is_tool: bool) -> Option<&str> {
    if !next_is_tool {
        return (!text.trim().is_empty()).then_some(text);
    }
    let trimmed = text.trim_end();
    let paragraph_start = trimmed.rfind("\n\n").map(|pos| pos + 2).unwrap_or(0);
    let trailing = trimmed[paragraph_start..].trim();
    let invocation = trailing.lines().count() <= 2
        && trailing.split_whitespace().count() <= 14
        && looks_like_tool_invocation(trailing);
    let visible = if invocation {
        trimmed[..paragraph_start].trim_end()
    } else {
        trimmed
    };
    (!visible.trim().is_empty()).then_some(visible)
}

fn looks_like_tool_invocation(text: &str) -> bool {
    let lower = text
        .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '*' | '>'))
        .to_ascii_lowercase();
    [
        "i'll ",
        "i’ll ",
        "i will ",
        "let me ",
        "i'm going to ",
        "i’m going to ",
        "now i'll ",
        "now i’ll ",
        "next i'll ",
        "next i’ll ",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
        && [
            "read",
            "inspect",
            "search",
            "edit",
            "write",
            "run",
            "check",
            "list",
            "fetch",
            "open",
            "update",
            "delete",
            "move",
            "copy",
            "download",
            "use the tool",
        ]
        .iter()
        .any(|verb| lower.contains(verb))
}

fn render_text_segment(
    text: &str,
    mi: usize,
    width: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    let indent = "  ";
    let mut rows = Vec::new();
    let reports = crate::agent::report::verification_reports(text);
    if reports.is_empty() {
        render_markdown(
            text,
            mi,
            width.saturating_sub(indent.len()).max(1),
            theme,
            &mut rows,
        );
    } else {
        let mut cursor = 0;
        for matched in reports {
            let before = &text[cursor..matched.start];
            let (prose, label) = verification_report_label(before);
            if !prose.trim().is_empty() {
                render_markdown(
                    prose.trim_end(),
                    mi,
                    width.saturating_sub(indent.len()).max(1),
                    theme,
                    &mut rows,
                );
            }
            render_verification_report(
                label.as_deref(),
                &matched.report,
                mi,
                width.saturating_sub(indent.len()).max(1),
                theme,
                &mut rows,
            );
            cursor = matched.end;
        }
        if !text[cursor..].trim().is_empty() {
            render_markdown(
                text[cursor..].trim_start(),
                mi,
                width.saturating_sub(indent.len()).max(1),
                theme,
                &mut rows,
            );
        }
    }
    for mut row in rows {
        row.line.spans.insert(0, Span::raw(indent));
        row.plain = format!("{}{}", indent, row.plain);
        out.push(row);
    }
}

fn verification_report_label(text: &str) -> (&str, Option<String>) {
    let trimmed = text.trim_end();
    let line_start = trimmed.rfind('\n').map_or(0, |index| index + 1);
    let candidate = trimmed[line_start..].trim();
    let lower = candidate.to_ascii_lowercase();
    let is_agent_label = lower.starts_with("agent ")
        && (lower.ends_with("(completed):")
            || lower.ends_with("(unresolved):")
            || lower.ends_with("(failed):"));
    if is_agent_label {
        (
            &trimmed[..line_start],
            Some(candidate.trim_end_matches(':').to_string()),
        )
    } else {
        (text, None)
    }
}

fn render_verification_report(
    label: Option<&str>,
    report: &crate::agent::report::VerificationDisplayReport,
    mi: usize,
    width: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    use crate::agent::report::FindingAnswer;

    let (icon, title, color) = match report.status.as_str() {
        "verified" => ("✓", "VERIFIED", theme.success),
        "partially_verified" => ("!", "PARTIALLY VERIFIED", theme.warning),
        "unresolved" => ("?", "UNRESOLVED", theme.warning),
        "failed" => ("×", "FAILED", theme.danger),
        _ => ("?", "UNKNOWN", theme.muted),
    };
    let subject = label.unwrap_or("Verification report");
    let header = format!(" {} {}  {} ", icon, title, subject);
    out.push(RenderedLine::new(
        Line::from(vec![
            Span::styled(
                format!(" {} {} ", icon, title),
                Style::default()
                    .bg(color)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {} ", subject),
                Style::default()
                    .fg(theme.text)
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        header,
        mi,
    ));

    for finding in &report.findings {
        let (glyph, answer, answer_color) = match finding.answer {
            FindingAnswer::Yes => ("✓", "YES", theme.success),
            FindingAnswer::No => ("×", "NO", theme.danger),
            FindingAnswer::Mixed => ("◐", "MIXED", theme.warning),
            FindingAnswer::Unknown => ("?", "UNKNOWN", theme.muted),
        };
        let heading = format!("{} {}  {}", glyph, answer, finding.check_id);
        out.push(RenderedLine::new(
            Line::from(vec![
                Span::styled(
                    format!(" {} {} ", glyph, answer),
                    Style::default()
                        .fg(Color::Black)
                        .bg(answer_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {}", finding.check_id),
                    Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}", finding.support),
                    Style::default().fg(theme.muted),
                ),
            ]),
            heading,
            mi,
        ));
        for line in wrap_words(&finding.statement, width.saturating_sub(4).max(1)) {
            out.push(RenderedLine::new(
                Line::from(vec![
                    Span::styled("  │ ", Style::default().fg(answer_color)),
                    Span::styled(line.clone(), Style::default().fg(theme.text)),
                ]),
                format!("  │ {}", line),
                mi,
            ));
        }
        if !finding.evidence.is_empty() {
            let evidence = format!("Evidence: {}", finding.evidence.join(" · "));
            for line in wrap_words(&evidence, width.saturating_sub(4).max(1)) {
                out.push(RenderedLine::new(
                    Line::from(vec![
                        Span::styled("  └ ", Style::default().fg(answer_color)),
                        Span::styled(line.clone(), Style::default().fg(theme.muted)),
                    ]),
                    format!("  └ {}", line),
                    mi,
                ));
            }
        }
    }

    if !report.unresolved.is_empty() {
        let text = format!("Unresolved checks: {}", report.unresolved.join(", "));
        out.push(RenderedLine::new(
            Line::from(vec![
                Span::styled(
                    " ? UNRESOLVED ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(theme.warning)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {}", report.unresolved.join(", ")),
                    Style::default().fg(theme.warning),
                ),
            ]),
            text,
            mi,
        ));
    }
    if !report.diagnostics.is_empty() {
        let text = format!("{} diagnostic note(s)", report.diagnostics.len());
        out.push(RenderedLine::new(
            Line::from(Span::styled(
                format!("  ⚑ {}", text),
                Style::default().fg(theme.muted),
            )),
            text,
            mi,
        ));
    }
}

fn render_markdown(
    text: &str,
    mi: usize,
    width: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut index = 0;
    while index < lines.len() {
        if let Some((table, consumed)) = parse_markdown_table(&lines[index..]) {
            render_markdown_table(&table, mi, width, theme, out);
            index += consumed;
            continue;
        }

        let raw = lines[index];
        // Thematic break (`---`, `***`, `___`) → a full-width text rule.
        if is_hr(raw) {
            let rule = "─".repeat(width.max(1));
            out.push(RenderedLine::new(
                Line::from(Span::styled(rule.clone(), Style::default().fg(theme.muted))),
                rule,
                mi,
            ));
            index += 1;
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
        index += 1;
    }
}

#[derive(Clone, Copy)]
enum TableAlignment {
    Left,
    Center,
    Right,
}

struct MarkdownTable {
    rows: Vec<Vec<String>>,
    alignments: Vec<TableAlignment>,
}

fn split_table_row(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return None;
    }
    let inner = trimmed
        .strip_prefix('|')
        .unwrap_or(trimmed)
        .strip_suffix('|')
        .unwrap_or_else(|| trimmed.strip_prefix('|').unwrap_or(trimmed));
    let cells: Vec<String> = inner
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect();
    (cells.len() >= 2).then_some(cells)
}

fn parse_table_separator(line: &str, columns: usize) -> Option<Vec<TableAlignment>> {
    let cells = split_table_row(line)?;
    if cells.len() != columns {
        return None;
    }
    cells
        .into_iter()
        .map(|cell| {
            let marker = cell.trim();
            let left = marker.starts_with(':');
            let right = marker.ends_with(':');
            let dashes = marker.trim_matches(':');
            if dashes.len() < 3 || !dashes.chars().all(|c| c == '-') {
                return None;
            }
            Some(match (left, right) {
                (true, true) => TableAlignment::Center,
                (false, true) => TableAlignment::Right,
                _ => TableAlignment::Left,
            })
        })
        .collect()
}

fn parse_markdown_table(lines: &[&str]) -> Option<(MarkdownTable, usize)> {
    if lines.len() < 2 {
        return None;
    }
    let header = split_table_row(lines[0])?;
    let alignments = parse_table_separator(lines[1], header.len())?;
    let columns = header.len();
    let mut rows = vec![header];
    let mut consumed = 2;
    while let Some(line) = lines.get(consumed) {
        let Some(mut row) = split_table_row(line) else {
            break;
        };
        if row.len() > columns {
            break;
        }
        row.resize(columns, String::new());
        rows.push(row);
        consumed += 1;
    }
    Some((MarkdownTable { rows, alignments }, consumed))
}

fn render_markdown_table(
    table: &MarkdownTable,
    mi: usize,
    width: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    let columns = table.alignments.len();
    if columns == 0 {
        return;
    }
    let mut widths = vec![1usize; columns];
    for row in &table.rows {
        for (column, cell) in row.iter().enumerate() {
            widths[column] = widths[column].max(display_width(cell));
        }
    }
    let chrome = columns * 3 + 1;
    let content_budget = width.saturating_sub(chrome).max(columns);
    while widths.iter().sum::<usize>() > content_budget {
        let Some((widest, _)) = widths
            .iter()
            .enumerate()
            .filter(|(_, w)| **w > 1)
            .max_by_key(|(_, w)| **w)
        else {
            break;
        };
        widths[widest] -= 1;
    }

    let border_style = Style::default().fg(theme.muted);
    let push_border = |left: char, join: char, right: char, out: &mut Vec<RenderedLine>| {
        let mut plain = String::new();
        plain.push(left);
        for (i, cell_width) in widths.iter().enumerate() {
            plain.push_str(&"─".repeat(cell_width + 2));
            plain.push(if i + 1 == widths.len() { right } else { join });
        }
        out.push(RenderedLine::new(
            Line::from(Span::styled(plain.clone(), border_style)),
            plain,
            mi,
        ));
    };

    push_border('┌', '┬', '┐', out);
    for (row_index, row) in table.rows.iter().enumerate() {
        let wrapped: Vec<Vec<String>> = row
            .iter()
            .enumerate()
            .map(|(column, cell)| wrap_words(cell, widths[column]))
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for visual_row in 0..height {
            let mut spans = vec![Span::styled("│".to_string(), border_style)];
            let mut plain = "│".to_string();
            for column in 0..columns {
                let text = wrapped[column]
                    .get(visual_row)
                    .map(String::as_str)
                    .unwrap_or("");
                let text_width = display_width(text);
                let remaining = widths[column].saturating_sub(text_width);
                let (left_pad, right_pad) = match table.alignments[column] {
                    TableAlignment::Left => (0, remaining),
                    TableAlignment::Center => (remaining / 2, remaining - remaining / 2),
                    TableAlignment::Right => (remaining, 0),
                };
                let left = format!(" {}", " ".repeat(left_pad));
                let right = format!("{} ", " ".repeat(right_pad));
                let base = if row_index == 0 {
                    Style::default().fg(theme.text).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text)
                };
                spans.push(Span::styled(left.clone(), base));
                spans.extend(style_inline(text, base, theme));
                spans.push(Span::styled(right.clone(), base));
                spans.push(Span::styled("│".to_string(), border_style));
                plain.push_str(&left);
                plain.push_str(text);
                plain.push_str(&right);
                plain.push('│');
            }
            out.push(RenderedLine::new(Line::from(spans), plain, mi));
        }
        if row_index == 0 {
            push_border('├', '┼', '┤', out);
        }
    }
    push_border('└', '┴', '┘', out);
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
            "  ".into(),
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

fn render_code(
    lang: &str,
    code: &str,
    mi: usize,
    width: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    let lang_disp = if lang.is_empty() { "code" } else { lang };
    let header = format!(" {} ", lang_disp);
    let hspans = vec![Span::styled(
        header.clone(),
        Style::default()
            .bg(Color::DarkGray)
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    )];
    out.push(RenderedLine::new(Line::from(hspans), header, mi));
    push_code(
        code,
        lang,
        "",
        "",
        Style::default(),
        Style::default().fg(theme.text),
        width.max(1),
        mi,
        theme,
        out,
    );
    // Keep code rows on the transcript's terminal background.
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
                let segments = vec![(src.to_string(), fallback_style)];
                for (ci, (spans, chunk)) in wrap_segments(&segments, width).into_iter().enumerate()
                {
                    let lead = if ci == 0 { prefix } else { cont_prefix };
                    let plain = format!("{}{}", lead, chunk);
                    let mut row_spans = Vec::with_capacity(spans.len() + 1);
                    row_spans.push(Span::styled(lead.to_string(), prefix_style));
                    row_spans.extend(spans);
                    out.push(RenderedLine::new(Line::from(row_spans), plain, mi));
                }
            }
        }
    }
}

/// Break a line of styled segments into visual rows no wider than `width`.
/// Wrapping prefers whitespace boundaries, drops separator whitespace at a wrap,
/// and only hard-breaks words that cannot fit on a row. Segment styles survive
/// unchanged, including Tree-sitter foregrounds and opaque backgrounds.
pub(crate) fn wrap_segments(
    segments: &[Segment],
    width: usize,
) -> Vec<(Vec<Span<'static>>, String)> {
    #[derive(Clone, Copy)]
    struct StyledChar {
        ch: char,
        style: Style,
    }

    fn display_width(chars: &[StyledChar]) -> usize {
        chars
            .iter()
            .map(|styled| UnicodeWidthChar::width(styled.ch).unwrap_or(0))
            .sum()
    }

    fn push_row(chars: &mut Vec<StyledChar>, rows: &mut Vec<(Vec<Span<'static>>, String)>) {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut plain = String::new();
        let mut run = String::new();
        let mut run_style = None;

        for styled in chars.drain(..) {
            plain.push(styled.ch);
            if run_style == Some(styled.style) {
                run.push(styled.ch);
            } else {
                if let Some(style) = run_style.replace(styled.style) {
                    spans.push(Span::styled(std::mem::take(&mut run), style));
                }
                run.push(styled.ch);
            }
        }
        if let Some(style) = run_style {
            spans.push(Span::styled(run, style));
        }
        rows.push((spans, plain));
    }

    fn append_hard_wrapped(
        chars: &[StyledChar],
        width: usize,
        row: &mut Vec<StyledChar>,
        col: &mut usize,
        rows: &mut Vec<(Vec<Span<'static>>, String)>,
    ) {
        for styled in chars {
            let cw = UnicodeWidthChar::width(styled.ch).unwrap_or(0);
            if *col + cw > width && !row.is_empty() {
                push_row(row, rows);
                *col = 0;
            }
            row.push(*styled);
            *col += cw;
        }
    }

    let width = width.max(1);
    let chars: Vec<StyledChar> = segments
        .iter()
        .flat_map(|(text, style)| text.chars().map(|ch| StyledChar { ch, style: *style }))
        .collect();
    if chars.is_empty() {
        return vec![(Vec::new(), String::new())];
    }

    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut col = 0usize;
    let mut pending_space = Vec::new();
    let mut index = 0usize;

    while index < chars.len() {
        let whitespace = chars[index].ch.is_whitespace();
        let start = index;
        while index < chars.len() && chars[index].ch.is_whitespace() == whitespace {
            index += 1;
        }
        let token = &chars[start..index];

        if whitespace {
            pending_space.extend_from_slice(token);
            continue;
        }

        let word_width = display_width(token);
        if row.is_empty() {
            append_hard_wrapped(&pending_space, width, &mut row, &mut col, &mut rows);
            pending_space.clear();
        } else if col + display_width(&pending_space) + word_width > width {
            push_row(&mut row, &mut rows);
            col = 0;
            pending_space.clear();
        } else {
            append_hard_wrapped(&pending_space, width, &mut row, &mut col, &mut rows);
            pending_space.clear();
        }

        append_hard_wrapped(token, width, &mut row, &mut col, &mut rows);
    }

    if row.is_empty() || col + display_width(&pending_space) <= width {
        append_hard_wrapped(&pending_space, width, &mut row, &mut col, &mut rows);
    }
    if !row.is_empty() {
        push_row(&mut row, &mut rows);
    }
    if rows.is_empty() {
        rows.push((Vec::new(), String::new()));
    }
    rows
}

fn render_thinking(
    text: &str,
    key: (usize, usize),
    width: usize,
    theme: &Theme,
    toggled: &HashSet<(usize, usize)>,
    out: &mut Vec<RenderedLine>,
) {
    let (mi, bi) = key;
    let expanded = toggled.contains(&(mi, bi));
    let n = text.lines().count().max(1);
    let arrow = if expanded { "▾" } else { "▸" };
    let header = format!(" {} thinking ({} lines) ", arrow, n);
    let chip_style = Style::default()
        .fg(Color::Green)
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
        let avail = width.saturating_sub(4).max(1);
        for raw in text.split('\n') {
            for wline in wrap_words(raw, avail) {
                let plain = format!("    {}", wline);
                out.push(RenderedLine::new(
                    Line::from(Span::styled(
                        plain.clone(),
                        Style::default().fg(theme.thinking),
                    )),
                    plain,
                    mi,
                ));
            }
        }
    }
}

fn tool_chip_header(
    _label: &str,
    arrow: &str,
    icon: &str,
    summary: &str,
    meta: Option<&str>,
    ok: bool,
    theme: &Theme,
) -> Line<'static> {
    let status = if ok { theme.accent } else { theme.danger };
    let mut spans = vec![
        Span::styled(
            format!(" {}{} ", arrow, icon),
            Style::default()
                .bg(status)
                .fg(crate::render::theme::fg_guard(Color::Black))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} ", summary),
            Style::default()
                .bg(Color::DarkGray)
                .fg(theme.text)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(meta) = meta {
        spans.push(Span::styled(
            format!(" {} ", meta),
            Style::default()
                .bg(Color::Black)
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

#[allow(dead_code)]
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

    let icon = kind.map(|k| k.icon()).unwrap_or("tool");
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
    let summary = crate::render::path::abbreviate_home(&call.summary());
    let head = format!("  {} {}", icon, summary);
    let mut row = RenderedLine::new(
        tool_chip_header(
            kind.map(|k| k.name()).unwrap_or("tool"),
            arrow,
            icon,
            &summary,
            None,
            true,
            theme,
        ),
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
                Line::from(Span::styled(hint.clone(), Style::default().fg(theme.muted))),
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
    let avail = width.max(1);
    let total = content.lines().count();
    let shown: String = content
        .lines()
        .take(WRITE_PREVIEW_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    push_code(
        &shown,
        path,
        "",
        "",
        Style::default(),
        Style::default().fg(theme.muted),
        avail,
        mi,
        theme,
        out,
    );
    if total > WRITE_PREVIEW_LINES {
        let more = format!("… {} more line(s)", total - WRITE_PREVIEW_LINES);
        out.push(RenderedLine::new(
            Line::from(Span::styled(more.clone(), Style::default().fg(theme.muted))),
            more,
            mi,
        ));
    }
}

struct SourceCard<'a> {
    label: &'a str,
    rail_color: Color,
    path: &'a str,
    code: &'a str,
    start_line: usize,
}

fn render_source_card(
    card: SourceCard<'_>,
    mi: usize,
    width: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    let SourceCard {
        label,
        rail_color,
        path,
        code,
        start_line,
    } = card;
    let header_plain = format!("█ {}", label);
    out.push(RenderedLine::new(
        Line::from(vec![
            Span::styled("█ ", Style::default().fg(rail_color)),
            Span::styled(
                label.to_string(),
                Style::default().fg(rail_color).add_modifier(Modifier::BOLD),
            ),
        ]),
        header_plain,
        mi,
    ));

    let code = code.strip_suffix('\n').unwrap_or(code);
    let source_lines: Vec<&str> = code.split('\n').collect();
    let last_line = start_line.saturating_add(source_lines.len().saturating_sub(1));
    let number_width = last_line.max(1).to_string().len();
    let gutter_width = number_width + 5;
    let code_width = width.saturating_sub(gutter_width).max(1);
    let highlighted = highlight::highlight(code, path, theme);

    for (index, source) in source_lines.iter().enumerate() {
        let fallback = vec![(source.to_string(), Style::default().fg(theme.text))];
        let segments = highlighted
            .as_ref()
            .and_then(|lines| lines.get(index))
            .filter(|segments| !segments.is_empty())
            .unwrap_or(&fallback);
        for (row_index, (spans, chunk)) in
            wrap_segments(segments, code_width).into_iter().enumerate()
        {
            let number = if row_index == 0 {
                format!("{:>width$}", start_line + index, width = number_width)
            } else {
                " ".repeat(number_width)
            };
            let gutter = format!("█ {} │ ", number);
            let mut row_spans = vec![
                Span::styled("█ ", Style::default().fg(rail_color)),
                Span::styled(number, Style::default().fg(theme.muted)),
                Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
            ];
            row_spans.extend(spans);
            out.push(RenderedLine::new(
                Line::from(row_spans),
                format!("{}{}", gutter, chunk),
                mi,
            ));
        }
    }
}

fn read_file_body(output: &str) -> (usize, String, Vec<String>) {
    let mut lines: Vec<&str> = output.lines().collect();
    let mut start_line = 1usize;
    let mut notes = Vec::new();

    if let Some(first) = lines.first().copied() {
        if let Some(range) = first
            .strip_prefix("[lines ")
            .and_then(|line| line.split_once(" of ").map(|(range, _)| range))
            .and_then(|range| range.split_once('-'))
        {
            start_line = range.0.parse().unwrap_or(1);
            lines.remove(0);
        }
    }
    while lines
        .first()
        .is_some_and(|line| line.starts_with("[limit capped"))
    {
        notes.push(lines.remove(0).to_string());
    }
    if lines
        .last()
        .is_some_and(|line| line.starts_with("[next: read("))
    {
        if let Some(note) = lines.pop() {
            notes.push(note.to_string());
        }
    }
    (start_line, lines.join("\n"), notes)
}

const TOOL_BODY_INDENT: usize = 2;

fn indent_tool_body(rows: &mut [RenderedLine], indent: usize) {
    if indent == 0 {
        return;
    }
    let pad = " ".repeat(indent);
    for row in rows {
        row.line.spans.insert(0, Span::raw(pad.clone()));
        row.plain = format!("{}{}", pad, row.plain);
    }
}

fn display_width(text: &str) -> usize {
    text.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

/// Below this width the old/new comparison falls back to stacked cards.
const SIDE_BY_SIDE_MIN_WIDTH: usize = 110;

struct EditComparison<'a> {
    path: &'a str,
    old: &'a str,
    new: &'a str,
    start_line: usize,
}

fn render_edit_comparison(
    comparison: EditComparison<'_>,
    mi: usize,
    width: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    let EditComparison {
        path,
        old,
        new,
        start_line,
    } = comparison;
    if width >= SIDE_BY_SIDE_MIN_WIDTH {
        let o: Vec<&str> = old.lines().collect();
        let n: Vec<&str> = new.lines().collect();
        let (p, removed_end, added_end) = diff_alignment(&o, &n);
        render_diff_columns(
            &o,
            &n,
            p,
            removed_end,
            added_end,
            path,
            start_line,
            mi,
            width,
            theme,
            out,
        );
        return;
    }
    render_source_card(
        SourceCard {
            label: "OLD",
            rail_color: theme.danger,
            path,
            code: old,
            start_line,
        },
        mi,
        width,
        theme,
        out,
    );
    out.push(RenderedLine::new(Line::default(), String::new(), mi));
    render_source_card(
        SourceCard {
            label: "NEW",
            rail_color: theme.success,
            path,
            code: new,
            start_line,
        },
        mi,
        width,
        theme,
        out,
    );
}

/// Common prefix `p` and the change ranges `removed_end`/`added_end` that line
/// up the two sides of a comparison: identical leading lines, then the removed
/// block, then the added block, then identical trailing lines.
fn diff_alignment(o: &[&str], n: &[&str]) -> (usize, usize, usize) {
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
    (p, o.len() - s, n.len() - s)
}

type DiffCell<'a> = Option<(usize, Option<Color>, &'a str, Option<&'a [Segment]>, Style)>;
type DiffPlanRow<'a> = (DiffCell<'a>, DiffCell<'a>);

/// Side-by-side old | new diff. Every line has a block and at least one
/// separating space before its line number: red = removed, green = added,
/// dark gray = unchanged.
#[allow(clippy::too_many_arguments)]
fn render_diff_columns(
    o: &[&str],
    n: &[&str],
    p: usize,
    removed_end: usize,
    added_end: usize,
    path: &str,
    start_line: usize,
    mi: usize,
    width: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    let last_line = start_line.saturating_add(n.len().max(o.len()).saturating_sub(1));
    let num_width = last_line.max(1).to_string().len().max(3);
    let gutter_w = num_width + 5;
    let col_width = width.saturating_sub(3) / 2;
    let code_w = col_width.saturating_sub(gutter_w).max(1);
    let divider = Span::styled(" │ ", Style::default().fg(Color::DarkGray));
    let divider_plain = " │ ";
    let old_hl = highlight::highlight(&o.join("\n"), path, theme);
    let new_hl = highlight::highlight(&n.join("\n"), path, theme);
    let old_seg = |i: usize| old_hl.as_ref().and_then(|l| l.get(i).map(Vec::as_slice));
    let new_seg = |i: usize| new_hl.as_ref().and_then(|l| l.get(i).map(Vec::as_slice));

    let ctx = Style::default().fg(theme.muted);
    let mut plan: Vec<DiffPlanRow<'_>> = Vec::new();
    for i in 0..p {
        plan.push((
            Some((start_line + i, None, o[i], old_seg(i), ctx)),
            Some((start_line + i, None, n[i], new_seg(i), ctx)),
        ));
    }
    let removed_count = removed_end.saturating_sub(p);
    let added_count = added_end.saturating_sub(p);
    for j in 0..removed_count.max(added_count) {
        let left = (j < removed_count).then(|| {
            let i = p + j;
            (
                start_line + i,
                Some(theme.danger),
                o[i],
                old_seg(i),
                Style::default().bg(Color::DarkGray),
            )
        });
        let right = (j < added_count).then(|| {
            let i = p + j;
            (
                start_line + i,
                Some(theme.success),
                n[i],
                new_seg(i),
                Style::default().bg(Color::DarkGray),
            )
        });
        plan.push((left, right));
    }
    for (i, source) in o.iter().enumerate().skip(removed_end) {
        let new_index = added_end + (i - removed_end);
        plan.push((
            Some((start_line + i, None, source, old_seg(i), ctx)),
            Some((
                start_line + new_index,
                None,
                n[new_index],
                new_seg(new_index),
                ctx,
            )),
        ));
    }

    for (left, right) in plan {
        let left_bar = left.as_ref().and_then(|(_, bar, _, _, _)| *bar);
        let right_bar = right.as_ref().and_then(|(_, bar, _, _, _)| *bar);
        let left_rows = diff_column_rows(left, num_width, code_w);
        let right_rows = diff_column_rows(right, num_width, code_w);
        let rows = left_rows.len().max(right_rows.len()).max(1);
        for row in 0..rows {
            let mut spans = Vec::new();
            let mut plain = String::new();
            match left_rows.get(row) {
                Some((s, p)) => {
                    spans.extend(s.iter().cloned());
                    plain.push_str(p);
                }
                None => {
                    let (s, p) = diff_column_continuation(left_bar, num_width);
                    spans.extend(s);
                    plain.push_str(&p);
                }
            }
            let left_width = display_width(&plain);
            if left_width < col_width {
                let padding = " ".repeat(col_width - left_width);
                spans.push(Span::raw(padding.clone()));
                plain.push_str(&padding);
            }
            spans.push(divider.clone());
            plain.push_str(divider_plain);
            let right_start = display_width(&plain);
            match right_rows.get(row) {
                Some((s, p)) => {
                    spans.extend(s.iter().cloned());
                    plain.push_str(p);
                }
                None => {
                    let (s, p) = diff_column_continuation(right_bar, num_width);
                    spans.extend(s);
                    plain.push_str(&p);
                }
            }
            let right_width = display_width(&plain).saturating_sub(right_start);
            if right_width < col_width {
                let padding = " ".repeat(col_width - right_width);
                spans.push(Span::raw(padding.clone()));
                plain.push_str(&padding);
            }
            out.push(RenderedLine::new(Line::from(spans), plain, mi));
        }
    }
}

/// One column of a side-by-side diff row: optional bar, line number, content.
fn diff_column_rows(
    cell: DiffCell<'_>,
    num_width: usize,
    code_w: usize,
) -> Vec<(Vec<Span<'static>>, String)> {
    let Some((num, bar, source, segments, style)) = cell else {
        return Vec::new();
    };
    let bar_text = "█ ";
    let bar_style = Style::default().fg(bar.unwrap_or(Color::DarkGray));
    let num_text = format!("{:>width$}", num, width = num_width);
    let gutter_spans = vec![
        Span::styled(bar_text.to_string(), bar_style),
        Span::styled(
            num_text.clone(),
            Style::default().fg(Color::White).bg(Color::Reset),
        ),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
    ];
    let gutter_plain = format!("{}{} │ ", bar_text, num_text);
    let fallback = vec![(source.to_string(), style)];
    let segments: &[(String, Style)] = if segments.is_some_and(|s| !s.is_empty()) {
        segments.unwrap()
    } else {
        &fallback
    };
    let mut rows = Vec::new();
    for (ci, (spans, chunk)) in wrap_segments(segments, code_w).into_iter().enumerate() {
        if ci == 0 {
            let mut line_spans = gutter_spans.clone();
            line_spans.extend(spans);
            rows.push((line_spans, format!("{}{}", gutter_plain, chunk)));
        } else {
            // Keep the gutter block connected through wrapped rows. The blank
            // line-number/divider area has exactly the same width as the first
            // row, so wrapped code starts in the identical content column.
            let continuation = " ".repeat(num_width + 3);
            let mut line_spans = vec![
                Span::styled(bar_text.to_string(), bar_style),
                Span::raw(continuation.clone()),
            ];
            line_spans.extend(spans);
            rows.push((line_spans, format!("{}{}{}", bar_text, continuation, chunk)));
        }
    }
    if rows.is_empty() {
        rows.push((gutter_spans, gutter_plain));
    }
    rows
}

/// Blank visual row used to keep side-by-side columns aligned when only the
/// counterpart has another wrapped segment. It continues this side's colored
/// block while deliberately leaving the line-number and content cells empty.
fn diff_column_continuation(bar: Option<Color>, num_width: usize) -> (Vec<Span<'static>>, String) {
    let bar_text = "█ ";
    let continuation = " ".repeat(num_width + 3);
    (
        vec![
            Span::styled(
                bar_text.to_string(),
                Style::default().fg(bar.unwrap_or(Color::DarkGray)),
            ),
            Span::raw(continuation.clone()),
        ],
        format!("{}{}", bar_text, continuation),
    )
}

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

    let (p, removed_end, added_end) = diff_alignment(&o, &n);

    if width >= SIDE_BY_SIDE_MIN_WIDTH {
        render_diff_columns(
            &o,
            &n,
            p,
            removed_end,
            added_end,
            path,
            1,
            mi,
            width,
            theme,
            out,
        );
        return;
    }

    let ctx: usize = 3;
    let num_width = n.len().max(o.len()).to_string().len().max(3);
    let gutter_w = num_width + 3;
    let avail = width.saturating_sub(gutter_w).max(1);
    let ctx_style = Style::default().fg(theme.muted);
    let changed_style = Style::default().bg(Color::DarkGray);
    let old_hl = highlight::highlight(old, path, theme);
    let new_hl = highlight::highlight(new, path, theme);
    let mut line_idx = 0usize;

    // Context before change
    let before_start = p.saturating_sub(ctx);
    for (i, &line) in o.iter().enumerate().take(p).skip(before_start) {
        push_diff_line(
            line,
            old_hl
                .as_ref()
                .and_then(|lines| lines.get(i).map(Vec::as_slice)),
            i + 1,
            DiffLineFormat {
                num_width,
                style: ctx_style,
                bar: None,
                background: None,
                avail,
                message_index: mi,
                line_idx,
            },
            out,
        );
        line_idx += 1;
    }

    // Removed lines
    for (i, &line) in o.iter().enumerate().take(removed_end).skip(p) {
        push_diff_line(
            line,
            old_hl
                .as_ref()
                .and_then(|lines| lines.get(i).map(Vec::as_slice)),
            i + 1,
            DiffLineFormat {
                num_width,
                style: changed_style,
                bar: Some(Color::Red),
                background: Some(Color::DarkGray),
                avail,
                message_index: mi,
                line_idx,
            },
            out,
        );
        line_idx += 1;
    }

    // Added lines
    for (i, &line) in n.iter().enumerate().take(added_end).skip(p) {
        push_diff_line(
            line,
            new_hl
                .as_ref()
                .and_then(|lines| lines.get(i).map(Vec::as_slice)),
            i + 1,
            DiffLineFormat {
                num_width,
                style: changed_style,
                bar: Some(Color::Green),
                background: Some(Color::DarkGray),
                avail,
                message_index: mi,
                line_idx,
            },
            out,
        );
        line_idx += 1;
    }

    // Context after change
    let after_end = (removed_end + ctx).min(o.len());
    for (i, &line) in o.iter().enumerate().take(after_end).skip(removed_end) {
        push_diff_line(
            line,
            old_hl
                .as_ref()
                .and_then(|lines| lines.get(i).map(Vec::as_slice)),
            i + 1,
            DiffLineFormat {
                num_width,
                style: ctx_style,
                bar: None,
                background: None,
                avail,
                message_index: mi,
                line_idx,
            },
            out,
        );
        line_idx += 1;
    }
}

#[derive(Clone, Copy)]
struct DiffLineFormat {
    num_width: usize,
    style: Style,
    /// Block color left of the line number: red = removed, green = added,
    /// None = unchanged dark gray.
    bar: Option<Color>,
    background: Option<Color>,
    avail: usize,
    message_index: usize,
    line_idx: usize,
}

/// Push one diff line with line number gutter, optional syntax segments, and hard-wrapping.
fn push_diff_line(
    src: &str,
    segments: Option<&[Segment]>,
    line_num: usize,
    format: DiffLineFormat,
    out: &mut Vec<RenderedLine>,
) {
    let DiffLineFormat {
        num_width,
        style,
        bar,
        background,
        avail,
        message_index: mi,
        line_idx,
    } = format;
    let ln_bg = if line_idx % 2 == 0 {
        Color::Reset
    } else {
        Color::DarkGray
    };
    let ln_style = Style::default().fg(Color::White).bg(ln_bg);
    let bar_text = "█ ";
    let bar_style = Style::default().fg(bar.unwrap_or(Color::DarkGray));
    let gutter_pad = format!("{:>width$} ", line_num, width = num_width);
    if src.is_empty() {
        let plain = format!("{}{}  ", bar_text, gutter_pad);
        out.push(
            RenderedLine::new(
                Line::from(vec![
                    Span::styled(bar_text.to_string(), bar_style),
                    Span::styled(gutter_pad, ln_style),
                    Span::styled("  ".to_string(), style),
                ]),
                plain,
                mi,
            )
            .with_background(background),
        );
        return;
    }

    if let Some(segments) = segments {
        let mut styled_segments: Vec<Segment> = segments
            .iter()
            .map(|(text, seg_style)| {
                let mut st = *seg_style;
                st.bg = style.bg;
                (text.clone(), st)
            })
            .collect();
        if styled_segments.is_empty() {
            styled_segments.push((src.to_string(), style));
        }
        for (ci, (spans, chunk_plain)) in wrap_segments(&styled_segments, avail)
            .into_iter()
            .enumerate()
        {
            let (mut row_spans, gutter) = diff_gutter(line_num, num_width, bar, ln_style, ci == 0);
            row_spans.extend(spans);
            out.push(
                RenderedLine::new(
                    Line::from(row_spans),
                    format!("{}{}", gutter, chunk_plain),
                    mi,
                )
                .with_background(background),
            );
        }
        return;
    }

    let styled_segments = vec![(src.to_string(), style)];
    for (ci, (content_spans, chunk)) in wrap_segments(&styled_segments, avail)
        .into_iter()
        .enumerate()
    {
        let (row_spans, gutter) = diff_gutter(line_num, num_width, bar, ln_style, ci == 0);
        let plain = format!("{}{}", gutter, chunk);
        let mut spans = row_spans;
        spans.extend(content_spans);
        out.push(RenderedLine::new(Line::from(spans), plain, mi).with_background(background));
    }
}

fn diff_gutter(
    line_num: usize,
    num_width: usize,
    bar: Option<Color>,
    ln_style: Style,
    first: bool,
) -> (Vec<Span<'static>>, String) {
    let bar_text = "█ ";
    let bar_style = Style::default().fg(bar.unwrap_or(Color::DarkGray));
    if first {
        let prefix = format!("{:>width$} ", line_num, width = num_width);
        let plain = format!("{}{} ", bar_text, prefix);
        (
            vec![
                Span::styled(bar_text.to_string(), bar_style),
                Span::styled(prefix, ln_style),
            ],
            plain,
        )
    } else {
        let padding = format!("{:>width$} ", "", width = num_width);
        let plain = format!("{}{} ", bar_text, padding);
        (
            vec![
                Span::styled(bar_text.to_string(), bar_style),
                Span::styled(padding, ln_style),
            ],
            plain,
        )
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
fn render_agent_result(
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
    if !output.contains("[agent-meta] ") {
        let legacy = output
            .strip_prefix("[agent-id:")
            .and_then(|text| text.split_once("]\n").map(|(_, body)| body))
            .unwrap_or(output);
        let running = legacy.strip_prefix("[running]\n");
        let report = running.unwrap_or(legacy);
        let state = if running.is_some() {
            "running"
        } else if ok {
            "completed"
        } else {
            "failed"
        };
        let expanded = show_output || toggled.contains(&(mi, bi));
        let arrow = if expanded { "▾ " } else { "▸ " };
        let icon = if running.is_some() {
            "◐"
        } else if ok {
            "●"
        } else {
            "×"
        };
        let header = format!("{} · {}", summary, state);
        out.push(
            RenderedLine::new(
                tool_chip_header("agent", arrow, icon, &header, None, ok, theme),
                header,
                mi,
            )
            .with_toggle((mi, bi)),
        );
        if expanded && !report.trim().is_empty() {
            render_markdown(report.trim(), mi, width.max(1), theme, out);
        }
        return;
    }

    let mut metadata = serde_json::Value::Null;
    let mut events = Vec::new();
    let mut report = "";
    let mut section = "";
    for line in output.lines() {
        if let Some(json) = line.strip_prefix("[agent-meta] ") {
            metadata = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
        } else if line == "[agent-events]" {
            section = "events";
        } else if line == "[agent-report]" {
            section = "report";
        } else if line.starts_with("[agent-id:") {
            continue;
        } else if section == "events" && !line.is_empty() {
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
                events.push(event);
            }
        } else if section == "report" {
            let offset = line.as_ptr() as usize - output.as_ptr() as usize;
            report = &output[offset..];
            break;
        }
    }

    let state = metadata["status"]
        .as_str()
        .unwrap_or(if ok { "completed" } else { "failed" });
    let activity = metadata["activity"].as_str().unwrap_or(state);
    let elapsed = metadata["elapsed_ms"].as_u64().map(fmt_duration_ms);
    let icon = match state {
        "running" => "◐",
        "completed" => "●",
        "unresolved" => "?",
        _ => "×",
    };
    let expanded = show_output || toggled.contains(&(mi, bi));
    let arrow = if expanded { "▾ " } else { "▸ " };
    let header = format!("{} · {}", summary, activity);
    let mut row = RenderedLine::new(
        tool_chip_header(
            "agent",
            arrow,
            icon,
            &header,
            elapsed.as_deref(),
            state != "failed",
            theme,
        ),
        header,
        mi,
    )
    .with_toggle((mi, bi));
    row.background = Some(Color::DarkGray);
    out.push(row);

    if !expanded {
        return;
    }

    let status_color = match state {
        "running" | "unresolved" => theme.warning,
        "completed" => theme.success,
        _ => theme.danger,
    };
    let mut details = vec![
        Span::styled(
            " STATUS ",
            Style::default()
                .bg(status_color)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} ", state.to_uppercase()),
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(duration) = elapsed.as_deref() {
        details.push(Span::styled(
            " DURATION ",
            Style::default()
                .bg(Color::DarkGray)
                .fg(theme.text)
                .add_modifier(Modifier::BOLD),
        ));
        details.push(Span::styled(
            format!(" {} ", duration),
            Style::default().fg(theme.text),
        ));
    }
    if let Some(task) = metadata["todo_index"].as_u64() {
        details.push(Span::styled(
            format!(" TASK {} ", task),
            Style::default()
                .bg(Color::DarkGray)
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        ));
    }
    out.push(RenderedLine::new(
        Line::from(details),
        format!("{} {}", state, elapsed.as_deref().unwrap_or("")),
        mi,
    ));

    if let Some(cwd) = metadata["cwd"].as_str() {
        out.push(RenderedLine::new(
            Line::from(vec![
                Span::styled(
                    " CWD ",
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(theme.text)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {}", crate::render::path::abbreviate_home(cwd)),
                    Style::default().fg(theme.muted),
                ),
            ]),
            cwd.to_string(),
            mi,
        ));
    }

    for (event_index, event) in events.into_iter().enumerate() {
        match event["kind"].as_str().unwrap_or("") {
            "tool" => {
                let name = event["name"].as_str().unwrap_or("tool");
                let summary = event["summary"].as_str().unwrap_or(name);
                let status = event["status"].as_str().unwrap_or("running");
                let event_ok = status != "failed";
                if status != "running" {
                    let call =
                        serde_json::from_value::<crate::agent::ToolCall>(event["call"].clone())
                            .ok();
                    if let Some(output) = event["output"].as_str() {
                        let nested_bi = usize::MAX.saturating_sub(event_index);
                        render_tool_result(
                            event_ok,
                            Some(name),
                            summary,
                            output,
                            call.as_ref(),
                            mi,
                            nested_bi,
                            width,
                            theme,
                            toggled,
                            show_output,
                            out,
                        );
                        continue;
                    }
                }
                let (event_icon, event_ok) = match status {
                    "completed" => ("✓", true),
                    "failed" => ("×", false),
                    _ => ("◐", true),
                };
                let duration = event["duration_ms"].as_u64().map(fmt_duration_ms);
                out.push(RenderedLine::new(
                    tool_chip_header(
                        name,
                        "",
                        event_icon,
                        summary,
                        duration.as_deref(),
                        event_ok,
                        theme,
                    ),
                    format!("{} {}", name, summary),
                    mi,
                ));
            }
            "checklist" => {
                let done = event["done"].as_u64().unwrap_or(0);
                let running = event["running"].as_u64().unwrap_or(0);
                let pending = event["pending"].as_u64().unwrap_or(0);
                let finalizing = running == 0 && pending == 0;
                let label = if finalizing {
                    format!(
                        "Local checklist complete · {} done · finalizing report",
                        done
                    )
                } else {
                    format!(
                        "Local checklist · {} done · {} running · {} pending",
                        done, running, pending
                    )
                };
                out.push(RenderedLine::new(
                    Line::from(vec![
                        Span::styled(
                            " CHECKLIST ",
                            Style::default()
                                .bg(theme.accent)
                                .fg(Color::Black)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!(" {} ", label),
                            Style::default().fg(if finalizing {
                                theme.warning
                            } else {
                                theme.text
                            }),
                        ),
                    ]),
                    label,
                    mi,
                ));
            }
            "phase" => {
                if let Some(text) = event["text"].as_str() {
                    out.push(RenderedLine::new(
                        Line::from(vec![
                            Span::styled(
                                " PHASE ",
                                Style::default()
                                    .bg(Color::DarkGray)
                                    .fg(theme.text)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(format!(" {}", text), Style::default().fg(theme.muted)),
                        ]),
                        text.to_string(),
                        mi,
                    ));
                }
            }
            _ => {}
        }
    }

    if !report.trim().is_empty() {
        let unresolved =
            state == "unresolved" || crate::agent::subtask::is_unresolved_report(report.trim());
        let label = if unresolved {
            " REVIEW UNRESOLVED "
        } else {
            " REVIEW "
        };
        let color = if unresolved {
            theme.warning
        } else {
            theme.accent
        };
        out.push(RenderedLine::new(
            Line::from(Span::styled(
                label,
                Style::default()
                    .bg(color)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            )),
            label.trim().into(),
            mi,
        ));
        let report = report
            .trim()
            .strip_prefix("[agent-outcome:unresolved]")
            .unwrap_or(report.trim())
            .trim();
        if let Some(structured) = crate::agent::report::verification_report(report) {
            render_verification_report(None, &structured, mi, width.max(1), theme, out);
        } else {
            render_markdown(report, mi, width.max(1), theme, out);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_tool_result(
    ok: bool,
    name: Option<&str>,
    summary: &str,
    output: &str,
    call: Option<&crate::agent::ToolCall>,
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
    let batch_calls = call.and_then(|call| call.expanded_calls().ok().flatten());
    let is_batch = batch_calls.as_ref().is_some_and(|items| !items.is_empty())
        || summary.contains(" operations)");
    let display_summary = crate::render::path::abbreviate_home(summary);
    let display_output = crate::render::path::abbreviate_home(output);

    // `todo` lives entirely in the sticky panel above the input — it never appears
    // in the scrollable transcript.
    if kind == Some(ToolKind::Todo) {
        return;
    }

    if kind == Some(ToolKind::Task) {
        render_agent_result(
            ok,
            &display_summary,
            &display_output,
            mi,
            bi,
            width,
            theme,
            toggled,
            show_output,
            out,
        );
        return;
    }

    if is_batch {
        render_batch_result(
            ok,
            kind,
            &display_summary,
            output,
            batch_calls.as_deref().unwrap_or(&[]),
            mi,
            bi,
            width,
            theme,
            toggled,
            show_output,
            out,
        );
        return;
    }

    // Mutating file tools have purpose-built source views backed by the original
    // executed call, rather than reconstructing code from the confirmation text.
    if !is_batch && matches!(kind, Some(ToolKind::Edit) | Some(ToolKind::Write)) {
        if !ok {
            push_status(
                "✗",
                &format!(
                    "{} failed: {}",
                    display_summary,
                    display_output.lines().next().unwrap_or("")
                ),
                theme.danger,
                mi,
                out,
            );
            return;
        }

        let confirmation = output
            .lines()
            .next()
            .filter(|line| !line.is_empty())
            .unwrap_or(&display_summary);
        let preview_lines = call
            .and_then(|call| match kind {
                Some(ToolKind::Edit) => call
                    .args
                    .get("old")
                    .or_else(|| call.args.get("old_string"))
                    .and_then(|value| value.as_str())
                    .map(|old| old.lines().count()),
                Some(ToolKind::Write) => call
                    .args
                    .get("content")
                    .and_then(|value| value.as_str())
                    .map(|content| content.lines().count()),
                _ => None,
            })
            .unwrap_or_else(|| output.lines().skip(1).count());
        let collapse_limit = if call.is_some() {
            WRITE_PREVIEW_LINES
        } else {
            12
        };
        let default_expanded = preview_lines <= collapse_limit;
        let expanded = show_output || (toggled.contains(&(mi, bi)) != default_expanded);
        let collapsible = preview_lines > collapse_limit && !show_output;
        let arrow = if collapsible {
            if expanded {
                "▾ "
            } else {
                "▸ "
            }
        } else {
            ""
        };
        let icon = kind.map(|tool| tool.icon()).unwrap_or("✓");
        let meta = (preview_lines > 0).then(|| format!("{} lines", preview_lines));
        let mut row = RenderedLine::new(
            tool_chip_header(
                kind.map(|tool| tool.name()).unwrap_or("tool"),
                arrow,
                icon,
                confirmation,
                meta.as_deref(),
                true,
                theme,
            ),
            format!("{} {}", icon, confirmation),
            mi,
        );
        if collapsible {
            row = row.with_toggle((mi, bi));
        }
        out.push(row);

        if expanded {
            let body_start = out.len();
            if let Some(call) = call {
                let path = call
                    .args
                    .get("path")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                match kind {
                    Some(ToolKind::Edit) => {
                        let old = call
                            .args
                            .get("old")
                            .or_else(|| call.args.get("old_string"))
                            .and_then(|value| value.as_str())
                            .unwrap_or("");
                        let new = call
                            .args
                            .get("new")
                            .or_else(|| call.args.get("new_string"))
                            .and_then(|value| value.as_str())
                            .unwrap_or("");
                        let start_line = call
                            .args
                            .get("__display_start_line")
                            .and_then(|value| value.as_u64())
                            .unwrap_or(1) as usize;
                        render_edit_comparison(
                            EditComparison {
                                path,
                                old,
                                new,
                                start_line,
                            },
                            mi,
                            width.saturating_sub(TOOL_BODY_INDENT).max(1),
                            theme,
                            out,
                        );
                    }
                    Some(ToolKind::Write) => {
                        let content = call
                            .args
                            .get("content")
                            .and_then(|value| value.as_str())
                            .unwrap_or("");
                        render_source_card(
                            SourceCard {
                                label: "CONTENT",
                                rail_color: theme.success,
                                path,
                                code: content,
                                start_line: 1,
                            },
                            mi,
                            width.saturating_sub(TOOL_BODY_INDENT).max(1),
                            theme,
                            out,
                        );
                    }
                    _ => {}
                }
            } else {
                let detail = output.lines().skip(1).collect::<Vec<_>>().join("\n");
                if !detail.is_empty() {
                    render_compact_diff_output(
                        &detail,
                        mi,
                        width.saturating_sub(TOOL_BODY_INDENT).max(1),
                        theme,
                        out,
                    );
                }
            }
            indent_tool_body(&mut out[body_start..], TOOL_BODY_INDENT);
        }
        return;
    }

    if kind == Some(ToolKind::Read) {
        if !ok {
            push_status(
                "✗",
                &format!(
                    "{} failed: {}",
                    display_summary,
                    output.lines().next().unwrap_or("")
                ),
                theme.danger,
                mi,
                out,
            );
            return;
        }

        let path = call
            .and_then(|call| call.args.get("path"))
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| summary_arg(summary).map(str::to_string))
            .unwrap_or_default();
        let (start_line, content, notes) = read_file_body(output);
        let source_lines = content.lines().count();
        let default_expanded = source_lines <= WRITE_PREVIEW_LINES;
        let expanded = show_output || (toggled.contains(&(mi, bi)) != default_expanded);
        let collapsible = source_lines > WRITE_PREVIEW_LINES && !show_output;
        let arrow = if collapsible {
            if expanded {
                "▾ "
            } else {
                "▸ "
            }
        } else {
            ""
        };
        let mut row = RenderedLine::new(
            tool_chip_header(
                "read",
                arrow,
                kind.map(|tool| tool.icon()).unwrap_or("✓"),
                &crate::render::path::abbreviate_home(&path),
                Some(&format!("{} lines", source_lines)),
                true,
                theme,
            ),
            format!("read {} ({} lines)", path, source_lines),
            mi,
        );
        if collapsible {
            row = row.with_toggle((mi, bi));
        }
        out.push(row);
        if expanded {
            let body_start = out.len();
            render_source_card(
                SourceCard {
                    label: "CONTENT",
                    rail_color: theme.accent,
                    path: &path,
                    code: &content,
                    start_line,
                },
                mi,
                width.saturating_sub(TOOL_BODY_INDENT).max(1),
                theme,
                out,
            );
            for note in notes {
                out.push(RenderedLine::new(
                    Line::from(Span::styled(note.clone(), Style::default().fg(theme.muted))),
                    note,
                    mi,
                ));
            }
            indent_tool_body(&mut out[body_start..], TOOL_BODY_INDENT);
        }
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
            | Some(ToolKind::PowerPoint)
    ) {
        let icon = if ok {
            kind.map(|k| k.icon()).unwrap_or("✓")
        } else {
            "✗"
        };
        let msg = display_output
            .lines()
            .next()
            .filter(|l| !l.is_empty())
            .unwrap_or(&display_summary);
        let label = kind.map(|k| k.name()).unwrap_or("tool");
        let plain = format!("{} {}", icon, msg);
        out.push(RenderedLine::new(
            tool_chip_header(label, "", icon, msg, None, ok, theme),
            plain,
            mi,
        ));
        return;
    }

    // Result-side tools (read/list/search/shell/web_*): the output is the payload.
    let lines: Vec<&str> = display_output.lines().collect();
    // Short output is always shown; long output collapses unless the global
    // "show output" toggle is on (or this block was individually flipped).
    let default_expanded = lines.len() <= 6;
    let expanded = show_output || (toggled.contains(&(mi, bi)) != default_expanded);
    let collapsible = lines.len() > 6 && !show_output;

    let icon = if ok {
        kind.map(|tool| tool.icon()).unwrap_or("✓")
    } else {
        "✗"
    };
    let arrow = if collapsible {
        if expanded {
            "▾ "
        } else {
            "▸ "
        }
    } else {
        ""
    };
    // The shell's command is already shown as a terminal prompt beneath the
    // header — don't repeat the `shell(...)` call wrapper on top.
    let header_summary = if ok && kind == Some(ToolKind::Shell) && !is_batch {
        crate::render::path::abbreviate_home(shell_command(call, summary))
    } else {
        display_summary.clone()
    };
    let header = format!(
        "    {}{} {} ({} lines)",
        arrow,
        icon,
        header_summary,
        lines.len()
    );
    let header_line = tool_chip_header(
        kind.map(|tool| tool.name()).unwrap_or("tool"),
        arrow,
        icon,
        &header_summary,
        Some(&format!("{} lines", lines.len())),
        ok,
        theme,
    );
    let mut row = RenderedLine::new(header_line, header, mi);
    if collapsible {
        row = row.with_toggle((mi, bi));
    }
    out.push(row);

    if expanded {
        let body_start = out.len();
        let avail = width.saturating_sub(TOOL_BODY_INDENT).max(1);
        // A successful `read` result is file content — syntax-highlight it by the
        // language inferred from the path inside the `read(path)` summary.
        let read_lang = if ok && kind == Some(ToolKind::Read) {
            summary_arg(summary).and_then(|p| highlight::is_supported(p).then(|| p.to_string()))
        } else {
            None
        };
        // Web results carry markdown links — render as markdown so sources are clickable.
        let as_markdown = ok
            && matches!(
                kind,
                Some(ToolKind::WebSearch)
                    | Some(ToolKind::WebFetch)
                    | Some(ToolKind::WebImages)
                    | Some(ToolKind::ReverseImage)
            );
        if let Some(lang) = read_lang.as_deref() {
            push_code(
                &display_output,
                lang,
                "",
                "",
                Style::default(),
                Style::default().fg(theme.muted),
                avail,
                mi,
                theme,
                out,
            );
        } else if as_markdown {
            render_markdown(&display_output, mi, avail, theme, out);
        } else if ok && kind == Some(ToolKind::Search) {
            let pattern = search_pattern_from_summary(summary);
            render_search_output(&display_output, pattern.as_deref(), mi, avail, theme, out);
        } else if ok
            && kind == Some(ToolKind::Shell)
            && !is_batch
            && render_shell_diff(
                shell_command(call, summary),
                &display_output,
                mi,
                avail,
                theme,
                out,
            )
        {
            // Shell patches keep their diff renderer beneath the highlighted prompt.
        } else if ok && kind == Some(ToolKind::Shell) && !is_batch {
            render_shell_output(
                shell_command(call, summary),
                &display_output,
                mi,
                avail,
                theme,
                out,
            );
        } else if render_unified_diff_output(&display_output, mi, avail, theme, out) {
            // Unified patches carry their own filenames, so changed code can be
            // parsed with the matching grammar instead of receiving only a flat
            // red/green line colour.
        } else {
            let style = Style::default().fg(theme.muted);
            for l in &lines {
                let segments = vec![(l.to_string(), style)];
                for (spans, plain) in wrap_segments(&segments, avail) {
                    out.push(RenderedLine::new(Line::from(spans), plain, mi));
                }
            }
        }
        indent_tool_body(&mut out[body_start..], TOOL_BODY_INDENT);
    }
}

#[derive(Debug)]
struct BatchResultSection<'a> {
    summary: &'a str,
    status: &'a str,
    body: &'a str,
}

fn parse_batch_result(output: &str) -> Vec<BatchResultSection<'_>> {
    let mut sections = Vec::new();
    let mut current_header: Option<(&str, &str)> = None;
    let mut body_start = 0usize;

    for (byte, _) in output.match_indices("## ") {
        if byte > 0 && !output[..byte].ends_with('\n') {
            continue;
        }
        let line_end = output[byte..]
            .find('\n')
            .map(|offset| byte + offset)
            .unwrap_or(output.len());
        let header = output[byte + 3..line_end].trim();
        let Some((position, rest)) = header.split_once(" · ") else {
            continue;
        };
        let valid_position = position.split_once('/').is_some_and(|(current, total)| {
            current.parse::<usize>().is_ok() && total.parse::<usize>().is_ok()
        });
        if !valid_position {
            continue;
        }
        let (summary, status) = rest
            .rsplit_once(" · ")
            .filter(|(_, status)| matches!(*status, "ok" | "error" | "cancelled"))
            .unwrap_or(("operation", rest));

        if let Some((previous_summary, previous_status)) = current_header.take() {
            let body = output[body_start..byte].trim_matches('\n');
            sections.push(BatchResultSection {
                summary: previous_summary,
                status: previous_status,
                body,
            });
        }
        current_header = Some((summary, status));
        body_start = line_end.saturating_add(1).min(output.len());
    }

    if let Some((summary, status)) = current_header {
        sections.push(BatchResultSection {
            summary,
            status,
            body: output[body_start..].trim_matches('\n'),
        });
    }
    sections
}

#[allow(clippy::too_many_arguments)]
fn render_batch_result(
    parent_ok: bool,
    kind: Option<crate::agent::ToolKind>,
    summary: &str,
    output: &str,
    calls: &[crate::agent::ToolCall],
    mi: usize,
    bi: usize,
    width: usize,
    theme: &Theme,
    toggled: &HashSet<(usize, usize)>,
    show_output: bool,
    out: &mut Vec<RenderedLine>,
) {
    use crate::agent::ToolKind;

    let sections = parse_batch_result(output);
    if sections.is_empty() {
        push_status(
            if parent_ok { "✓" } else { "✗" },
            summary,
            if parent_ok {
                theme.accent
            } else {
                theme.danger
            },
            mi,
            out,
        );
        return;
    }

    let failed = sections
        .iter()
        .filter(|section| section.status != "ok")
        .count();
    let succeeded = sections.len().saturating_sub(failed);
    let all_ok = parent_ok && failed == 0;
    let expanded = show_output || toggled.contains(&(mi, bi));
    let arrow = if expanded { "▾ " } else { "▸ " };
    let icon = if all_ok { "✓" } else { "✗" };
    let meta = if failed == 0 {
        format!("{} completed", succeeded)
    } else {
        format!("{} completed · {} failed", succeeded, failed)
    };
    out.push(
        RenderedLine::new(
            tool_chip_header(
                kind.map(|tool| tool.name()).unwrap_or("operations"),
                arrow,
                icon,
                summary,
                Some(&meta),
                all_ok,
                theme,
            ),
            format!("{} {} ({})", icon, summary, meta),
            mi,
        )
        .with_toggle((mi, bi)),
    );

    let body_width = width.saturating_sub(4).max(1);
    for (index, section) in sections.iter().enumerate() {
        let call = calls.get(index);
        let child_kind = call.and_then(crate::agent::ToolCall::kind).or(kind);
        let child_ok = section.status == "ok";
        let child_icon = if child_ok { "✓" } else { "✗" };
        let child_color = if child_ok {
            theme.success
        } else {
            theme.danger
        };
        let child_summary = call
            .map(crate::agent::ToolCall::summary)
            .unwrap_or_else(|| section.summary.to_string());
        let child_summary = crate::render::path::abbreviate_home(&child_summary);
        let number = format!("  {:>2}. ", index + 1);
        let mut spans = vec![
            Span::styled(number.clone(), Style::default().fg(theme.muted)),
            Span::styled(
                format!("{} ", child_icon),
                Style::default()
                    .fg(child_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                child_summary.clone(),
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
        ];
        if !child_ok {
            spans.push(Span::styled(
                format!("  {}", section.status),
                Style::default().fg(theme.danger),
            ));
        }
        for (row_spans, plain) in wrap_segments(
            &spans
                .into_iter()
                .map(|span| (span.content.into_owned(), span.style))
                .collect::<Vec<_>>(),
            width.max(1),
        ) {
            out.push(RenderedLine::new(Line::from(row_spans), plain, mi));
        }

        if !expanded || section.body.is_empty() {
            continue;
        }
        let detail_start = out.len();
        match (child_kind, call) {
            (Some(ToolKind::Edit), Some(call)) if child_ok => {
                let path = call.get_arg("path").unwrap_or("");
                let old = call
                    .get_arg("old")
                    .or_else(|| call.get_arg("old_string"))
                    .unwrap_or("");
                let new = call
                    .get_arg("new")
                    .or_else(|| call.get_arg("new_string"))
                    .unwrap_or("");
                render_edit_comparison(
                    EditComparison {
                        path,
                        old,
                        new,
                        start_line: 1,
                    },
                    mi,
                    body_width,
                    theme,
                    out,
                );
            }
            (Some(ToolKind::Write), Some(call)) if child_ok => {
                render_source_card(
                    SourceCard {
                        label: "CONTENT",
                        rail_color: theme.success,
                        path: call.get_arg("path").unwrap_or(""),
                        code: call.get_arg("content").unwrap_or(""),
                        start_line: 1,
                    },
                    mi,
                    body_width,
                    theme,
                    out,
                );
            }
            (Some(ToolKind::Read), Some(call)) if child_ok => {
                let (start_line, content, notes) = read_file_body(section.body);
                render_source_card(
                    SourceCard {
                        label: "CONTENT",
                        rail_color: theme.accent,
                        path: call.get_arg("path").unwrap_or(""),
                        code: &content,
                        start_line,
                    },
                    mi,
                    body_width,
                    theme,
                    out,
                );
                for note in notes {
                    push_status("›", &note, theme.muted, mi, out);
                }
            }
            (Some(ToolKind::Shell), Some(call)) if child_ok => {
                render_shell_output(
                    call.get_arg("command").unwrap_or(""),
                    section.body,
                    mi,
                    body_width,
                    theme,
                    out,
                );
            }
            _ => {
                let style = Style::default().fg(if child_ok { theme.muted } else { theme.danger });
                for line in section.body.lines() {
                    for (spans, plain) in wrap_segments(&[(line.to_string(), style)], body_width) {
                        out.push(RenderedLine::new(Line::from(spans), plain, mi));
                    }
                }
            }
        }
        indent_tool_body(&mut out[detail_start..], 4);
    }
}

fn render_compact_diff_output(
    output: &str,
    mi: usize,
    width: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    for line in output.lines() {
        let style = if line.starts_with("+ ") {
            Style::default().fg(theme.success)
        } else if line.starts_with("- ") {
            Style::default().fg(theme.danger)
        } else if line.starts_with("@@") {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        for (spans, plain) in wrap_segments(&[(line.to_string(), style)], width) {
            out.push(RenderedLine::new(Line::from(spans), plain, mi));
        }
    }
}

fn shell_command<'a>(call: Option<&'a crate::agent::ToolCall>, summary: &'a str) -> &'a str {
    call.and_then(|call| call.args.get("command"))
        .and_then(|value| value.as_str())
        .or_else(|| summary_arg(summary))
        .unwrap_or("")
}

fn render_shell_diff(
    command: &str,
    output: &str,
    mi: usize,
    width: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) -> bool {
    let is_diff = output.lines().any(|line| line.starts_with("@@"))
        && output
            .lines()
            .any(|line| line.starts_with("--- ") || line.starts_with("+++ "));
    if !is_diff {
        return false;
    }
    render_terminal_prompt(command, mi, width, theme, out);
    render_unified_diff_output(output, mi, width, theme, out)
}

/// Render shell output as a terminal session: a Bash-highlighted prompt followed
/// by opaque output rows and explicit stderr/exit status styling.
fn render_shell_output(
    command: &str,
    output: &str,
    mi: usize,
    width: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    render_terminal_prompt(command, mi, width, theme, out);

    let bg = Color::Black;
    let term_style = Style::default().bg(bg).fg(Color::White);
    let stderr_style = Style::default().bg(bg).fg(theme.danger);
    let exit_ok = Style::default()
        .bg(bg)
        .fg(theme.success)
        .add_modifier(Modifier::BOLD);
    let exit_fail = Style::default()
        .bg(bg)
        .fg(theme.danger)
        .add_modifier(Modifier::BOLD);
    let gutter = Style::default().bg(bg).fg(Color::DarkGray);
    let mut stderr = false;

    for line in output.lines() {
        let (display, style) = if line == "[stderr]" {
            stderr = true;
            (
                "stderr".to_string(),
                stderr_style.add_modifier(Modifier::BOLD),
            )
        } else if let Some(code) = line
            .strip_prefix("[exit ")
            .and_then(|text| text.strip_suffix(']'))
        {
            let ok = code == "0";
            (
                format!("process exited with status {}", code),
                if ok { exit_ok } else { exit_fail },
            )
        } else {
            (
                line.to_string(),
                if stderr { stderr_style } else { term_style },
            )
        };
        let segments = vec![("│ ".to_string(), gutter), (display, style)];
        for (spans, plain) in wrap_segments(&segments, width) {
            out.push(RenderedLine::new(Line::from(spans), plain, mi).with_background(Some(bg)));
        }
    }
}

fn render_terminal_prompt(
    command: &str,
    mi: usize,
    width: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    let bg = Color::Black;
    let prompt = Style::default()
        .bg(bg)
        .fg(theme.success)
        .add_modifier(Modifier::BOLD);
    let continuation = Style::default().bg(bg).fg(Color::DarkGray);
    let fallback = Style::default().bg(bg).fg(Color::White);
    let highlighted = highlight::highlight(command, "bash", theme).unwrap_or_else(|| {
        command
            .lines()
            .map(|line| vec![(line.to_string(), fallback)])
            .collect()
    });

    for (line_index, segments) in highlighted.into_iter().enumerate() {
        let prefix = if line_index == 0 { "$ " } else { "> " };
        let prefix_style = if line_index == 0 {
            prompt
        } else {
            continuation
        };
        let mut terminal_segments = vec![(prefix.to_string(), prefix_style)];
        terminal_segments.extend(segments.into_iter().map(|(text, style)| {
            let mut style = style;
            style.bg = Some(bg);
            (text, style)
        }));
        for (spans, plain) in wrap_segments(&terminal_segments, width) {
            out.push(RenderedLine::new(Line::from(spans), plain, mi).with_background(Some(bg)));
        }
    }
}

/// Render unified-diff output with Tree-sitter styles inferred from each file
/// header. Returns false when `output` is not recognisably a patch.
fn render_unified_diff_output(
    output: &str,
    mi: usize,
    width: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) -> bool {
    let is_diff = output.lines().any(|line| line.starts_with("@@"))
        && output
            .lines()
            .any(|line| line.starts_with("--- ") || line.starts_with("+++ "));
    if !is_diff {
        return false;
    }

    let mut path = String::new();
    for line in output.lines() {
        if let Some(candidate) = line.strip_prefix("+++ ") {
            path = candidate
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_start_matches("b/")
                .to_string();
        }
        let (marker, body, marker_color) = if line.starts_with("+++") || line.starts_with("---") {
            ("", line, theme.accent)
        } else if let Some(body) = line.strip_prefix('+') {
            ("+", body, theme.success)
        } else if let Some(body) = line.strip_prefix('-') {
            ("-", body, theme.danger)
        } else if let Some(body) = line.strip_prefix(' ') {
            (" ", body, theme.muted)
        } else {
            ("", line, diff_line_color(line, theme))
        };

        let segments = if marker.is_empty() || path.is_empty() {
            vec![(body.to_string(), Style::default().fg(marker_color))]
        } else {
            highlight::highlight(body, &path, theme)
                .and_then(|lines| lines.into_iter().next())
                .unwrap_or_else(|| vec![(body.to_string(), Style::default().fg(theme.muted))])
        };
        for (row_index, (spans, plain)) in
            wrap_segments(&segments, width.saturating_sub(marker.len()).max(1))
                .into_iter()
                .enumerate()
        {
            let lead = if row_index == 0 { marker } else { " " };
            let mut row_spans = vec![Span::styled(
                lead.to_string(),
                Style::default()
                    .fg(marker_color)
                    .add_modifier(Modifier::BOLD),
            )];
            row_spans.extend(spans);
            out.push(RenderedLine::new(
                Line::from(row_spans),
                format!("{}{}", lead, plain),
                mi,
            ));
        }
    }
    true
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
                spans.push(Span::styled(
                    inner,
                    Style::default()
                        .fg(theme.warning)
                        .add_modifier(Modifier::BOLD),
                ));
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
            created_at: None,
        }]
    }

    #[test]
    fn hides_only_the_final_tool_invocation_paragraph() {
        let call = crate::agent::ToolCall {
            name: "read".into(),
            args: serde_json::json!({"path": "a.rs"}),
            id: None,
        };
        let rows = build(
            &doc(
                "assistant",
                vec![
                    Block::Markdown(
                        "The parser needs one more verification.\n\nI’ll inspect the file now."
                            .into(),
                    ),
                    Block::ToolCall(call),
                ],
            ),
            60,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows.iter().any(|row| row
            .plain
            .contains("The parser needs one more verification.")));
        assert!(!rows
            .iter()
            .any(|row| row.plain.contains("I’ll inspect the file now.")));
    }

    #[test]
    fn unrelated_plan_prose_before_tool_call_remains_visible() {
        let call = crate::agent::ToolCall {
            name: "read".into(),
            args: serde_json::json!({"path": "a.rs"}),
            id: None,
        };
        let rows = build(
            &doc(
                "assistant",
                vec![
                    Block::Markdown("Plan:\n1. Confirm parsing.\n2. Verify rendering.".into()),
                    Block::ToolCall(call),
                ],
            ),
            60,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows.iter().any(|row| row.plain.contains("Plan:")));
        assert!(rows.iter().any(|row| row.plain.contains("Confirm parsing")));
    }

    #[test]
    fn prose_around_partial_tool_payload_remains_visible() {
        let rows = build(
            &doc(
                "assistant",
                vec![
                    Block::Markdown("Preparing one operation.".into()),
                    Block::Code {
                        lang: "tool".into(),
                        code: "{\"name\":\"read\"".into(),
                    },
                    Block::Markdown("This explanation remains visible.".into()),
                ],
            ),
            60,
            &Theme::default(),
            &HashSet::new(),
            false,
            true,
        );
        assert!(rows
            .iter()
            .any(|row| row.plain.contains("Preparing one operation.")));
        assert!(rows
            .iter()
            .any(|row| row.plain.contains("This explanation remains visible.")));
        assert!(!rows.iter().any(|row| row.plain.contains("{\"name\"")));
    }

    #[test]
    fn markdown_wraps_to_width() {
        let msgs = doc(
            "assistant",
            vec![Block::Markdown("aaaa bbbb cccc dddd".into())],
        );
        let rows = build(&msgs, 9, &Theme::default(), &HashSet::new(), false, false);
        // block header + 2-space indented wrapped lines + blank
        for r in &rows {
            let w = unicode_width::UnicodeWidthStr::width(r.plain.as_str());
            assert!(w <= 9, "row exceeds width 9: {:?} (width={})", r.plain, w);
        }
    }

    #[test]
    fn markdown_tables_render_with_borders_rows_and_alignment() {
        let rows = build(
            &doc(
                "assistant",
                vec![Block::Markdown(
                    "| Name | Count | Note |\n| :--- | ---: | :---: |\n| Alpha | 12 | ready |\n| Beta | 3 | waiting |".into(),
                )],
            ),
            60,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows.iter().any(|row| row.plain.contains('┌')));
        assert!(rows.iter().any(|row| row.plain.contains("Alpha")));
        assert!(rows.iter().any(|row| row.plain.contains("Beta")));
        assert!(!rows.iter().any(|row| row.plain.contains("---:")));
        let alpha = rows.iter().find(|row| row.plain.contains("Alpha")).unwrap();
        let count_start = alpha.plain.find("12").unwrap();
        let count_cell_start = alpha.plain[..count_start].rfind('│').unwrap();
        assert!(alpha.plain[count_cell_start + '│'.len_utf8()..count_start].len() > 1);
    }

    #[test]
    fn markdown_tables_wrap_to_available_width() {
        let rows = build(
            &doc(
                "assistant",
                vec![Block::Markdown(
                    "A | B\n--- | ---\na very long value | another long value".into(),
                )],
            ),
            24,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows.iter().any(|row| row.plain.contains('┌')));
        for row in rows {
            assert!(
                display_width(&row.plain) <= 24,
                "table row too wide: {:?}",
                row.plain
            );
        }
    }

    #[test]
    fn non_table_pipe_text_stays_plain_markdown() {
        let rows = build(
            &doc(
                "assistant",
                vec![Block::Markdown("a | b\nnot a separator".into())],
            ),
            40,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(!rows.iter().any(|row| row.plain.contains('┌')));
        assert!(rows.iter().any(|row| row.plain.contains("a | b")));
    }

    #[test]
    fn styled_segments_wrap_whole_words_and_keep_styles() {
        let keyword = Style::default().fg(Color::Blue);
        let string = Style::default().fg(Color::Green).bg(Color::DarkGray);
        let rows = wrap_segments(
            &[
                ("let value = ".into(), keyword),
                ("highlighted text".into(), string),
            ],
            13,
        );

        assert_eq!(
            rows.iter()
                .map(|(_, plain)| plain.as_str())
                .collect::<Vec<_>>(),
            vec!["let value =", "highlighted", "text"]
        );
        assert!(rows[1].0.iter().all(|span| span.style == string));
        assert!(rows[2].0.iter().all(|span| span.style == string));
    }

    #[test]
    fn styled_segments_only_hard_break_overlong_words() {
        let style = Style::default().fg(Color::Yellow);
        let rows = wrap_segments(&[("tiny abcdefghij".into(), style)], 6);
        assert_eq!(
            rows.iter()
                .map(|(_, plain)| plain.as_str())
                .collect::<Vec<_>>(),
            vec!["tiny", "abcdef", "ghij"]
        );
    }

    #[test]
    fn text_segments_are_indented_inside_the_message() {
        let rows = build(
            &doc("assistant", vec![Block::Markdown("indented text".into())]),
            40,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        let text = rows
            .iter()
            .find(|row| row.plain.contains("indented text"))
            .expect("text row");
        assert!(text.plain.starts_with("    indented text"));
    }

    #[test]
    fn thinking_first_message_has_no_blank_row_above_section() {
        let rows = build(
            &doc("assistant", vec![Block::Thinking("reasoning".into())]),
            40,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        let thinking = rows
            .iter()
            .position(|row| row.plain.contains("thinking (1 lines)"))
            .expect("thinking row");
        assert_eq!(
            thinking, 1,
            "thinking should immediately follow role header"
        );
        assert!(!rows[thinking - 1].plain.trim().is_empty());
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
    fn horizontal_rule_renders_as_text_rule_without_background() {
        let rows = build(
            &doc("assistant", vec![Block::Markdown("a\n---\nb".into())]),
            20,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows.iter().any(|row| row.plain.contains('─')));
        // The block header row has a colored background; horizontal rule content does not.
        for row in rows.iter().filter(|r| r.role_start.is_none()) {
            for span in &row.line.spans {
                assert!(
                    span.style.bg.is_none(),
                    "non-header span has bg: {:?}",
                    span
                );
            }
        }
        assert!(!rows.iter().any(|row| row.plain.trim() == "---"));
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
    fn tool_headers_show_status_icon_without_tool_name_chip() {
        let theme = Theme::default();
        let line = tool_chip_header("shell", "▸ ", "✓", "cargo test", Some("1.2s"), true, &theme);
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("✓"));
        assert!(text.contains("cargo test"));
        assert!(!text.to_ascii_lowercase().contains("shell"));
    }

    #[test]
    fn subagent_tool_events_use_the_same_file_renderer() {
        let output = concat!(
            "[agent-id:7]\n",
            "[agent-meta] {\"status\":\"completed\",\"activity\":\"done\"}\n",
            "[agent-events]\n",
            "{\"kind\":\"tool\",\"name\":\"read\",\"summary\":\"read(src/main.rs)\",\"status\":\"completed\",\"duration_ms\":42,\"call\":{\"name\":\"file_management\",\"args\":{\"action\":\"read\",\"path\":\"src/main.rs\"},\"id\":null},\"output\":\"[lines 5-6 of 6]\\nfn main() {}\\nlet value = true;\"}\n",
            "[agent-report]\n",
            "Done."
        );
        let block = Block::ToolResult {
            ok: true,
            name: Some("agent".into()),
            summary: "agent 1 (\"inspect\")".into(),
            output: output.into(),
        };
        let mut toggled = HashSet::new();
        toggled.insert((0, 0));
        let rows = build(
            &doc("tool", vec![block]),
            100,
            &Theme::default(),
            &toggled,
            false,
            false,
        );
        assert!(rows.iter().any(|row| row.plain.contains("█ CONTENT")));
        assert!(rows.iter().any(|row| row.plain.contains("█ 5 │ fn main()")));
        assert!(rows.iter().any(|row| {
            row.line.spans.iter().any(|span| {
                span.content.as_ref() == "fn" && span.style.fg == Some(Theme::default().hl_keyword)
            })
        }));
    }

    #[test]
    fn agent_result_is_inline_collapsed_and_expands_with_highlighted_tools() {
        let output = concat!(
            "[agent-id:7]\n",
            "[agent-meta] {\"status\":\"running\",\"activity\":\"Local checklist complete · finalizing report\",\"todo_index\":2,\"cwd\":\"/tmp/project\",\"elapsed_ms\":1250}\n",
            "[agent-events]\n",
            "{\"kind\":\"checklist\",\"done\":3,\"running\":0,\"pending\":0}\n",
            "{\"kind\":\"tool\",\"name\":\"search\",\"summary\":\"search(\\\"barrier\\\")\",\"status\":\"completed\",\"duration_ms\":42}\n",
            "[agent-report]\n",
            "Found the lifecycle boundary."
        );
        let block = Block::ToolResult {
            ok: true,
            name: Some("agent".into()),
            summary: "agent 1 → task 2 (\"trace lifecycle\")".into(),
            output: output.into(),
        };
        let collapsed = build(
            &doc("tool", vec![block.clone()]),
            100,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(collapsed.iter().any(|row| {
            row.plain
                .contains("Local checklist complete · finalizing report")
                && row.toggle == Some((0, 0))
        }));
        assert!(!collapsed
            .iter()
            .any(|row| row.plain.contains("Found the lifecycle boundary")));

        let mut toggled = HashSet::new();
        toggled.insert((0, 0));
        let expanded = build(
            &doc("tool", vec![block]),
            100,
            &Theme::default(),
            &toggled,
            false,
            false,
        );
        assert!(expanded.iter().any(|row| {
            row.plain
                .contains("Local checklist complete · 3 done · finalizing report")
        }));
        assert!(expanded.iter().any(|row| {
            row.plain.contains("search(\"barrier\")")
                && row.line.spans.iter().any(|span| span.style.bg.is_some())
        }));
        assert!(expanded
            .iter()
            .any(|row| row.plain.contains("Found the lifecycle boundary")));
        assert!(expanded
            .iter()
            .any(|row| row.plain.contains("running 1.2s")));
    }

    #[test]
    fn completed_agent_card_shows_final_status_and_duration() {
        let output = concat!(
            "[agent-id:8]\n",
            "[agent-meta] {\"status\":\"completed\",\"activity\":\"trace lifecycle\",\"todo_index\":null,\"cwd\":\".\",\"elapsed_ms\":2345}\n",
            "[agent-events]\n",
            "[agent-report]\n",
            "Done."
        );
        let block = Block::ToolResult {
            ok: true,
            name: Some("agent".into()),
            summary: "agent 1 (\"trace lifecycle\")".into(),
            output: output.into(),
        };
        let mut toggled = HashSet::new();
        toggled.insert((0, 0));
        let rows = build(
            &doc("tool", vec![block]),
            80,
            &Theme::default(),
            &toggled,
            false,
            false,
        );
        assert!(rows.iter().any(|row| row.plain.contains("completed 2.3s")));
        assert!(rows.iter().any(|row| row.plain.contains("Done.")));
    }

    #[test]
    fn unresolved_agent_card_is_polished_and_hides_internal_marker() {
        let output = concat!(
            "[agent-id:9]\n",
            "[agent-meta] {\"status\":\"unresolved\",\"activity\":\"review unavailable\",\"cwd\":\".\",\"elapsed_ms\":900}\n",
            "[agent-events]\n",
            "[agent-report]\n",
            "[agent-outcome:unresolved]\n## Review unresolved\n\n**Reason:** The provider rejected the child request."
        );
        let block = Block::ToolResult {
            ok: true,
            name: Some("agent".into()),
            summary: "agent 7 (\"review\")".into(),
            output: output.into(),
        };
        let mut toggled = HashSet::new();
        toggled.insert((0, 0));
        let rows = build(
            &doc("tool", vec![block]),
            90,
            &Theme::default(),
            &toggled,
            false,
            false,
        );
        assert!(rows
            .iter()
            .any(|row| row.plain.contains("REVIEW UNRESOLVED")));
        assert!(rows
            .iter()
            .any(|row| row.plain.contains("provider rejected")));
        assert!(!rows.iter().any(|row| row.plain.contains("agent-outcome")));
        assert!(!rows.iter().any(|row| row.plain.contains("(failed):")));
    }

    #[test]
    fn embedded_verification_reports_render_as_status_cards_without_raw_json() {
        let text = concat!(
            "All delegated child agents have completed. Reports:\n\n",
            "agent 1 (completed):\n",
            "{\"schema\":\"aitui.verification-summary.v1\",\"status\":\"verified\",",
            "\"findings\":[{\"check_id\":\"latency\",\"answer\":\"yes\",",
            "\"statement\":\"Avoidable waits were found.\",\"support\":\"2/2 replicas\",",
            "\"evidence\":[\"src/agent/subtask.rs:201-207\"]}],",
            "\"unresolved\":[],\"diagnostics\":[]}\n\n---\n\n",
            "agent 2 (unresolved):\n",
            "{\"schema\":\"aitui.verification-summary.v1\",\"status\":\"unresolved\",",
            "\"findings\":[],\"unresolved\":[\"access\"],",
            "\"diagnostics\":[\"invalid report\"]}"
        );
        let rows = build(
            &doc("user", vec![Block::Markdown(text.into())]),
            100,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows.iter().any(|row| {
            row.plain.contains("✓ VERIFIED") && row.plain.contains("agent 1 (completed)")
        }));
        assert!(rows.iter().any(|row| row.plain.contains("✓ YES  latency")));
        assert!(rows
            .iter()
            .any(|row| row.plain.contains("Evidence: src/agent/subtask.rs:201-207")));
        assert!(rows.iter().any(|row| {
            row.plain.contains("? UNRESOLVED") && row.plain.contains("agent 2 (unresolved)")
        }));
        assert!(rows
            .iter()
            .any(|row| row.plain.contains("Unresolved checks: access")));
        assert!(rows
            .iter()
            .any(|row| row.plain.contains("1 diagnostic note(s)")));
        assert!(!rows.iter().any(|row| {
            row.plain.contains("aitui.verification-summary.v1")
                || row.plain.contains("\"findings\"")
        }));
    }

    #[test]
    fn agent_report_json_uses_structured_finding_icons() {
        let output = concat!(
            "[agent-id:10]\n",
            "[agent-meta] {\"status\":\"completed\",\"activity\":\"verified\",\"cwd\":\".\",\"elapsed_ms\":500}\n",
            "[agent-events]\n[agent-report]\n",
            "{\"schema\":\"aitui.verification-summary.v1\",\"status\":\"verified\",",
            "\"findings\":[{\"check_id\":\"access\",\"answer\":\"mixed\",",
            "\"statement\":\"Some paths still need review.\",\"support\":\"2/3 replicas\",",
            "\"evidence\":[]}],\"unresolved\":[],\"diagnostics\":[]}"
        );
        let block = Block::ToolResult {
            ok: true,
            name: Some("agent".into()),
            summary: "agent 3 (\"review\")".into(),
            output: output.into(),
        };
        let mut toggled = HashSet::new();
        toggled.insert((0, 0));
        let rows = build(
            &doc("tool", vec![block]),
            90,
            &Theme::default(),
            &toggled,
            false,
            false,
        );
        assert!(rows.iter().any(|row| row.plain.contains("✓ VERIFIED")));
        assert!(rows.iter().any(|row| row.plain.contains("◐ MIXED  access")));
        assert!(!rows
            .iter()
            .any(|row| row.plain.contains("aitui.verification-summary.v1")));
    }

    #[test]
    fn search_result_highlights_path_line_and_match() {
        let block = Block::ToolResult {
            ok: true,
            name: Some("search".into()),
            summary: "search(\"needle\")".into(),
            output: "2 match(es) for 'needle' (showing 1-2):\n  src/a.rs:12: let needle = true;"
                .into(),
        };
        let rows = build(
            &doc("tool", vec![block]),
            80,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        let file_index = rows
            .iter()
            .position(|row| row.plain.trim() == "FILE  src/a.rs")
            .expect("search FILE row");
        assert!(file_index > 0);
        assert!(rows[file_index - 1].plain.trim().is_empty());
        assert!(rows[file_index].plain.starts_with("    "));
        let match_row = rows
            .iter()
            .find(|row| row.plain.contains("let needle = true"))
            .expect("search match row");
        assert!(match_row.plain.starts_with("    "));
        assert!(rows.iter().any(|r| r.plain.contains("src/a.rs")));
        assert!(rows.iter().any(|r| {
            r.line.spans.iter().any(|s| {
                s.content.as_ref() == "needle"
                    && s.style.fg == Some(Color::Black)
                    && s.style.bg == Some(Theme::default().warning)
                    && s.style.add_modifier.contains(Modifier::BOLD)
            })
        }));
        assert!(rows.iter().any(|r| {
            r.plain.trim() == "FILE  src/a.rs"
                && r.line.spans.iter().any(|span| {
                    span.content.as_ref() == "FILE"
                        && span.style.fg == Some(Theme::default().accent)
                })
        }));
        assert!(rows.iter().any(|r| {
            r.line.spans.iter().any(|s| {
                s.content.as_ref() == "let" && s.style.fg == Some(Theme::default().hl_keyword)
            })
        }));
    }

    #[test]
    fn search_result_highlights_regex_match_segments() {
        let block = Block::ToolResult {
            ok: true,
            name: Some("search".into()),
            summary: "search(\"foo\\s+bar\")".into(),
            output:
                "1 match(es) for 'foo\\s+bar' (showing 1-1):\n  src/a.rs:7: let value = foo   bar;"
                    .into(),
        };
        let rows = build(
            &doc("tool", vec![block]),
            80,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows.iter().any(|row| {
            row.line.spans.iter().any(|span| {
                span.content.as_ref() == "foo   bar"
                    && span.style.bg == Some(Theme::default().warning)
            })
        }));
    }

    #[test]
    fn search_results_are_grouped_below_one_home_shortened_path_header() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let home = std::path::Path::new(&home).display();
        let path = format!("{home}/Codes/demo/src/a.rs");
        let block = Block::ToolResult {
            ok: true,
            name: Some("search".into()),
            summary: "search(\"needle\")".into(),
            output: format!(
                "2 match(es) for 'needle' (showing 1-2):\n  {path}:12: let needle = true;\n  {path}:18: println!(\"needle\");"
            ),
        };
        let rows = build(
            &doc("tool", vec![block]),
            80,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        let header_indices: Vec<_> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.plain.contains("~/Codes/demo/src/a.rs"))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(header_indices.len(), 1);
        let header = header_indices[0];
        let first_match = rows
            .iter()
            .position(|row| row.plain.trim_start().starts_with("12 "))
            .expect("first match row");
        let second_match = rows
            .iter()
            .position(|row| row.plain.trim_start().starts_with("18 "))
            .expect("second match row");
        assert!(header < first_match && first_match < second_match);
        assert!(!rows[first_match].plain.contains(&path));
        assert!(!rows[second_match].plain.contains(&path));
    }

    #[test]
    fn wrapped_search_results_keep_an_opaque_aligned_gutter() {
        let block = Block::ToolResult {
            ok: true,
            name: Some("search".into()),
            summary: "search(\"needle\")".into(),
            output: "1 match(es) for 'needle' (showing 1-1):\n  src/a.rs:12: alpha beta gamma needle delta epsilon"
                .into(),
        };
        let rows = build(
            &doc("tool", vec![block]),
            24,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        let result_rows: Vec<_> = rows
            .iter()
            .filter(|row| {
                row.plain.contains("alpha beta")
                    || row.plain.contains("needle delta")
                    || row.plain.contains("epsilon")
            })
            .collect();
        assert!(result_rows.len() >= 2);
        for row in result_rows {
            assert!(!row.line.spans.is_empty());
        }
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

    /// Whether a rendered code row contains a highlighted `fn` keyword span.
    fn has_keyword_colour(rows: &[RenderedLine]) -> bool {
        let kw = Theme::default().hl_keyword;
        rows.iter().any(|row| {
            row.line
                .spans
                .iter()
                .any(|span| span.content.as_ref() == "fn" && span.style.fg == Some(kw))
        })
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
    fn code_blocks_use_terminal_background() {
        let rows = build(
            &doc(
                "assistant",
                vec![Block::Code {
                    lang: "rust".into(),
                    code: "fn a() {}".into(),
                }],
            ),
            60,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        let code = rows.iter().find(|row| row.plain.contains("fn a()"));
        assert!(code.is_some_and(|row| row.line.spans.iter().all(|span| span.style.bg.is_none())));
    }

    #[test]
    fn tool_call_blocks_are_not_user_visible() {
        let call = crate::agent::ToolCall {
            name: "write_file".into(),
            args: serde_json::json!({"path": "a.rs", "content": "fn a() {}\n"}),
            id: None,
        };
        let rows = build(
            &doc("assistant", vec![Block::ToolCall(call)]),
            60,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows.is_empty(), "call-only messages leave no visible shell");
    }

    #[test]
    fn streaming_partial_tool_payload_is_hidden() {
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
        assert!(rows.is_empty());
    }

    #[test]
    fn streaming_and_finalized_messages_hide_only_tool_invocation_prose() {
        let blocks = vec![
            Block::Markdown("let me edit the file".into()),
            Block::ToolCall(crate::agent::ToolCall {
                name: "edit".into(),
                args: serde_json::json!({"path": "a.rs", "old": "x", "new": "y"}),
                id: None,
            }),
        ];
        for streaming in [true, false] {
            let rows = build(
                &doc("assistant", blocks.clone()),
                60,
                &Theme::default(),
                &HashSet::new(),
                false,
                streaming,
            );
            assert!(rows.is_empty());
        }
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
    fn read_file_view_shows_path_actual_line_numbers_and_highlighting() {
        let block = Block::ToolFileResult {
            ok: true,
            name: Some("read".into()),
            summary: "read(src/main.rs)".into(),
            output: "[lines 41-42 of 100]\nfn main() {}\nlet value = 1;\n[next: read(path=\"src/main.rs\", offset=43, limit=2)]".into(),
            call: crate::agent::ToolCall {
                name: "file_management".into(),
                args: serde_json::json!({"action": "read", "path": "src/main.rs", "offset": "41", "limit": "2"}),
                id: None,
            },
        };
        let rows = build(
            &doc("tool", vec![block]),
            80,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows.iter().any(|row| row.plain.contains("█ CONTENT")));
        assert!(rows
            .iter()
            .any(|row| row.plain.contains("█ 41 │ fn main()")));
        assert!(rows
            .iter()
            .any(|row| row.plain.contains("█ 42 │ let value")));
        assert!(rows.iter().any(|row| row.plain.contains("[next: read(")));
        assert!(has_keyword_colour(&rows));
    }

    #[test]
    fn write_file_view_shows_path_line_numbers_and_highlighting() {
        let block = Block::ToolFileResult {
            ok: true,
            name: Some("write".into()),
            summary: "write(src/lib.rs · 2 lines)".into(),
            output: "Created /tmp/src/lib.rs (2 lines)".into(),
            call: crate::agent::ToolCall {
                name: "file_management".into(),
                args: serde_json::json!({
                    "action": "write",
                    "path": "src/lib.rs",
                    "content": "pub fn answer() -> usize {\n    42\n}"
                }),
                id: None,
            },
        };
        let rows = build(
            &doc("tool", vec![block]),
            80,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows.iter().any(|row| row.plain.contains("█ CONTENT")));
        assert!(rows
            .iter()
            .any(|row| row.plain.contains("█ 1 │ pub fn answer")));
        assert!(rows.iter().any(|row| row.plain.contains("█ 3 │ }")));
        assert!(has_keyword_colour(&rows));
    }

    #[test]
    fn edit_file_view_matches_access_request_old_new_cards_with_actual_lines() {
        let theme = Theme::default();
        let block = Block::ToolFileResult {
            ok: true,
            name: Some("edit".into()),
            summary: "edit(src/lib.rs)".into(),
            output: "Edit /tmp/src/lib.rs (1 occurrence)".into(),
            call: crate::agent::ToolCall {
                name: "file_management".into(),
                args: serde_json::json!({
                    "action": "edit",
                    "path": "src/lib.rs",
                    "old": "fn old() {\n    false\n}",
                    "new": "fn new() {\n    true\n}",
                    "__display_start_line": 18
                }),
                id: None,
            },
        };
        let rows = build(
            &doc("tool", vec![block]),
            100,
            &theme,
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows.iter().any(|row| row.plain.contains("█ OLD")));
        assert!(rows.iter().any(|row| row.plain.contains("█ NEW")));
        assert!(!rows
            .iter()
            .any(|row| row.plain.contains("BEFORE") || row.plain.contains("AFTER")));
        let old = rows
            .iter()
            .find(|row| row.plain.contains("█ 18 │ fn old()"))
            .expect("OLD card source row");
        let new = rows
            .iter()
            .find(|row| row.plain.contains("█ 18 │ fn new()"))
            .expect("NEW card source row");
        assert!(old.line.spans.iter().any(|span| {
            span.content.as_ref() == "█ " && span.style.fg == Some(theme.danger)
        }));
        assert!(new.line.spans.iter().any(|span| {
            span.content.as_ref() == "█ " && span.style.fg == Some(theme.success)
        }));
        assert!(old
            .line
            .spans
            .iter()
            .any(|span| span.style.fg == Some(theme.hl_keyword)));
        assert!(new
            .line
            .spans
            .iter()
            .any(|span| span.style.fg == Some(theme.hl_keyword)));
    }

    #[test]
    fn edit_gutter_always_has_a_spaced_gray_or_change_block() {
        let (unchanged_spans, unchanged_plain) =
            diff_gutter(12, 3, None, Style::default().fg(Color::White), true);
        assert!(unchanged_plain.starts_with("█  12"));
        assert_eq!(unchanged_spans[0].content.as_ref(), "█ ");
        assert_eq!(unchanged_spans[0].style.fg, Some(Color::DarkGray));

        let (changed_spans, changed_plain) = diff_gutter(
            12,
            3,
            Some(Color::Green),
            Style::default().fg(Color::White),
            true,
        );
        assert!(changed_plain.starts_with("█  12"));
        assert_eq!(changed_spans[0].content.as_ref(), "█ ");
        assert_eq!(changed_spans[0].style.fg, Some(Color::Green));
    }

    #[test]
    fn wrapped_edit_lines_keep_the_change_block_without_repeating_line_numbers() {
        let theme = Theme::default();
        let mut rows = Vec::new();
        push_diff_line(
            "a deliberately long changed line",
            None,
            12,
            DiffLineFormat {
                num_width: 3,
                style: Style::default().fg(theme.text),
                bar: Some(theme.success),
                background: None,
                avail: 8,
                message_index: 0,
                line_idx: 0,
            },
            &mut rows,
        );

        assert!(rows.len() > 1);
        assert!(rows[0].plain.starts_with("█  12 "));
        assert!(rows[1].plain.starts_with("█      "));
        assert!(!rows[1].plain.contains("12"));
        assert!(rows.iter().all(|row| {
            row.line.spans.first().is_some_and(|span| {
                span.content.as_ref() == "█ " && span.style.fg == Some(theme.success)
            })
        }));
    }

    #[test]
    fn side_by_side_diff_falls_back_to_source_text_without_a_highlighter() {
        let theme = Theme::default();
        let mut rows = Vec::new();
        render_diff_columns(
            &["old text remains visible"],
            &["new text remains visible"],
            0,
            1,
            1,
            "README",
            1,
            0,
            120,
            &theme,
            &mut rows,
        );

        assert!(rows
            .iter()
            .any(|row| row.plain.contains("old text remains visible")));
        assert!(rows
            .iter()
            .any(|row| row.plain.contains("new text remains visible")));
    }

    #[test]
    fn side_by_side_diff_fills_old_gutter_when_new_wraps_farther() {
        let theme = Theme::default();
        let mut rows = Vec::new();
        render_diff_columns(
            &["short old"],
            &["a much longer new line that needs several wrapped visual rows"],
            0,
            1,
            1,
            "src/lib.rs",
            7,
            0,
            56,
            &theme,
            &mut rows,
        );

        assert!(rows.len() > 1);
        let continuation_colors = rows[1]
            .line
            .spans
            .iter()
            .filter(|span| span.content.as_ref() == "█ ")
            .map(|span| span.style.fg)
            .collect::<Vec<_>>();
        assert_eq!(
            continuation_colors,
            vec![Some(theme.danger), Some(theme.success)]
        );
        assert!(!rows[1].plain.contains("  7 │"));
    }

    #[test]
    fn side_by_side_diff_fills_new_gutter_when_old_wraps_farther() {
        let theme = Theme::default();
        let mut rows = Vec::new();
        render_diff_columns(
            &["a much longer old line that needs several wrapped visual rows"],
            &["short new"],
            0,
            1,
            1,
            "src/lib.rs",
            9,
            0,
            56,
            &theme,
            &mut rows,
        );

        assert!(rows.len() > 1);
        let continuation_colors = rows[1]
            .line
            .spans
            .iter()
            .filter(|span| span.content.as_ref() == "█ ")
            .map(|span| span.style.fg)
            .collect::<Vec<_>>();
        assert_eq!(
            continuation_colors,
            vec![Some(theme.danger), Some(theme.success)]
        );
        assert!(!rows[1].plain.contains("  9 │"));
    }

    #[test]
    fn wide_edit_diff_keeps_a_fixed_center_divider_when_lines_wrap() {
        let block = Block::ToolFileResult {
            ok: true,
            name: Some("edit".into()),
            summary: "edit(src/lib.rs)".into(),
            output: "Edit src/lib.rs (1 occurrence)".into(),
            call: crate::agent::ToolCall {
                name: "file_management".into(),
                args: serde_json::json!({
                    "action": "edit",
                    "path": "src/lib.rs",
                    "old": "/// A deliberately long documentation line that wraps on the left side without moving the divider.\nfn old() {}",
                    "new": "/// A deliberately long documentation line that wraps on the right side without moving the divider.\nfn new() {}",
                    "__display_start_line": 41
                }),
                id: None,
            },
        };
        let rows = build(
            &doc("tool", vec![block]),
            120,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        let diff_rows = rows
            .iter()
            .filter(|row| row.plain.contains(" │ ") && row.plain.contains("│"))
            .collect::<Vec<_>>();
        assert!(diff_rows.len() >= 2);
        let divider_columns = diff_rows
            .iter()
            .map(|row| {
                let midpoint = display_width(&row.plain) / 2;
                row.plain
                    .match_indices(" │ ")
                    .map(|(byte, _)| display_width(&row.plain[..byte]))
                    .min_by_key(|column| column.abs_diff(midpoint))
                    .expect("center divider")
            })
            .collect::<Vec<_>>();
        assert!(
            divider_columns
                .iter()
                .all(|column| *column == divider_columns[0]),
            "divider columns {divider_columns:?}; rows: {:?}",
            diff_rows
                .iter()
                .map(|row| row.plain.trim_end().to_string())
                .collect::<Vec<_>>()
        );
        assert!(diff_rows.iter().all(|row| row.plain.starts_with("    ")));
        assert!(diff_rows.iter().any(|row| row.plain.contains(" 41 │")));
    }

    #[test]
    fn unified_diff_tool_output_is_syntax_highlighted() {
        let block = Block::ToolResult {
            ok: true,
            name: Some("shell".into()),
            summary: "shell(git diff)".into(),
            output: "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-fn old() {}\n+fn new() {}"
                .into(),
        };
        let rows = build(
            &doc("tool", vec![block]),
            80,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows.iter().any(|row| row.plain.contains("+fn new()")));
        assert!(has_keyword_colour(&rows));
    }

    #[test]
    fn batched_results_use_operation_summary_rows_instead_of_raw_headers() {
        let block = Block::ToolFileResult {
            ok: true,
            name: Some("shell".into()),
            summary: "shell(2 operations)".into(),
            output: "## 1/2 · shell(cargo test) · ok\npassed\n\n## 2/2 · shell(cargo clippy) · error\nwarning".into(),
            call: crate::agent::ToolCall {
                name: "shell".into(),
                args: serde_json::json!({"commands": ["cargo test", "cargo clippy"]}),
                id: None,
            },
        };
        let rows = build(
            &doc("tool", vec![block]),
            80,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows
            .iter()
            .any(|row| row.plain.contains("1 completed · 1 failed")));
        assert!(rows
            .iter()
            .any(|row| row.plain.contains("1. ✓ shell(cargo test)")));
        assert!(rows
            .iter()
            .any(|row| row.plain.contains("2. ✗ shell(cargo clippy)")));
        assert!(!rows.iter().any(|row| row.plain.starts_with("## ")));
        assert!(!rows.iter().any(|row| row.plain.contains("passed")));
        assert!(rows.iter().any(|row| row.toggle.is_some()));
    }

    #[test]
    fn expanded_batched_shell_results_use_terminal_panels() {
        let block = Block::ToolFileResult {
            ok: true,
            name: Some("shell".into()),
            summary: "shell(2 operations)".into(),
            output: "## 1/2 · shell(cargo test) · ok\npassed\n\n## 2/2 · shell(cargo clippy) · ok\nclean".into(),
            call: crate::agent::ToolCall {
                name: "shell".into(),
                args: serde_json::json!({"commands": ["cargo test", "cargo clippy"]}),
                id: None,
            },
        };
        let mut toggled = HashSet::new();
        toggled.insert((0, 0));
        let rows = build(
            &doc("tool", vec![block]),
            80,
            &Theme::default(),
            &toggled,
            false,
            false,
        );
        assert!(rows.iter().any(|row| row.plain.contains("$ cargo test")));
        assert!(rows.iter().any(|row| row.plain.contains("passed")));
        assert!(rows.iter().any(|row| row.background == Some(Color::Black)));
    }

    #[test]
    fn batched_shell_result_does_not_render_an_empty_terminal_prompt() {
        let block = Block::ToolFileResult {
            ok: true,
            name: Some("shell".into()),
            summary: "shell(2 operations)".into(),
            output: "## 1/2 · shell(cargo test) · ok\npassed\n\n## 2/2 · shell(cargo clippy) · ok\nclean".into(),
            call: crate::agent::ToolCall {
                name: "shell".into(),
                args: serde_json::json!({"commands": ["cargo test", "cargo clippy"]}),
                id: None,
            },
        };
        let rows = build(
            &doc("tool", vec![block]),
            80,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows
            .iter()
            .any(|row| row.plain.contains("shell(2 operations)")));
        assert!(rows.iter().any(|row| row.plain.contains("cargo test")));
        assert!(!rows.iter().any(|row| row.plain.trim() == "$"));
    }

    #[test]
    fn batched_edit_result_does_not_render_empty_old_and_new_cards() {
        let block = Block::ToolFileResult {
            ok: true,
            name: Some("edit".into()),
            summary: "edit(2 operations)".into(),
            output: "## 1/2 · edit(a.rs) · ok\nEdit a.rs (1 occurrence)\n\n## 2/2 · edit(b.rs) · ok\nEdit b.rs (1 occurrence)".into(),
            call: crate::agent::ToolCall {
                name: "file_management".into(),
                args: serde_json::json!({
                    "action": "edit",
                    "batch": [
                        {"path": "a.rs", "old": "old a", "new": "new a"},
                        {"path": "b.rs", "old": "old b", "new": "new b"}
                    ]
                }),
                id: None,
            },
        };
        let rows = build(
            &doc("tool", vec![block]),
            80,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows
            .iter()
            .any(|row| row.plain.contains("edit(2 operations)")));
        assert!(rows.iter().any(|row| row.plain.contains("edit(a.rs)")));
        assert!(!rows.iter().any(|row| row.plain.contains("█ OLD")));
        assert!(!rows.iter().any(|row| row.plain.contains("█ NEW")));
    }

    #[test]
    fn shell_result_renders_bash_highlighted_terminal_session() {
        let block = Block::ToolResult {
            ok: true,
            name: Some("shell".into()),
            summary: "shell(if true; then echo \"ok\"; fi)".into(),
            output: "ok".into(),
        };
        let theme = Theme::default();
        let rows = build(
            &doc("tool", vec![block]),
            80,
            &theme,
            &HashSet::new(),
            false,
            false,
        );
        let prompt = rows
            .iter()
            .find(|row| {
                row.plain.trim_start().starts_with("$ if true")
                    && row.background == Some(Color::Black)
            })
            .expect("terminal prompt");
        assert_eq!(prompt.background, Some(Color::Black));
        assert!(prompt.line.spans.iter().any(|span| {
            span.content.as_ref() == "if" && span.style.fg == Some(theme.hl_keyword)
        }));
        assert!(rows
            .iter()
            .any(|row| row.plain.trim() == "│ ok" && row.background == Some(Color::Black)));
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
    fn child_agent_result_marks_running_then_completed() {
        let running = Block::ToolResult {
            ok: true,
            name: Some("agent".into()),
            summary: "agent 1 (\"inspect UI\")".into(),
            output: "[agent-id:1]\n[running]\nsearch(src/ui)".into(),
        };
        let rows = build(
            &doc("tool", vec![running]),
            80,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows.iter().any(|row| row.plain.contains("running")));
        assert!(!rows.iter().any(|row| row.plain.contains("search(src/ui)")));

        let mut toggled = HashSet::new();
        toggled.insert((0, 0));
        let expanded = build(
            &doc(
                "tool",
                vec![Block::ToolResult {
                    ok: true,
                    name: Some("agent".into()),
                    summary: "agent 1 (\"inspect UI\")".into(),
                    output: "[agent-id:1]\n[running]\nsearch(src/ui)".into(),
                }],
            ),
            80,
            &Theme::default(),
            &toggled,
            false,
            false,
        );
        assert!(expanded
            .iter()
            .any(|row| row.plain.contains("search(src/ui)")));

        let completed = Block::ToolResult {
            ok: true,
            name: Some("agent".into()),
            summary: "agent 1 (\"inspect UI\")".into(),
            output: "Found the implementation.".into(),
        };
        let rows = build(
            &doc("tool", vec![completed]),
            80,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows.iter().any(|row| row.plain.contains("completed")));
        assert!(!rows
            .iter()
            .any(|row| row.plain.contains("Found the implementation.")));

        let mut toggled = HashSet::new();
        toggled.insert((0, 0));
        let expanded = build(
            &doc(
                "tool",
                vec![Block::ToolResult {
                    ok: true,
                    name: Some("agent".into()),
                    summary: "agent 1 (\"inspect UI\")".into(),
                    output: "Found the implementation.".into(),
                }],
            ),
            80,
            &Theme::default(),
            &toggled,
            false,
            false,
        );
        assert!(expanded
            .iter()
            .any(|row| row.plain.contains("Found the implementation.")));
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
    fn edit_result_renders_completed_diff_without_call_payload() {
        let block = Block::ToolResult {
            ok: true,
            name: Some("edit".into()),
            summary: "edit(a.rs)".into(),
            output: "Edit a.rs (1 occurrence)\n@@ line 1 @@\n- foo\n+ bar".into(),
        };
        let rows = build(
            &doc("tool", vec![block]),
            40,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows
            .iter()
            .any(|row| row.plain.contains("Edit a.rs (1 occurrence)")));
        let removed = rows
            .iter()
            .find(|row| row.plain.contains("- foo"))
            .expect("removed result line");
        let added = rows
            .iter()
            .find(|row| row.plain.contains("+ bar"))
            .expect("added result line");
        assert!(removed
            .line
            .spans
            .iter()
            .any(|span| span.style.fg == Some(Theme::default().danger)));
        assert!(added
            .line
            .spans
            .iter()
            .any(|span| span.style.fg == Some(Theme::default().success)));
    }

    #[test]
    fn tool_role_has_no_separator() {
        let rows = build(
            &doc("tool", vec![]),
            60,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows.iter().all(|row| row.role_start.is_none()));
        assert!(rows.iter().all(|row| !row.plain.starts_with('─')));
        assert!(rows
            .iter()
            .all(|row| !row.plain.trim_start().starts_with('▌')));
    }

    #[test]
    fn write_result_reports_actual_created_file() {
        let block = Block::ToolResult {
            ok: true,
            name: Some("write".into()),
            summary: "write(a.rs)".into(),
            output: "Created a.rs (12 lines)".into(),
        };
        let rows = build(
            &doc("tool", vec![block]),
            60,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(rows
            .iter()
            .any(|row| row.plain.contains("Created a.rs (12 lines)")));
        assert!(!rows.iter().any(|row| row.toggle.is_some()));
    }

    #[test]
    fn long_edit_result_is_collapsible() {
        let detail = (1..=14)
            .map(|line| format!("+ changed line {}", line))
            .collect::<Vec<_>>()
            .join("\n");
        let block = Block::ToolResult {
            ok: true,
            name: Some("edit".into()),
            summary: "edit(a.rs)".into(),
            output: format!("Edit a.rs (1 occurrence)\n{}", detail),
        };
        let collapsed = build(
            &doc("tool", vec![block.clone()]),
            60,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        assert!(collapsed.iter().any(|row| row.toggle == Some((0, 0))));
        assert!(!collapsed
            .iter()
            .any(|row| row.plain.contains("changed line 14")));

        let mut toggled = HashSet::new();
        toggled.insert((0, 0));
        let expanded = build(
            &doc("tool", vec![block]),
            60,
            &Theme::default(),
            &toggled,
            false,
            false,
        );
        assert!(expanded
            .iter()
            .any(|row| row.plain.contains("changed line 14")));
    }
}
