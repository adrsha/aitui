//! Structured content model. A message's raw text is parsed once into an
//! ordered list of `Block`s. Rendering and navigation work off this structured
//! form instead of re-scanning strings every frame, which keeps the UI fast and
//! the renderer simple.

use crate::agent::ToolCall;

/// A semantic chunk of a message.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// Prose / markdown text (inline styling is applied by the renderer).
    Markdown(String),
    /// A fenced code block.
    Code { lang: String, code: String },
    /// Model reasoning ("thinking") — rendered collapsed by default.
    Thinking(String),
    /// A tool the assistant asked to run (parsed from a `<tool>` tag).
    ToolCall(ToolCall),
    /// The result of a tool execution (parsed from a stored "tool" message).
    /// `name` is the canonical tool name (e.g. "edit", "delete") when known, so the
    /// renderer can pick a purpose-built view; `None` for legacy stored results.
    ToolResult {
        ok: bool,
        name: Option<String>,
        summary: String,
        output: String,
    },
    /// A tool result enriched with its original call for file-aware rendering.
    ToolFileResult {
        ok: bool,
        name: Option<String>,
        summary: String,
        output: String,
        call: ToolCall,
    },
}

/// Parse an assistant/user message body into ordered blocks.
///
/// Handles, in a single forward pass that tolerates unclosed markers (so it is
/// safe to call on partial streaming text):
/// - `<think>…</think>` / `<thinking>…</thinking>` → [`Block::Thinking`]
/// - `<tool>…</tool>` (or a legacy ```` ```tool … ``` ```` fence) → [`Block::ToolCall`]
///   (falls back to code if the JSON does not parse)
/// - ```` ```lang … ``` ```` → [`Block::Code`]
/// - everything else → [`Block::Markdown`]
pub fn parse_blocks(text: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut prose = String::new();
    let mut rest = text;

    let flush = |prose: &mut String, blocks: &mut Vec<Block>| {
        let trimmed = prose.trim_matches('\n');
        if !trimmed.trim().is_empty() {
            blocks.push(Block::Markdown(trimmed.to_string()));
        }
        prose.clear();
    };

    while !rest.is_empty() {
        match next_marker(rest) {
            Some((pos, marker)) => {
                prose.push_str(&rest[..pos]);
                rest = &rest[pos..];
                match marker {
                    Marker::Think(open_len, close) => {
                        flush(&mut prose, &mut blocks);
                        let after = &rest[open_len..];
                        match after.find(close) {
                            Some(end) => {
                                let inner = &after[..end];
                                push_thinking(inner, &mut blocks);
                                rest = &after[end + close.len()..];
                            }
                            None => {
                                // Unclosed (streaming): treat remainder as thinking.
                                push_thinking(after, &mut blocks);
                                rest = "";
                            }
                        }
                    }
                    Marker::Fence => {
                        flush(&mut prose, &mut blocks);
                        let (block, consumed) = parse_fence(rest);
                        if let Some(b) = block {
                            blocks.push(b);
                        }
                        rest = &rest[consumed..];
                    }
                    Marker::Tool => {
                        flush(&mut prose, &mut blocks);
                        let (block, consumed) = parse_tool_tag(rest);
                        if let Some(b) = block {
                            blocks.push(b);
                        }
                        rest = &rest[consumed..];
                    }
                }
            }
            None => {
                prose.push_str(rest);
                rest = "";
            }
        }
    }

    flush(&mut prose, &mut blocks);
    blocks
}

fn push_thinking(inner: &str, blocks: &mut Vec<Block>) {
    let trimmed = inner.trim();
    if !trimmed.is_empty() {
        blocks.push(Block::Thinking(trimmed.to_string()));
    }
}

/// The body of the last fenced code block in `text`, if any — used by `:copy-code`
/// to grab the most recent code snippet the assistant produced.
pub fn last_code_block(text: &str) -> Option<String> {
    parse_blocks(text).into_iter().rev().find_map(|b| match b {
        Block::Code { code, .. } => Some(code),
        _ => None,
    })
}

