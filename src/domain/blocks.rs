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
    /// A tool the assistant asked to run (parsed from a ```tool fence).
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
}

/// Parse an assistant/user message body into ordered blocks.
///
/// Handles, in a single forward pass that tolerates unclosed markers (so it is
/// safe to call on partial streaming text):
/// - `<think>…</think>` / `<thinking>…</thinking>` → [`Block::Thinking`]
/// - ```` ```tool … ``` ```` → [`Block::ToolCall`] (falls back to code if the
///   JSON does not parse)
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

#[derive(Debug, Clone)]
enum Marker {
    /// Opening tag length + matching close tag.
    Think(usize, &'static str),
    Fence,
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
    consider(find_fence(s), Marker::Fence);
    best
}

/// Find a fenced-code opener (```), preferring one anchored at line start.
///
/// A ` ```tool ` opener is accepted **wherever** it appears, even glued to the
/// end of a prose line (e.g. `…and commands.```tool`). Models frequently emit
/// tool fences mid-line, and without this the scan would skip the real (mid-line)
/// opener and latch onto the line-anchored *closing* fence instead — swallowing
/// the whole tool call into prose and silently running nothing.
fn find_fence(s: &str) -> Option<usize> {
    let mut search_from = 0;
    while let Some(rel) = s[search_from..].find("```") {
        let pos = search_from + rel;
        let is_tool = s[pos + 3..]
            .trim_start_matches([' ', '\t'])
            .starts_with("tool");
        if line_anchored(s, pos) || is_tool {
            return Some(pos);
        }
        search_from = pos + 3;
    }
    // No line-anchored fence; accept the first occurrence if any (robustness).
    s.find("```")
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
    let (inner, consumed) = match find_closing_fence(body, is_tool) {
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

fn find_closing_fence(body: &str, allow_midline: bool) -> Option<usize> {
    let mut from = 0;
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
        body.find("```")
    } else {
        None
    }
}

/// Parse the JSON inside a ```tool fence into a [`ToolCall`], accepting both
/// `args` and `arguments` keys.
fn parse_tool_json(s: &str) -> Option<ToolCall> {
    let v: serde_json::Value = serde_json::from_str(s.trim()).ok()?;
    let name = v.get("name").and_then(|n| n.as_str())?.to_string();
    let args = v
        .get("args")
        .or_else(|| v.get("arguments"))
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    let id = v.get("id").and_then(|i| i.as_str()).map(|s| s.to_string());
    Some(ToolCall { name, args, id })
}

/// Parse a stored tool-result message body of the form
/// `"[tool-result:<name>] <summary> (ok|error)\n<output>"` into a [`Block::ToolResult`].
/// The `:<name>` tag is optional — older stored results use the bare `[tool-result]`
/// header and yield `name = None`.
pub fn parse_tool_result(text: &str) -> Block {
    let first = text.lines().next().unwrap_or("");
    // Recover the optional canonical tool name from `[tool-result:<name>] …`.
    let (name, header) = if let Some(rest) = first.strip_prefix("[tool-result:") {
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
    let output = text.splitn(2, '\n').nth(1).unwrap_or("").to_string();
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

    #[test]
    fn empty_or_whitespace_yields_no_blocks() {
        assert!(parse_blocks("").is_empty());
        assert!(parse_blocks("   \n  \n").is_empty());
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
        let block = parse_tool_result("[tool-result:edit] 🔧 edit(a.rs) (ok)\n- x\n+ y");
        assert_eq!(
            block,
            Block::ToolResult {
                ok: true,
                name: Some("edit".to_string()),
                summary: "🔧 edit(a.rs)".to_string(),
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
