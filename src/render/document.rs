//! Builds a flat list of screen rows (`RenderedLine`) from parsed message blocks.
//! Each row is exactly one terminal line (text is pre-wrapped to the viewport
//! width), so the chat view can scroll, place a cursor, and virtualize by simple
//! integer indexing. The result is cached by the chat view and only rebuilt when
//! the content, width, or collapse-state changes.

use std::collections::HashSet;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::domain::blocks::Block;
use crate::render::theme::Theme;
use crate::render::wrap::{hard_chunks, wrap_words};

/// One rendered screen row.
#[derive(Clone)]
pub struct RenderedLine {
    pub line: Line<'static>,
    pub plain: String,
    /// Owning message index (for context-aware actions like reply / yank).
    pub msg: usize,
    /// If this row is a collapsible header, the (msg, block) it toggles.
    pub toggle: Option<(usize, usize)>,
}

impl RenderedLine {
    fn new(line: Line<'static>, plain: String, msg: usize) -> Self {
        Self { line, plain, msg, toggle: None }
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

/// Build the full document. `toggled` holds (msg, block) keys the user has
/// explicitly flipped from their default collapse state.
pub fn build(
    messages: &[DocMessage],
    width: usize,
    theme: &Theme,
    toggled: &HashSet<(usize, usize)>,
) -> Vec<RenderedLine> {
    let mut out: Vec<RenderedLine> = Vec::new();
    let inner = width.max(1);

    for (mi, msg) in messages.iter().enumerate() {
        render_role_header(&msg.role, mi, theme, &mut out);

        for (bi, block) in msg.blocks.iter().enumerate() {
            match block {
                Block::Markdown(text) => render_markdown(text, mi, inner, theme, &mut out),
                Block::Code { lang, code } => render_code(lang, code, mi, inner, theme, &mut out),
                Block::Thinking(text) => {
                    render_thinking(text, mi, bi, inner, theme, toggled, &mut out)
                }
                Block::ToolCall(call) => render_tool_call(call, mi, inner, theme, &mut out),
                Block::ToolResult { ok, summary, output } => {
                    render_tool_result(*ok, summary, output, mi, bi, inner, theme, toggled, &mut out)
                }
            }
        }

        // Separator + blank between messages.
        let sep = "─".repeat(inner.min(60));
        out.push(RenderedLine::new(
            Line::from(Span::styled(sep.clone(), Style::default().fg(theme.border))),
            sep,
            mi,
        ));
        out.push(RenderedLine::new(Line::raw(""), String::new(), mi));
    }

    out
}

fn render_role_header(role: &str, mi: usize, theme: &Theme, out: &mut Vec<RenderedLine>) {
    let (label, color) = match role {
        "user" => ("▌ YOU", theme.user),
        "assistant" => ("▌ AI", theme.assistant),
        "system" => ("▌ SYSTEM", theme.system),
        "tool" => ("▌ TOOL", theme.tool),
        _ => ("▌ ?", theme.danger),
    };
    out.push(RenderedLine::new(
        Line::from(Span::styled(label.to_string(), Style::default().fg(color).add_modifier(Modifier::BOLD))),
        label.to_string(),
        mi,
    ));
}

fn render_markdown(text: &str, mi: usize, width: usize, theme: &Theme, out: &mut Vec<RenderedLine>) {
    for raw in text.split('\n') {
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

/// Returns (prefix, remaining-body, base style, is_bullet) for a markdown line.
fn classify_line(raw: &str, theme: &Theme) -> (String, String, Style, bool) {
    if let Some(rest) = raw.strip_prefix("# ") {
        return ("".into(), rest.to_string(), Style::default().fg(theme.warning).add_modifier(Modifier::BOLD | Modifier::UNDERLINED), false);
    }
    if let Some(rest) = raw.strip_prefix("## ").or_else(|| raw.strip_prefix("### ")) {
        return ("".into(), rest.to_string(), Style::default().fg(theme.warning).add_modifier(Modifier::BOLD), false);
    }
    if let Some(rest) = raw.strip_prefix("- ").or_else(|| raw.strip_prefix("* ")) {
        return ("  • ".into(), rest.to_string(), Style::default().fg(theme.text), true);
    }
    if let Some(rest) = raw.strip_prefix("> ") {
        return ("▌ ".into(), rest.to_string(), Style::default().fg(theme.muted), false);
    }
    ("".into(), raw.to_string(), Style::default().fg(theme.text), false)
}

fn render_code(lang: &str, code: &str, mi: usize, width: usize, theme: &Theme, out: &mut Vec<RenderedLine>) {
    let lang_disp = if lang.is_empty() { "code" } else { lang };
    let header = format!("┌─ {} ", lang_disp);
    out.push(RenderedLine::new(
        Line::from(Span::styled(header.clone(), Style::default().fg(theme.faint))),
        header,
        mi,
    ));
    let avail = width.saturating_sub(2).max(1);
    for src in code.split('\n') {
        let chunks = if src.is_empty() { vec![String::new()] } else { hard_chunks(src, avail) };
        for chunk in chunks {
            let plain = format!("│ {}", chunk);
            out.push(RenderedLine::new(
                Line::from(vec![
                    Span::styled("│ ".to_string(), Style::default().fg(theme.faint)),
                    Span::styled(chunk, Style::default().fg(theme.text)),
                ]),
                plain,
                mi,
            ));
        }
    }
    let footer = "└─".to_string();
    out.push(RenderedLine::new(
        Line::from(Span::styled(footer.clone(), Style::default().fg(theme.faint))),
        footer,
        mi,
    ));
}

fn render_thinking(
    text: &str,
    mi: usize,
    bi: usize,
    width: usize,
    theme: &Theme,
    toggled: &HashSet<(usize, usize)>,
    out: &mut Vec<RenderedLine>,
) {
    // Thinking defaults to collapsed.
    let expanded = toggled.contains(&(mi, bi));
    let n = text.lines().count().max(1);
    let arrow = if expanded { "▾" } else { "▸" };
    let header = format!("  {} thinking ({} lines)", arrow, n);
    out.push(
        RenderedLine::new(
            Line::from(Span::styled(
                header.clone(),
                Style::default().fg(theme.thinking).add_modifier(Modifier::ITALIC),
            )),
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
                    Line::from(Span::styled(plain.clone(), Style::default().fg(theme.thinking))),
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
            Span::styled("  ▸ ".to_string(), Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{} ", icon), Style::default().fg(color)),
            Span::styled(call.summary(), Style::default().fg(theme.text).add_modifier(Modifier::BOLD)),
        ]),
        head,
        mi,
    ));
    // For edit_file, preview the diff inline (structural editing, agent-style).
    if call.name == "edit_file" {
        let old = call.args.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
        let new = call.args.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
        render_diff(old, new, mi, width, theme, out);
    }
}

/// Render a minimal line-wise diff (old removed, new added).
fn render_diff(old: &str, new: &str, mi: usize, width: usize, theme: &Theme, out: &mut Vec<RenderedLine>) {
    let avail = width.saturating_sub(4).max(1);
    for l in old.split('\n') {
        for chunk in hard_chunks(l, avail) {
            let plain = format!("    - {}", chunk);
            out.push(RenderedLine::new(
                Line::from(Span::styled(plain.clone(), Style::default().fg(theme.danger))),
                plain,
                mi,
            ));
        }
    }
    for l in new.split('\n') {
        for chunk in hard_chunks(l, avail) {
            let plain = format!("    + {}", chunk);
            out.push(RenderedLine::new(
                Line::from(Span::styled(plain.clone(), Style::default().fg(theme.success))),
                plain,
                mi,
            ));
        }
    }
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
    out: &mut Vec<RenderedLine>,
) {
    let lines: Vec<&str> = output.lines().collect();
    let default_expanded = lines.len() <= 6;
    let expanded = toggled.contains(&(mi, bi)) != default_expanded;

    let icon = if ok { "✓" } else { "✗" };
    let color = if ok { theme.success } else { theme.danger };
    let arrow = if lines.len() > 6 { if expanded { "▾ " } else { "▸ " } } else { "" };
    let header = format!("  {}{} {} ({} lines)", arrow, icon, summary, lines.len());
    let mut row = RenderedLine::new(
        Line::from(Span::styled(header.clone(), Style::default().fg(color).add_modifier(Modifier::BOLD))),
        header,
        mi,
    );
    if lines.len() > 6 {
        row = row.with_toggle((mi, bi));
    }
    out.push(row);

    if expanded {
        let avail = width.saturating_sub(4).max(1);
        for l in &lines {
            for chunk in hard_chunks(l, avail) {
                let plain = format!("  │ {}", chunk);
                out.push(RenderedLine::new(
                    Line::from(Span::styled(plain.clone(), Style::default().fg(theme.muted))),
                    plain,
                    mi,
                ));
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

/// Extract http(s) links from text in order (for link navigation).
pub fn extract_links(text: &str) -> Vec<String> {
    let mut links = Vec::new();
    for token in text.split_whitespace() {
        let t = token.trim_end_matches(|c| matches!(c, '.' | ',' | ')' | ']' | '}' | '>' | '"' | '\''));
        if t.starts_with("http://") || t.starts_with("https://") {
            links.push(t.to_string());
        }
    }
    links
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
        let rows = build(&msgs, 9, &Theme::default(), &HashSet::new());
        // header + wrapped lines + separator + blank
        let texts: Vec<&str> = rows.iter().map(|r| r.plain.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("AI")));
        for r in &rows {
            assert!(unicode_width::UnicodeWidthStr::width(r.plain.as_str()) <= 9 || r.plain.contains("AI"));
        }
    }

    #[test]
    fn thinking_collapsed_by_default_hides_body() {
        let msgs = doc("assistant", vec![Block::Thinking("secret\nreasoning".into())]);
        let rows = build(&msgs, 40, &Theme::default(), &HashSet::new());
        assert!(rows.iter().any(|r| r.plain.contains("thinking (2 lines)")));
        assert!(!rows.iter().any(|r| r.plain.contains("secret")));
        // The header row is a toggle.
        assert!(rows.iter().any(|r| r.toggle.is_some()));
    }

    #[test]
    fn thinking_expands_when_toggled() {
        let msgs = doc("assistant", vec![Block::Thinking("secret".into())]);
        let mut toggled = HashSet::new();
        toggled.insert((0usize, 0usize));
        let rows = build(&msgs, 40, &Theme::default(), &toggled);
        assert!(rows.iter().any(|r| r.plain.contains("secret")));
    }

    #[test]
    fn short_tool_result_shown_long_collapsed() {
        let short = Block::ToolResult { ok: true, summary: "Read a".into(), output: "l1\nl2".into() };
        let rows = build(&doc("tool", vec![short]), 40, &Theme::default(), &HashSet::new());
        assert!(rows.iter().any(|r| r.plain.contains("l1")));

        let long_out = (0..20).map(|i| format!("line{}", i)).collect::<Vec<_>>().join("\n");
        let long = Block::ToolResult { ok: true, summary: "Big".into(), output: long_out };
        let rows = build(&doc("tool", vec![long]), 40, &Theme::default(), &HashSet::new());
        assert!(!rows.iter().any(|r| r.plain.contains("line5")));
        assert!(rows.iter().any(|r| r.toggle.is_some()));
    }

    #[test]
    fn extract_links_works() {
        let links = extract_links("see https://example.com/a, and http://x.io done");
        assert_eq!(links, vec!["https://example.com/a", "http://x.io"]);
    }

    #[test]
    fn edit_file_call_renders_diff() {
        let call = crate::agent::ToolCall {
            name: "edit_file".into(),
            args: serde_json::json!({"path":"a.rs","old_string":"foo","new_string":"bar"}),
            id: None,
        };
        let rows = build(&doc("assistant", vec![Block::ToolCall(call)]), 40, &Theme::default(), &HashSet::new());
        assert!(rows.iter().any(|r| r.plain.contains("- foo")));
        assert!(rows.iter().any(|r| r.plain.contains("+ bar")));
    }
}