#[derive(Debug, Clone)]
enum Marker {
    /// Opening tag length + matching close tag.
    Think(usize, &'static str),
    Fence,
    /// A `<tool>…</tool>` call.
    Tool,
}

/// Find the earliest content marker in `s`.
fn next_marker(s: &str) -> Option<(usize, Marker)> {
    let mut best: Option<(usize, Marker)> = None;
    let mut consider = |pos: Option<usize>, marker: Marker| {
        if let Some(p) = pos {
            match &best {
                Some((bp, _)) if *bp <= p => {}
                _ => best = Some((p, marker)),
            }
        }
    };
    consider(s.find("<think>"), Marker::Think(7, "</think>"));
    consider(s.find("<thinking>"), Marker::Think(10, "</thinking>"));
    consider(find_tool_tag(s), Marker::Tool);
    consider(find_fence(s), Marker::Fence);
    best
}

/// Find a real `<tool>` opener. Tool protocol tags are block-level markers; an
/// inline prose example such as `mention <tool>{...}</tool> literally` must not
/// become executable or cause the renderer to suppress surrounding Markdown.
fn find_tool_tag(s: &str) -> Option<usize> {
    let marker = crate::agent::parser::TOOL_OPEN;
    let mut search_from = 0;
    while let Some(rel) = s[search_from..].find(marker) {
        let pos = search_from + rel;
        if line_anchored(s, pos) && !inside_inline_code_span(s, pos) {
            return Some(pos);
        }
        search_from = pos + marker.len();
    }
    None
}

/// Parse a `<tool>{json}</tool>` block at the start of `s`. Returns the parsed
/// [`Block::ToolCall`] and bytes consumed. An incomplete or malformed payload (a
/// call still streaming in) becomes a `Block::Code {lang:"tool"}` so the renderer
/// shows its "preparing tool call" placeholder instead of leaking raw JSON.
fn parse_tool_tag(s: &str) -> (Option<Block>, usize) {
    use crate::agent::parser::{json_object_span, TOOL_CLOSE, TOOL_OPEN};
    debug_assert!(s.starts_with(TOOL_OPEN));
    let after = &s[TOOL_OPEN.len()..];
    let body_start = TOOL_OPEN.len() + usize::from(after.starts_with('\n'));
    let body = &s[body_start..];
    // The payload's JSON strings can contain `</tool>` (a `write` echoing this very
    // format), so search for the closer only *after* the balanced object.
    let closer_from = json_object_span(body).map_or(0, |(_, end)| end);
    let (inner, consumed, closed) = match body[closer_from..].find(TOOL_CLOSE) {
        Some(rel) => {
            let end = closer_from + rel;
            let after_close = end + TOOL_CLOSE.len();
            let extra = usize::from(body[after_close..].starts_with('\n'));
            (&body[..end], body_start + after_close + extra, true)
        }
        None => (body, s.len(), false),
    };
    let inner = inner.trim().strip_suffix('\n').unwrap_or(inner.trim());
    if let Some(call) = parse_tool_json(inner) {
        (Some(Block::ToolCall(call)), consumed)
    } else if closed {
        // A complete but non-call tag is documentation/prose, not a partial tool
        // payload. Preserve it visibly rather than manufacturing a hidden tool
        // code block that suppresses the surrounding report.
        (
            Some(Block::Markdown(
                s[..consumed].trim_matches('\n').to_string(),
            )),
            consumed,
        )
    } else {
        // Still streaming / malformed → placeholder code block.
        (
            Some(Block::Code {
                lang: "tool".to_string(),
                code: inner.to_string(),
            }),
            consumed,
        )
    }
}

/// Find a fenced-code opener (```), preferring one anchored at line start.
///
/// A ` ```tool ` opener is accepted **wherever** it appears, even glued to the
/// end of a prose line (e.g. `…and commands.```tool`). Models frequently emit
/// tool fences mid-line, and without this the scan would skip the real (mid-line)
/// opener and latch onto the line-anchored *closing* fence instead — swallowing
/// the whole tool call into prose and silently running nothing.
///
/// Backticks inside an inline-code span are never fences. In particular, Markdown
/// commonly quotes a fence with four-backtick delimiters (` ```` ```tool ```` `);
/// treating the inner ` ```tool ` as an opener creates a phantom tool block and
/// causes the renderer to hide the message's prose.
fn find_fence(s: &str) -> Option<usize> {
    let mut search_from = 0;
    while let Some(rel) = s[search_from..].find("```") {
        let pos = search_from + rel;
        let is_part_of_longer_run = s[..pos].ends_with('`') || s[pos + 3..].starts_with('`');
        let is_tool = s[pos + 3..]
            .trim_start_matches([' ', '\t'])
            .starts_with("tool");
        if !is_part_of_longer_run
            && !inside_inline_code_span(s, pos)
            && (line_anchored(s, pos) || is_tool)
        {
            return Some(pos);
        }
        search_from = pos + 3;
    }
    None
}

/// Whether `pos` falls inside an inline-code span on its current line.
///
/// This intentionally only tracks spans opened before `pos`; the candidate's own
/// run may be a supported mid-line legacy tool fence. Exact run lengths close a
/// span, matching Markdown's backtick-delimiter rules closely enough for fence
/// discovery without requiring a second full Markdown parser.
fn inside_inline_code_span(s: &str, pos: usize) -> bool {
    let line_start = s[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let prefix = &s[line_start..pos];
    let bytes = prefix.as_bytes();
    let mut open_run = None;
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i] == b'`' {
            i += 1;
        }
        let run_len = i - start;
        match open_run {
            Some(open_len) if open_len == run_len => open_run = None,
            None => open_run = Some(run_len),
            _ => {}
        }
    }

