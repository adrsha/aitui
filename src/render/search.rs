use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use regex::Regex;

use crate::render::document::{wrap_segments, RenderedLine};
use crate::render::highlight::{self, Segment};
use crate::render::path::abbreviate_home;
use crate::render::theme::{fg_guard, Theme};

pub(crate) fn render_search_output(
    output: &str,
    pattern: Option<&str>,
    mi: usize,
    avail: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    let lines: Vec<&str> = output.lines().collect();
    let mut index = 0usize;
    let mut line_idx = 0usize; // alternation counter across all match lines

    while index < lines.len() {
        let line = lines[index];
        let Some((path, _, _)) = split_search_match(line) else {
            push_generic_line(line, pattern, mi, avail, theme, out);
            index += 1;
            continue;
        };

        let path = path.to_string();
        let start = index;
        while index < lines.len()
            && split_search_match(lines[index])
                .map(|(candidate, _, _)| candidate == path)
                .unwrap_or(false)
        {
            index += 1;
        }
        render_file_group(
            &path,
            &lines[start..index],
            pattern,
            mi,
            avail,
            theme,
            &mut line_idx,
            out,
        );
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

fn render_file_group(
    path: &str,
    lines: &[&str],
    pattern: Option<&str>,
    mi: usize,
    avail: usize,
    theme: &Theme,
    line_idx: &mut usize,
    out: &mut Vec<RenderedLine>,
) {
    let display_path = abbreviate_home(path.trim());
    // Separate each file group from the result summary or preceding file. The
    // nested tool indent is applied by the document renderer afterward.
    out.push(RenderedLine::new(Line::default(), String::new(), mi));
    let header = vec![
        Span::styled("█ ", Style::default().fg(theme.accent)),
        Span::styled(
            "FILE",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {}", display_path),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
    ];
    out.push(RenderedLine::new(
        Line::from(header),
        format!("FILE  {}", display_path),
        mi,
    ));

    let number_width = lines
        .iter()
        .filter_map(|line| split_search_match(line).map(|(_, line_no, _)| line_no.len()))
        .max()
        .unwrap_or(1);
    for line in lines {
        let Some((_, line_no, body)) = split_search_match(line) else {
            continue;
        };
        render_match_line(
            &display_path,
            line_no,
            body,
            number_width,
            pattern,
            mi,
            avail,
            theme,
            *line_idx,
            out,
        );
        *line_idx += 1;
    }
}

#[allow(clippy::too_many_arguments)]
fn render_match_line(
    path: &str,
    line_no: &str,
    body: &str,
    number_width: usize,
    pattern: Option<&str>,
    mi: usize,
    avail: usize,
    theme: &Theme,
    idx: usize,
    out: &mut Vec<RenderedLine>,
) {
    let dim = Style::default().fg(theme.muted);
    let base = dim;
    let ln_bg = if idx % 2 == 0 {
        None
    } else {
        Some(Color::DarkGray)
    };
    let prefix_text = format!("{:>width$} ", line_no, width = number_width);
    let ln_style = Style::default()
        .fg(Color::White)
        .bg(ln_bg.unwrap_or(Color::Reset));
    let prefix_width = prefix_text.chars().count();
    let body_width = avail.saturating_sub(prefix_width).max(1);
    let body_segments = highlight::highlight(body, path, theme)
        .and_then(|lines| lines.into_iter().next())
        .unwrap_or_else(|| vec![(body.to_string(), base)]);
    let body_segments =
        highlight_segments_literal(&body_segments, pattern, base, search_match_style(theme));

    for (row_index, (body_spans, body_plain)) in wrap_segments(&body_segments, body_width)
        .into_iter()
        .enumerate()
    {
        let display_prefix = if row_index == 0 {
            prefix_text.clone()
        } else {
            " ".repeat(prefix_width)
        };
        let mut spans = Vec::with_capacity(body_spans.len() + 1);
        spans.push(Span::styled(display_prefix, ln_style));
        spans.extend(body_spans);
        out.push(
            RenderedLine::new(
                Line::from(spans),
                format!("{}{}", prefix_text, body_plain),
                mi,
            )
            .with_background(ln_bg),
        );
    }
}

fn push_generic_line(
    line: &str,
    _pattern: Option<&str>,
    mi: usize,
    avail: usize,
    theme: &Theme,
    out: &mut Vec<RenderedLine>,
) {
    let base = Style::default().fg(theme.muted);
    let segments = if line.starts_with("No matches for ") {
        vec![("  ".to_string(), base)]
    } else if line.contains(" match(es) for '") {
        // "N match(es) for '...' (showing X-Y):" → "N matches" or "N matches (showing X-Y)"
        let count: String = line
            .chars()
            .skip_while(|c| c.is_whitespace())
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let showing = line.find("(showing ").and_then(|i| {
            let rest = &line[i..];
            rest.find(')').map(|j| rest[..=j].to_string())
        });
        let count_style = Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD);
        let mut parts = vec![
            ("  ".to_string(), base),
            (count.clone(), count_style),
            (" matches".to_string(), base),
        ];
        if let Some(ref s) = showing {
            parts.push((" ".to_string(), base));
            parts.push((s.clone(), base));
        }
        parts
    } else if line.trim_start().starts_with('…') {
        let dim = Style::default().fg(Color::DarkGray);
        vec![("  ".to_string(), dim), (line.trim().to_string(), dim)]
    } else {
        vec![("  ".to_string(), base), (abbreviate_home(line), base)]
    };

    for (spans, plain) in wrap_segments(&segments, avail) {
        out.push(RenderedLine::new(Line::from(spans), plain, mi));
    }
}

fn split_search_match(line: &str) -> Option<(&str, &str, &str)> {
    let line = line.trim_start();
    let (path, rest) = line.split_once(':')?;
    let (line_no, body) = rest.split_once(':')?;
    (!path.is_empty() && line_no.chars().all(|c| c.is_ascii_digit())).then_some((
        path,
        line_no,
        body.trim_start(),
    ))
}

fn highlight_segments_literal(
    segments: &[Segment],
    needle: Option<&str>,
    background: Style,
    hit: Style,
) -> Vec<Segment> {
    let with_background = |style: Style| {
        let mut styled = style;
        styled.bg = background.bg;
        styled
    };
    let Some(needle) = needle.filter(|s| !s.is_empty()) else {
        return segments
            .iter()
            .map(|(text, style)| (text.clone(), with_background(*style)))
            .collect();
    };
    let full_text = segments
        .iter()
        .map(|(text, _)| text.as_str())
        .collect::<String>();
    let ranges = match Regex::new(needle) {
        Ok(regex) => regex
            .find_iter(&full_text)
            .filter(|found| !found.is_empty())
            .map(|found| found.range())
            .collect::<Vec<_>>(),
        Err(_) => literal_ranges(&full_text, needle),
    };
    if ranges.is_empty() {
        return segments
            .iter()
            .map(|(text, style)| (text.clone(), with_background(*style)))
            .collect();
    }

    let mut out = Vec::new();
    let mut segment_start = 0usize;
    for (text, style) in segments {
        let segment_end = segment_start + text.len();
        let mut cursor = segment_start;
        for range in ranges
            .iter()
            .filter(|range| range.start < segment_end && range.end > segment_start)
        {
            let hit_start = range.start.max(segment_start);
            let hit_end = range.end.min(segment_end);
            if hit_start > cursor {
                out.push((
                    full_text[cursor..hit_start].to_string(),
                    with_background(*style),
                ));
            }
            if hit_end > hit_start {
                out.push((full_text[hit_start..hit_end].to_string(), hit));
            }
            cursor = cursor.max(hit_end);
        }
        if cursor < segment_end {
            out.push((
                full_text[cursor..segment_end].to_string(),
                with_background(*style),
            ));
        }
        segment_start = segment_end;
    }
    out
}

fn literal_ranges(text: &str, needle: &str) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0usize;
    while let Some(relative) = text[start..].find(needle) {
        let hit_start = start + relative;
        let hit_end = hit_start + needle.len();
        ranges.push(hit_start..hit_end);
        start = hit_end;
    }
    ranges
}

fn search_match_style(theme: &Theme) -> Style {
    Style::default()
        .bg(theme.warning)
        .fg(fg_guard(Color::Black))
        .add_modifier(Modifier::BOLD)
}
