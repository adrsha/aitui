use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::render::document::{wrap_segments, RenderedLine};
use crate::render::highlight::Segment;
use crate::render::theme::Theme;

pub(crate) fn render_search_output(
    output: &str,
    pattern: Option<&str>,
    mi: usize,
    avail: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    for line in output.lines() {
        let segments = search_line_segments(line, pattern, theme);
        for (spans, chunk_plain) in wrap_segments(&segments, avail) {
            let plain = format!("    │ {}", chunk_plain);
            let mut row_spans = vec![Span::styled("    │ ".to_string(), theme.subtle())];
            row_spans.extend(spans);
            out.push(RenderedLine::new(Line::from(row_spans), plain, mi));
        }
    }
}

pub(crate) fn search_pattern_from_summary(summary: &str) -> Option<String> {
    let open = summary.find('(')?;
    let close = summary.rfind(')')?;
    if close <= open + 1 {
        return None;
    }
    Some(
        summary[open + 1..close]
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string(),
    )
    .filter(|s| !s.is_empty() && s != "?")
}

fn search_line_segments(line: &str, pattern: Option<&str>, theme: &Theme) -> Vec<Segment> {
    let base = Style::default().fg(theme.muted);
    if line.starts_with("No matches for ") {
        return highlight_literal(line, pattern, base, search_match_style(theme));
    }
    if line.contains(" match(es) for '") {
        return highlight_literal(
            line,
            pattern,
            Style::default().fg(theme.accent),
            search_match_style(theme),
        );
    }
    if line.trim_start().starts_with('…') {
        return vec![(line.to_string(), Style::default().fg(theme.accent))];
    }

    let Some((path, line_no, body)) = split_search_match(line) else {
        return highlight_literal(line, pattern, base, search_match_style(theme));
    };

    let mut segments = vec![
        (path.to_string(), Style::default().fg(theme.accent)),
        (":".to_string(), base),
        (line_no.to_string(), Style::default().fg(theme.warning)),
        (":".to_string(), base),
    ];
    segments.extend(highlight_literal(
        body,
        pattern,
        Style::default().fg(theme.text),
        search_match_style(theme),
    ));
    segments
}

fn split_search_match(line: &str) -> Option<(&str, &str, &str)> {
    let (path, rest) = line.split_once(':')?;
    let (line_no, body) = rest.split_once(':')?;
    (!path.is_empty() && line_no.chars().all(|c| c.is_ascii_digit())).then_some((path, line_no, body))
}

fn highlight_literal(text: &str, needle: Option<&str>, base: Style, hit: Style) -> Vec<Segment> {
    let Some(needle) = needle.filter(|s| !s.is_empty()) else {
        return vec![(text.to_string(), base)];
    };
    let mut segments = Vec::new();
    let mut start = 0usize;
    while let Some(rel) = text[start..].find(needle) {
        let hit_start = start + rel;
        let hit_end = hit_start + needle.len();
        if hit_start > start {
            segments.push((text[start..hit_start].to_string(), base));
        }
        segments.push((text[hit_start..hit_end].to_string(), hit));
        start = hit_end;
    }
    if start < text.len() {
        segments.push((text[start..].to_string(), base));
    }
    if segments.is_empty() {
        vec![(text.to_string(), base)]
    } else {
        segments
    }
}

fn search_match_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.warning)
        .bg(theme.subtle_pill)
        .add_modifier(Modifier::BOLD)
}