    open_run.is_some()
}

/// A fence is "line-anchored" if only whitespace precedes it on its line, so
/// **indented** fences (e.g. a ```bash block nested under a list item) are still
/// recognised — not rendered as literal prose.
fn line_anchored(s: &str, pos: usize) -> bool {
    let line_start = s[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    s[line_start..pos].chars().all(|c| c == ' ' || c == '\t')
}

/// Parse a fenced block starting at `s` (which begins with ```). Returns the
/// block and how many bytes were consumed (including the closing fence).
fn parse_fence(s: &str) -> (Option<Block>, usize) {
    debug_assert!(s.starts_with("```"));
    let after_ticks = &s[3..];
    // The info string (lang + attrs) normally runs to the end of the opener line,
    // with the body starting on the next line. But models also emit fences inline
    // on a single line (`…issues.```tool {json}``` more`); there the lang is just
    // the first token and the body is whatever follows it on the same line.
    let (lang, body_start_rel) = match after_ticks.find('\n') {
        Some(nl) => {
            let info = after_ticks[..nl].trim();
            let lang = info.split_whitespace().next().unwrap_or("").to_string();
            (lang, 3 + nl + 1)
        }
        None => {
            let lang_end = after_ticks
                .find(char::is_whitespace)
                .unwrap_or(after_ticks.len());
            let lang = after_ticks[..lang_end].trim().to_string();
            // Skip a single separating space so the body begins at the payload.
            let ws = after_ticks[lang_end..].len() - after_ticks[lang_end..].trim_start().len();
            (lang, 3 + lang_end + ws)
        }
    };
    let body = &s[body_start_rel..];
    let is_tool = lang == "tool";

    // Closing fence: ``` at a line start (or end of string when streaming). For a
    // tool fence we also accept a mid-line closer so a call emitted entirely on
    // one line still closes cleanly instead of parsing as prose.
    // A tool payload's JSON strings can contain ``` (an `edit` on markdown, say), so
    // start the closer search after the balanced object rather than at the first ```
    // — otherwise the block is cut mid-string and the call parses as dead code.
    let closer_from = if is_tool {
        crate::agent::parser::json_object_span(body)
            .map(|(_, end)| end)
            .unwrap_or(0)
    } else {
        0
    };
    let (inner, consumed) = match find_closing_fence(body, is_tool, closer_from) {
        Some(end) => {
            let after_close = end + 3;
            // Swallow a trailing newline after the close fence.
            let extra = if body[after_close..].starts_with('\n') {
                1
            } else {
                0
            };
            (&body[..end], body_start_rel + after_close + extra)
        }
        None => (body, s.len()),
    };
    let inner = inner.strip_suffix('\n').unwrap_or(inner);

    if is_tool {
        if let Some(call) = parse_tool_json(inner) {
            return (Some(Block::ToolCall(call)), consumed);
        }
        // Fall through to a code block if the JSON is malformed.
    }
    (
        Some(Block::Code {
            lang,
            code: inner.to_string(),
        }),
        consumed,
    )
}

/// Find the closing fence in `body`, scanning from byte offset `from` (callers pass
/// a non-zero `from` to skip a payload whose own content contains backticks).
fn find_closing_fence(body: &str, allow_midline: bool, from: usize) -> Option<usize> {
    let start = from.min(body.len());
    let mut from = start;
    while let Some(rel) = body[from..].find("```") {
        let pos = from + rel;
        if line_anchored(body, pos) {
            return Some(pos);
        }
        from = pos + 3;
    }
    // No line-anchored closer. For tool fences, fall back to the first ``` anywhere
    // so a call emitted entirely on one line (`…issues.```tool {json}``` more`)
    // still closes instead of dragging trailing prose into the block. Regular code
    // blocks keep the strict rule so streaming/partial content isn't cut short.
    if allow_midline {
        body[start..].find("```").map(|rel| start + rel)
    } else {
        None
    }
}

/// Parse the JSON inside a ```tool fence into a [`ToolCall`], accepting both
/// `args` and `arguments` keys.
/// Delegate to the one canonical tool-JSON parser so the block parser (which gates
/// execution) accepts exactly what the stream-cut parser accepts — never stricter,
/// or a cut fence would render as a dead code block instead of running.
fn parse_tool_json(s: &str) -> Option<ToolCall> {
    crate::agent::parser::parse_tool_json(s)
}

/// Parse a stored tool-result message body. Current messages use
/// `"[tool-result:<name>] <summary>\n<output>"`; legacy messages may use
/// `[tool:<name>]`, a bare `[tool-result]`, and/or append `(ok|error)` to the
/// summary. These storage markers are consumed here and never shown by the UI.
pub fn parse_tool_result(text: &str) -> Block {
    let first = text.lines().next().unwrap_or("");
    // Recover the optional canonical tool name from the private storage marker.
    let (name, header) = if let Some(rest) = first
        .strip_prefix("[tool-result:")
        .or_else(|| first.strip_prefix("[tool:"))
    {
        match rest.split_once("] ") {
            Some((n, h)) => (Some(n.to_string()), h),
            None => (None, first),
        }
    } else {
        (None, first.strip_prefix("[tool-result] ").unwrap_or(first))
    };
    let ok = !header.ends_with("(error)");
    let summary = header
        .trim_end_matches("(ok)")
        .trim_end_matches("(error)")
        .trim()
        .to_string();
    let output = text.split_once('\n').map(|x| x.1).unwrap_or("").to_string();
    Block::ToolResult {
        ok,
        name,
        summary,
        output,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_one_markdown_block() {
        let blocks = parse_blocks("hello world");
        assert_eq!(blocks, vec![Block::Markdown("hello world".to_string())]);
    }

    /// The block parser gates rendering and must agree with the execution parser:
    /// a payload carrying ``` (an `edit` on markdown) has to stay a ToolCall, not
    /// degrade into a dead code block.
    #[test]
    fn tool_fence_survives_backticks_inside_its_payload() {
        let text = "```tool\n{\"name\":\"edit\",\"args\":{\"path\":\"a.md\",\"old\":\"see ```rust\\nfoo\\n```\",\"new\":\"x\"}}\n```\nafter";
        let blocks = parse_blocks(text);
        match &blocks[0] {
            Block::ToolCall(call) => {
                assert_eq!(call.name, "edit");
                assert_eq!(call.args["old"], "see ```rust\nfoo\n```");
            }
            other => panic!("expected ToolCall, got {:?}", other),
        }
        assert_eq!(blocks[1], Block::Markdown("after".to_string()));
    }

    #[test]
    fn empty_or_whitespace_yields_no_blocks() {
        assert!(parse_blocks("").is_empty());
        assert!(parse_blocks("   \n  \n").is_empty());
    }

    /// Collect the tool calls `parse_blocks` (the execution gate) finds in `text`.
    fn block_tool_calls(text: &str) -> Vec<crate::agent::tools::ToolCall> {
        parse_blocks(text)
            .into_iter()
            .filter_map(|b| match b {
                Block::ToolCall(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn unclosed_tool_fence_is_still_a_runnable_toolcall() {
        // Regression: the stream-cut parser recovers an unclosed fence, so the block
        // parser (execution gate) must too — otherwise the cut fires but nothing runs
        // and the fence renders as dead text.
        let text = "sure:\n```tool\n{\"name\":\"list\",\"args\":{\"path\":\".\"}}";
        let calls = block_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "list");
    }

    #[test]
    fn tool_fence_with_trailing_prose_still_runs() {
        let text = "```tool\n{\"name\":\"read\",\"args\":{\"path\":\"a.rs\"}}\nnow reading it";
        let calls = block_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read");
    }

    #[test]
    fn leaked_think_then_tool_block_runs() {
        // The exact failing shape: leaked <think> CoT followed by a tool fence whose
        // JSON has fields in a non-standard order (name last).
        let text = "<think>deciding</think>\n\n```tool\n{\"args\":{\"path\":\".\"},\"id\":\"c1\",\"name\":\"list\"}\n```";
        let calls = block_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "list");
        assert_eq!(calls[0].id.as_deref(), Some("c1"));
    }

    #[test]
    fn block_parser_agrees_with_stream_cut_parser() {
        // The two parsers must never disagree about whether a fence is runnable.
        for text in [
            "```tool\n{\"name\":\"list\",\"args\":{\"path\":\".\"}}\n```",
            "```tool\n{\"name\":\"list\",\"args\":{\"path\":\".\"}}", // unclosed
            "prose ```tool {\"name\":\"read\",\"args\":{\"path\":\"x\"}} ``` more",
            "<think>x</think>\n```tool\n{\"name\":\"shell\",\"args\":{\"command\":\"ls\"}} trailing",
        ] {
            let via_blocks = block_tool_calls(text).len();
            let via_stream = crate::agent::parser::extract_tool_calls(text).len();
            assert_eq!(via_blocks, via_stream, "parsers disagree on: {text:?}");
            assert_eq!(via_blocks, 1, "expected one call in: {text:?}");
        }
    }

    #[test]
    fn last_code_block_returns_final_snippet() {
        let text = "intro\n```py\nprint(1)\n```\nmid\n```rust\nfn main() {}\n```\nend";
        assert_eq!(last_code_block(text).as_deref(), Some("fn main() {}"));
        // No fenced code → None.
        assert_eq!(last_code_block("just prose, `inline` only"), None);
        assert_eq!(last_code_block(""), None);
    }

    #[test]
    fn code_block_is_extracted_with_lang() {
        let blocks = parse_blocks("before\n```rust\nfn main() {}\n```\nafter");
        assert_eq!(
            blocks,
            vec![
                Block::Markdown("before".to_string()),
                Block::Code {
                    lang: "rust".to_string(),
                    code: "fn main() {}".to_string()
                },
                Block::Markdown("after".to_string()),
            ]
        );
    }

    #[test]
    fn code_block_without_lang() {
        let blocks = parse_blocks("```\nplain\n```");
        assert_eq!(
            blocks,
            vec![Block::Code {
                lang: "".to_string(),
                code: "plain".to_string()
            }]
        );
    }

    #[test]
    fn unclosed_code_block_streams_gracefully() {
        let blocks = parse_blocks("text\n```python\nprint(1)");
        assert_eq!(
            blocks,
            vec![
                Block::Markdown("text".to_string()),
                Block::Code {
                    lang: "python".to_string(),
                    code: "print(1)".to_string()
                },
            ]
        );
    }

    #[test]
    fn tool_tag_becomes_tool_call_block() {
        let blocks = parse_blocks(
            "Sure.\n<tool>\n{\"name\":\"list\",\"args\":{\"path\":\".\"}}\n</tool>\ndone",
        );
        assert!(matches!(blocks.first(), Some(Block::Markdown(t)) if t == "Sure."));
        assert!(
            matches!(blocks.get(1), Some(Block::ToolCall(c)) if c.name == "list"),
            "expected a ToolCall block, got {blocks:?}"
        );
        assert!(matches!(blocks.get(2), Some(Block::Markdown(t)) if t == "done"));
    }

    /// The rendering path must not choke on a `write` whose content holds a ```` ``` ````
    /// fence — the exact case that silently dropped the call under ```` ```tool ````.
    #[test]
    fn tool_tag_with_fence_in_content_is_a_tool_call_not_dead_code() {
        let text = "<tool>\n{\"name\":\"write\",\"args\":{\"path\":\"a.md\",\"content\":\"see ```json\\n[1]\\n```\"}}\n</tool>";
        let blocks = parse_blocks(text);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::ToolCall(c) => {
                assert_eq!(c.name, "write");
                assert!(c.args["content"].as_str().unwrap().contains("```json"));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn literal_inline_tool_tag_stays_markdown() {
        let text = "Prose mentioning <tool>{...}</tool> literally stays visible.";
        assert_eq!(parse_blocks(text), vec![Block::Markdown(text.to_string())]);
        assert!(block_tool_calls(text).is_empty());
    }

    #[test]
    fn closed_malformed_block_tool_tag_stays_markdown() {
        let text = "Before\n<tool>{...}</tool>\nAfter";
        let blocks = parse_blocks(text);
        assert!(block_tool_calls(text).is_empty());
        assert!(blocks
            .iter()
            .all(|block| matches!(block, Block::Markdown(_))));
        let visible = blocks
            .iter()
            .filter_map(|block| match block {
                Block::Markdown(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(visible.contains("Before"));
        assert!(visible.contains("<tool>{...}</tool>"));
        assert!(visible.contains("After"));
    }

    #[test]
    fn inline_code_tool_tag_stays_markdown() {
        let text = "Use `<tool>{\"name\":\"read\"}</tool>` only as documentation.";
        assert_eq!(parse_blocks(text), vec![Block::Markdown(text.to_string())]);
        assert!(block_tool_calls(text).is_empty());
    }

    #[test]
    fn inline_code_mention_of_tool_fence_stays_markdown() {
        let text = "Legacy ```` ```tool ```` blocks remain supported.";
        assert_eq!(parse_blocks(text), vec![Block::Markdown(text.to_string())]);
        assert!(block_tool_calls(text).is_empty());
    }

    #[test]
    fn repeated_inline_tool_fence_mentions_do_not_mangle_following_fences() {
        let text = concat!(
            "## Agent loop summary\n\n",
            "2. Legacy ```` ```tool ```` blocks remain supported.\n",
            "3. Cached reads use:\n",
            "```text\n[cached: file unchanged since last read]\n```\n",
            "when an unchanged file is served from cache.\n",
            "5. Another ```` ```tool ```` mention remains prose."
        );
        let blocks = parse_blocks(text);

        assert!(
            block_tool_calls(text).is_empty(),
            "unexpected tool call: {blocks:?}"
        );
        assert!(!blocks
            .iter()
            .any(|block| matches!(block, Block::Code { lang, .. } if lang == "tool")));
        assert!(
            matches!(
                blocks.as_slice(),
                [Block::Markdown(before), Block::Code { lang, code }, Block::Markdown(after)]
                    if before.contains("Legacy ```` ```tool ```` blocks")
                        && lang == "text"
                        && code == "[cached: file unchanged since last read]"
                        && after.contains("when an unchanged file is served from cache")
                        && after.contains("Another ```` ```tool ```` mention")
            ),
            "unexpected blocks: {blocks:?}"
        );
    }

    #[test]
    fn inline_code_span_with_triple_backtick_text_stays_markdown() {
        let text = "Mention ` ```tool ` without making it executable.";
        assert_eq!(parse_blocks(text), vec![Block::Markdown(text.to_string())]);
    }

    #[test]
    fn legacy_tool_fence_still_becomes_tool_call_block() {
        let blocks = parse_blocks("```tool\n{\"name\":\"read\",\"args\":{\"path\":\"a.rs\"}}\n```");
        assert!(matches!(blocks.first(), Some(Block::ToolCall(c)) if c.name == "read"));
    }

    #[test]
    fn think_tag_becomes_thinking_block() {
        let blocks = parse_blocks("<think>reasoning here</think>answer");
        assert_eq!(
            blocks,
            vec![
                Block::Thinking("reasoning here".to_string()),
                Block::Markdown("answer".to_string()),
            ]
        );
    }

    #[test]
    fn unclosed_think_tag_streams() {
        let blocks = parse_blocks("<think>still thinking...");
        assert_eq!(
            blocks,
            vec![Block::Thinking("still thinking...".to_string())]
        );
    }

    #[test]
    fn thinking_variant_tag() {
        let blocks = parse_blocks("<thinking>hmm</thinking>done");
        assert_eq!(
            blocks,
            vec![
                Block::Thinking("hmm".to_string()),
                Block::Markdown("done".to_string()),
            ]
        );
    }

    #[test]
    fn tool_fence_becomes_tool_call() {
        let text =
            "I will read it\n```tool\n{\"name\":\"read_file\",\"args\":{\"path\":\"a.rs\"}}\n```";
        let blocks = parse_blocks(text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], Block::Markdown("I will read it".to_string()));
        match &blocks[1] {
            Block::ToolCall(c) => {
                assert_eq!(c.name, "read_file");
                assert_eq!(c.args.get("path").unwrap().as_str(), Some("a.rs"));
            }
            other => panic!("expected ToolCall, got {:?}", other),
        }
    }

    #[test]
    fn tool_fence_glued_to_prose_line_still_parses() {
        // Model glued the opener to the end of a prose line (not line-anchored),
        // with a line-anchored closing fence after. Must still yield a ToolCall,
        // not swallow it into prose + an empty code block.
        let text = "…verify the docs match the code and commands.```tool\n{\"name\":\"run_shell\",\"args\":{\"command\":\"find . -name '*.md' | sort\"}}\n```";
        let blocks = parse_blocks(text);
        let call = blocks.iter().find_map(|b| match b {
            Block::ToolCall(c) => Some(c),
            _ => None,
        });
        assert!(call.is_some(), "expected a ToolCall, got {:?}", blocks);
        assert_eq!(call.unwrap().name, "run_shell");
    }

    #[test]
    fn tool_fence_fully_inline_still_parses() {
        // Opener and closer both glued on one line, with trailing prose after.
        let text = "…concrete issues.```tool {\"name\":\"read_file\",\"args\":{\"path\":\"Cargo.toml\"}}``` I need the result.";
        let blocks = parse_blocks(text);
        let call = blocks.iter().find_map(|b| match b {
            Block::ToolCall(c) => Some(c),
            _ => None,
        });
        assert!(call.is_some(), "expected a ToolCall, got {:?}", blocks);
        assert_eq!(call.unwrap().name, "read_file");
        // Trailing prose after the closer is preserved.
        assert!(blocks
            .iter()
            .any(|b| matches!(b, Block::Markdown(m) if m.contains("I need the result"))));
    }

    #[test]
    fn indented_code_fence_is_recognised() {
        // A ```bash block indented under a list item must parse as a Code block,
        // not literal prose.
        let text = "1. Run this:\n   ```bash\n   find . -type f\n   ```\ndone";
        let blocks = parse_blocks(text);
        let code = blocks.iter().find_map(|b| match b {
            Block::Code { lang, code } => Some((lang.clone(), code.clone())),
            _ => None,
        });
        assert!(
            code.is_some(),
            "indented fence should be a Code block, got {:?}",
            blocks
        );
        assert_eq!(code.as_ref().unwrap().0, "bash");
        assert!(code.unwrap().1.contains("find"));
    }

    #[test]
    fn tool_fence_accepts_arguments_key() {
        let text = "```tool\n{\"name\":\"run_shell\",\"arguments\":{\"command\":\"ls\"}}\n```";
        let blocks = parse_blocks(text);
        match &blocks[0] {
            Block::ToolCall(c) => assert_eq!(c.name, "run_shell"),
            other => panic!("expected ToolCall, got {:?}", other),
        }
    }

    #[test]
    fn malformed_tool_fence_falls_back_to_code() {
        let blocks = parse_blocks("```tool\nnot json\n```");
        assert_eq!(
            blocks,
            vec![Block::Code {
                lang: "tool".to_string(),
                code: "not json".to_string()
            }]
        );
    }

    #[test]
    fn interleaved_think_code_prose() {
        let text = "<think>plan</think>Here:\n```sh\nls\n```\nDone.";
        let blocks = parse_blocks(text);
        assert_eq!(
            blocks,
            vec![
                Block::Thinking("plan".to_string()),
                Block::Markdown("Here:".to_string()),
                Block::Code {
                    lang: "sh".to_string(),
                    code: "ls".to_string()
                },
                Block::Markdown("Done.".to_string()),
            ]
        );
    }

    #[test]
    fn tool_result_parsing_ok() {
        // Bare (legacy) header → name is None.
        let block = parse_tool_result("[tool-result] Read a.rs (ok)\nfile contents\nline2");
        assert_eq!(
            block,
            Block::ToolResult {
                ok: true,
                name: None,
                summary: "Read a.rs".to_string(),
                output: "file contents\nline2".to_string(),
            }
        );
    }

    #[test]
    fn legacy_tool_marker_is_consumed_and_never_becomes_display_text() {
        let block = parse_tool_result("[tool:read] read(src/main.rs) (ok)\nfn main() {}");
        assert_eq!(
            block,
            Block::ToolResult {
                ok: true,
                name: Some("read".to_string()),
                summary: "read(src/main.rs)".to_string(),
                output: "fn main() {}".to_string(),
            }
        );
    }

    #[test]
    fn tool_result_parsing_error() {
        let block = parse_tool_result("[tool-result] Shell foo (error)\nboom");
        assert_eq!(
            block,
            Block::ToolResult {
                ok: false,
                name: None,
                summary: "Shell foo".to_string(),
                output: "boom".to_string()
            }
        );
    }

    #[test]
    fn tool_result_parsing_extracts_name() {
        // New header form carries the canonical tool name for purpose-built rendering.
        let block = parse_tool_result("[tool-result:edit] tool edit(a.rs) (ok)\n- x\n+ y");
        assert_eq!(
            block,
            Block::ToolResult {
                ok: true,
                name: Some("edit".to_string()),
                summary: "tool edit(a.rs)".to_string(),
                output: "- x\n+ y".to_string(),
            }
        );
    }

    #[test]
    fn code_fence_with_inner_triple_backtick_in_prose_not_confused() {
        // A fence that contains text but closes properly.
        let blocks = parse_blocks("```\na\nb\n```");
        assert_eq!(
            blocks,
            vec![Block::Code {
                lang: "".into(),
                code: "a\nb".into()
            }]
        );
    }
}
