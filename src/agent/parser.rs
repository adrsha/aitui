// `StreamingParser` and `strip_tool_blocks` are kept for live in-stream tool
// parsing; the app currently extracts tool calls once a turn completes.
#![allow(dead_code)]

use super::tools::ToolCall;

/// Delimiters that wrap a tool call in the model's reply. `<tool>…</tool>` is the
/// current format: unlike the old ```` ```tool ```` fence it cannot be closed early
/// by a code fence inside the call's own JSON (a `write` whose content holds
/// ```` ``` ````), which used to make the parser silently drop the call.
pub const TOOL_OPEN: &str = "<tool>";
pub const TOOL_CLOSE: &str = "</tool>";
/// Legacy fence opener, still accepted when reading older sessions/replies.
const LEGACY_OPEN: &str = "```tool";

/// Find the earliest tool-call opener (new `<tool>` tag or legacy ```` ```tool ````
/// fence) in `s`. Returns its byte position, the byte offset where the JSON payload
/// begins (past the opener and any single separating newline), and whether it was
/// the legacy fence (so the caller knows a trailing ```` ``` ```` may need skipping).
fn find_tool_open(s: &str) -> Option<(usize, usize, bool)> {
    let tag = find_block_tool_tag(s).map(|p| (p, p + TOOL_OPEN.len(), false));
    let fence = find_legacy_tool_fence(s).map(|p| {
        let after = &s[p + LEGACY_OPEN.len()..];
        let skip = usize::from(after.starts_with('\n'));
        (p, p + LEGACY_OPEN.len() + skip, true)
    });
    match (tag, fence) {
        (Some(t), Some(f)) => Some(if t.0 <= f.0 { t } else { f }),
        (Some(t), None) => Some(t),
        (None, f) => f,
    }
}

fn find_block_tool_tag(s: &str) -> Option<usize> {
    let mut search_from = 0;
    while let Some(rel) = s[search_from..].find(TOOL_OPEN) {
        let pos = search_from + rel;
        if line_anchored(s, pos) && !inside_inline_code_span(s, pos) {
            return Some(pos);
        }
        search_from = pos + TOOL_OPEN.len();
    }
    None
}

fn find_legacy_tool_fence(s: &str) -> Option<usize> {
    let mut search_from = 0;
    while let Some(rel) = s[search_from..].find(LEGACY_OPEN) {
        let pos = search_from + rel;
        let longer_run = s[..pos].ends_with('`');
        if !longer_run && !inside_inline_code_span(s, pos) {
            return Some(pos);
        }
        search_from = pos + LEGACY_OPEN.len();
    }
    None
}

fn line_anchored(s: &str, pos: usize) -> bool {
    let line_start = s[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    s[line_start..pos].chars().all(|c| c == ' ' || c == '\t')
}

fn inside_inline_code_span(s: &str, pos: usize) -> bool {
    let line_start = s[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let bytes = &s.as_bytes()[line_start..pos];
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

/// State machine that watches streaming text and extracts ```tool ... ``` blocks.
/// As text streams in token by token, feed it to `push()`.
/// When `take_completed()` returns Some(ToolCall), a complete call is ready.
#[derive(Debug, Default)]
pub struct StreamingParser {
    buffer: String,
    in_tool_block: bool,
    tool_content: String,
}

impl StreamingParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a token from the stream. Returns text that should be displayed
    /// (non-tool content) and whether any tool calls were completed.
    pub fn push(&mut self, token: &str) -> (String, Vec<ToolCall>) {
        self.buffer.push_str(token);
        let mut display_text = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();

        loop {
            if self.in_tool_block {
                // Looking for closing </tool>
                if let Some(end_pos) = self.buffer.find(TOOL_CLOSE) {
                    let json_str = self.buffer[..end_pos].trim().to_string();
                    self.buffer = self.buffer[end_pos + TOOL_CLOSE.len()..].to_string();
                    self.tool_content.push_str(&json_str);
                    self.in_tool_block = false;

                    if let Some(call) = parse_tool_json(&self.tool_content) {
                        calls.push(call);
                    }
                    self.tool_content.clear();
                } else {
                    // Not complete yet, keep buffering (don't display tool internals)
                    break;
                }
            } else {
                // Looking for <tool> opening
                if let Some(start_pos) = self.buffer.find(TOOL_OPEN) {
                    // Everything before the marker goes to display
                    display_text.push_str(&self.buffer[..start_pos]);
                    let after_marker = &self.buffer[start_pos + TOOL_OPEN.len()..];
                    // Skip a single newline after <tool>
                    let content_start = if after_marker.starts_with('\n') { 1 } else { 0 };
                    self.buffer = after_marker[content_start..].to_string();
                    self.in_tool_block = true;
                    self.tool_content.clear();
                } else {
                    // No tool block found — but we might be in the middle of receiving "```tool"
                    // Only flush text that can't be the start of a marker
                    let safe_end = safe_flush_point(&self.buffer);
                    if safe_end > 0 {
                        display_text.push_str(&self.buffer[..safe_end]);
                        self.buffer = self.buffer[safe_end..].to_string();
                    }
                    break;
                }
            }
        }

        (display_text, calls)
    }

    /// Flush remaining buffered text when the stream ends.
    pub fn flush(&mut self) -> String {
        let remaining = self.buffer.clone();
        self.buffer.clear();
        self.tool_content.clear();
        self.in_tool_block = false;
        remaining
    }
}

/// Find the safe point up to which we can flush buffered text
/// (i.e. not in the middle of a potential `<tool>` marker).
fn safe_flush_point(buf: &str) -> usize {
    // If the buffer ends with a partial `<tool>` opener, hold it back.
    // Guard every cut with `is_char_boundary`: a buffer ending in a multi-byte
    // char (emoji / CJK / accents) would otherwise panic on the slice below,
    // crashing mid-stream. The marker `"<tool>"` is pure ASCII, so a real
    // partial match can only start on a char boundary anyway.
    for suffix_len in (1..=TOOL_OPEN.len().min(buf.len())).rev() {
        let cut = buf.len() - suffix_len;
        if !buf.is_char_boundary(cut) {
            continue;
        }
        let suffix = &buf[cut..];
        if TOOL_OPEN.starts_with(suffix) {
            return cut;
        }
    }
    buf.len()
}

/// Parse a `name`/`args` tool-call object out of `s`, tolerating trailing prose or
/// a missing closing fence by falling back to the first balanced `{...}` object.
///
/// This is the single canonical tool-JSON parser: the stream-cut decision
/// (`extract_tool_calls`) and the execution decision (`domain::blocks`) both go
/// through it, so they can never disagree about whether a fence is a runnable call.
pub fn parse_tool_json(s: &str) -> Option<ToolCall> {
    // Try strict parse of the whole string first.
    if let Some(call) = parse_tool_value(s) {
        return Some(call);
    }
    // The string may carry trailing prose (an unclosed block that the stream ran
    // past, or a model that kept talking after the JSON). Fall back to the first
    // balanced `{...}` object and parse that.
    let obj = extract_json_object(s)?;
    parse_tool_value(obj)
}

fn parse_tool_value(s: &str) -> Option<ToolCall> {
    let v = serde_json::from_str::<serde_json::Value>(s.trim()).ok()?;
    let name = v
        .get("name")
        .and_then(|n| n.as_str())
        .map(|s| s.to_string())?;
    let args = v
        .get("args")
        .or_else(|| v.get("arguments"))
        .cloned()
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
    let id = v.get("id").and_then(|i| i.as_str()).map(|s| s.to_string());
    Some(ToolCall { name, args, id })
}

/// Return the first balanced top-level `{...}` substring of `s`, tracking string
/// literals and escapes so a brace inside a JSON string doesn't fool the counter.
/// Used to recover a tool call from text that has an unclosed fence or trailing
/// prose after the object.
fn extract_json_object(s: &str) -> Option<&str> {
    let (start, end) = json_object_span(s)?;
    Some(&s[start..end])
}

/// Byte span (`start..end`) of the first balanced top-level `{...}` object in `s`,
/// tracking string literals and escapes. Callers use the end offset to resume
/// scanning *after* the payload, which matters because a tool call's own JSON
/// strings may contain ``` (an `edit` whose old/new is markdown, say) and the
/// first ``` therefore is not necessarily the fence's closer.
pub fn json_object_span(s: &str) -> Option<(usize, usize)> {
    let bytes = s.as_bytes();
    let start = s.find('{')?;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((start, i + 1));
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse tool calls from a completed (non-streaming) response text.
/// Used when reviewing a whole assistant message.
pub fn extract_tool_calls(text: &str) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    let mut remaining = text;

    while let Some((_, content_start, legacy)) = find_tool_open(remaining) {
        let content = &remaining[content_start..];
        // Take the balanced JSON object first: the payload's own strings may hold the
        // closing delimiter (a `write` whose content contains ``` or `</tool>`), and
        // cutting at the first one would truncate the JSON mid-string and silently
        // drop the whole call. Resuming after the object also skips the closer, so the
        // next opener is found where it should be.
        if let Some((obj_start, obj_end)) = json_object_span(content) {
            if let Some(call) = parse_tool_value(&content[obj_start..obj_end]) {
                calls.push(call);
                remaining = &content[obj_end..];
                continue;
            }
        }
        // No parseable object (malformed, or still streaming) — fall back to the
        // delimited slice up to the matching closer.
        let closer = if legacy { "```" } else { TOOL_CLOSE };
        if let Some(end) = content.find(closer) {
            if let Some(call) = parse_tool_json(content[..end].trim()) {
                calls.push(call);
            }
            remaining = &content[end + closer.len()..];
        } else {
            // Unclosed block — the stream was cut mid-call. Recover from whatever
            // trailing content we have rather than dropping it silently.
            if let Some(call) = parse_tool_json(content) {
                calls.push(call);
            }
            break;
        }
    }

    calls
}

/// Tool calls in the *visible* part of `text` — fences inside reasoning
/// (`<think>…</think>`) are excluded.
///
/// A model working out loud sketches calls it then never makes, so a fence in
/// reasoning is a draft, not a commitment. Use this for any decision taken while
/// the reply is still streaming (cutting the stream, speculating a read): mid-turn
/// there is no way to tell a draft from a reply, so only visible content counts.
pub fn visible_tool_calls(text: &str) -> Vec<ToolCall> {
    extract_tool_calls(&strip_think_blocks(text))
}

/// The tool calls a finished message committed to.
///
/// Visible fences win. The fallback to scanning reasoning covers an endpoint that
/// routes the *whole* reply through its reasoning channel and emits no visible
/// fence at all: there the reasoning text is the reply. That fallback is only safe
/// once the turn is over — mid-stream, "no visible fence yet" just means the model
/// is still thinking. Use [`visible_tool_calls`] there instead.
pub fn committed_tool_calls(text: &str) -> Vec<ToolCall> {
    let calls = visible_tool_calls(text);
    if !calls.is_empty() {
        return calls;
    }
    extract_tool_calls(text)
}

/// Reasoning open tags and their matching closers.
const THINK_TAGS: [(&str, &str); 2] = [("<think>", "</think>"), ("<thinking>", "</thinking>")];

/// Completed tool calls inside *closed* `<think>…</think>` / `<thinking>…</thinking>`
/// spans, when the model has produced nothing but that thinking so far.
///
/// Covers the interleaved-thinking reply pattern (Claude Code style): the model
/// writes its deliberation as visible thinking tags in the content channel, emits
/// its tool call inside the block, closes the block, and stops for the harness to
/// run the call. Once `</think>`/`</thinking>` has arrived the deliberation is over
/// and the call is a commitment, not a sketch — so the stream may be cut on it.
///
/// Deliberately distinct from the reasoning *channel*: a fence streamed through
/// `reasoning_content` stays a draft (the model may still discard it) and is never
/// cut on. An unclosed opener means the model is still deliberating — nothing is
/// committed yet.
pub fn closed_thinking_calls(text: &str) -> Vec<ToolCall> {
    // Anything visible means the model is still writing its reply; a thinking-block
    // call must not pre-empt a visible commitment it might still make.
    if !strip_think_blocks(text).trim().is_empty() {
        return Vec::new();
    }
    let mut content = String::new();
    let mut rest = text;
    while let Some((pos, open, close)) = THINK_TAGS
        .iter()
        .filter_map(|(open, close)| rest.find(open).map(|pos| (pos, *open, *close)))
        .min_by_key(|(pos, _, _)| *pos)
    {
        let after = &rest[pos + open.len()..];
        let Some(end) = after.find(close) else {
            break;
        };
        content.push_str(&after[..end]);
        content.push('\n');
        rest = &after[end + close.len()..];
    }
    extract_tool_calls(&content)
}

/// Remove `<think>…</think>` / `<thinking>…</thinking>` spans. An unclosed opener
/// (mid-stream reasoning) swallows the rest of the text, since none of it is
/// visible content yet.
pub fn strip_think_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let open = THINK_TAGS
            .iter()
            .filter_map(|(open, close)| rest.find(open).map(|pos| (pos, *open, *close)))
            .min_by_key(|(pos, _, _)| *pos);
        let Some((pos, open, close)) = open else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..pos]);
        let after = &rest[pos + open.len()..];
        match after.find(close) {
            Some(end) => rest = &after[end + close.len()..],
            None => return out,
        }
    }
}

/// Educated guesses for files a streaming reply will want to read, so the
/// harness can pre-read them in parallel before the model commits the calls.
///
/// Sources (visible content only — reasoning drafts are excluded):
/// 1. Textual `read("path")` / `read(path = "path")` / `read('path')` mentions,
///    committed or not.
/// 2. Path-like tokens in plan bullets (`- `, `* `, `1. `, `• `) that look like
///    source files: explicitly quoted paths, or bare tokens with a known
///    code/config extension.
///
/// Paths are deduplicated and returned in appearance order. The caller decides
/// how many to actually pre-read (backpressure).
pub fn plan_read_guesses(text: &str) -> Vec<String> {
    let visible = strip_think_blocks(text);
    let mut out: Vec<String> = Vec::new();
    let push = |path: &str, out: &mut Vec<String>| {
        let cleaned = path
            .trim()
            .trim_matches([
                '"', '\'', '`', ',', ';', '.', ':', '(', ')', ']', '}', '?', '!',
            ])
            .to_string();
        if cleaned.is_empty()
            || cleaned.starts_with("http")
            || cleaned.starts_with("~")
            || cleaned.starts_with("$")
            || cleaned.contains("://")
        {
            return;
        }
        if !out.iter().any(|existing| existing == &cleaned) {
            out.push(cleaned);
        }
    };

    for line in visible.lines() {
        // 1. `read(...)` mentions inline in this line.
        let mut rest = line;
        while let Some(idx) = rest.find("read") {
            let after = &rest[idx + 4..];
            let mut max = after.len().min(64);
            while !after.is_char_boundary(max) {
                max -= 1;
            }
            if let Some(path) = quoted_string_in(&after[..max]) {
                push(path, &mut out);
            }
            rest = after;
        }
        // 2. Plan bullets: quoted citations always, bare tokens only when they
        //    carry a known source extension.
        let trimmed = line.trim_start();
        let is_bullet = trimmed.starts_with("- ")
            || trimmed.starts_with("* ")
            || trimmed.starts_with("• ")
            || trimmed
                .chars()
                .next()
                .map(|c| c.is_ascii_digit() && trimmed.contains(". "))
                .unwrap_or(false);
        if !is_bullet {
            continue;
        }
        if let Some(quoted) = quoted_string_in(trimmed) {
            push(quoted, &mut out);
            continue;
        }
        for token in trimmed.split_whitespace() {
            if looks_like_source_path(token) {
                push(token, &mut out);
            }
        }
    }
    out
}

fn quoted_string_in(s: &str) -> Option<&str> {
    for quote in ['"', '\''] {
        if let Some(start) = s.find(quote) {
            let inner = &s[start + 1..];
            if let Some(end) = inner.find(quote) {
                return Some(&inner[..end]);
            }
        }
    }
    None
}

fn looks_like_source_path(token: &str) -> bool {
    let token = token.trim().trim_matches([
        '"', '\'', '`', ',', ';', '.', ':', '(', ')', ']', '}', '?', '!',
    ]);
    if token.is_empty() || token.starts_with("http") || token.contains("://") {
        return false;
    }
    [
        "rs", "py", "js", "jsx", "ts", "tsx", "go", "rb", "java", "c", "h", "cpp", "hpp", "cc",
        "cs", "php", "swift", "kt", "toml", "json", "yaml", "yml", "md", "sh", "sql", "html",
        "css", "vue", "svelte", "zig", "lua", "lock",
    ]
    .iter()
    .any(|ext| token.ends_with(&format!(".{ext}")))
}

/// Strip ```tool ... ``` blocks from text for display purposes.
pub fn strip_tool_blocks(text: &str) -> String {
    let mut result = String::new();
    let mut remaining = text;

    while let Some((start, content_start, legacy)) = find_tool_open(remaining) {
        result.push_str(&remaining[..start]);
        let content = &remaining[content_start..];
        // Skip past the payload before looking for the closer: the closing delimiter
        // (``` or </tool>) may appear inside the call's own JSON (an `edit` on
        // markdown), which would otherwise end the block early and leave the rest of
        // the JSON behind as prose.
        let closer = if legacy { "```" } else { TOOL_CLOSE };
        let payload_end = json_object_span(content).map_or(0, |(_, end)| end);
        if let Some(end) = content[payload_end..].find(closer).map(|r| payload_end + r) {
            remaining = &content[end + closer.len()..];
            // Skip leading newline after block
            if remaining.starts_with('\n') {
                remaining = &remaining[1..];
            }
        } else {
            // Unclosed block - skip rest
            break;
        }
    }
    result.push_str(remaining);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_flush_point_never_panics_on_multibyte_tail() {
        // A buffer ending in a multi-byte char (emoji / CJK / accent) used to panic
        // by slicing off a char boundary. It must return a valid boundary.
        for tail in ["日本語", "café", "a日本", "<too日本"] {
            let n = safe_flush_point(tail);
            assert!(
                tail.is_char_boundary(n),
                "cut {n} off boundary for {tail:?}"
            );
            // Slicing at the returned point must not panic.
            let _ = &tail[..n];
        }
    }

    #[test]
    fn safe_flush_point_still_holds_back_partial_marker() {
        assert_eq!(safe_flush_point("hello <to"), "hello ".len());
        assert_eq!(safe_flush_point("plain text"), "plain text".len());
    }

    #[test]
    fn extract_tool_call_parses_valid_json() {
        let text = r#"Some prose
```tool
{"name": "read_file", "args": {"path": "src/main.rs"}, "id": "1"}
```
more text"#;
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].args["path"], "src/main.rs");
        assert_eq!(calls[0].id.as_deref(), Some("1"));
    }

    #[test]
    fn extract_tool_call_with_arguments_key() {
        let text = r#"```tool
{"name": "write_file", "arguments": {"path": "foo.txt", "content": "hello"}}
```"#;
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "write_file");
    }

    #[test]
    fn extract_multiple_tool_calls() {
        let text = r#"First call:
```tool
{"name": "read_file", "args": {"path": "a.txt"}, "id": "1"}
```
Second call:
```tool
{"name": "read_file", "args": {"path": "b.txt"}, "id": "2"}
```"#;
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].args["path"], "b.txt");
    }

    #[test]
    fn extract_recovers_unclosed_final_block() {
        // Stream cut mid-fence: no closing ```. The call must still be recovered.
        let text =
            "sure, listing:\n```tool\n{\"name\":\"list\",\"args\":{\"path\":\".\",\"depth\":2}}";
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "list");
        assert_eq!(calls[0].args["depth"], 2);
    }

    #[test]
    fn extract_recovers_unclosed_block_with_trailing_prose() {
        // Model kept talking after the JSON without ever closing the fence.
        let text = "```tool\n{\"name\":\"read\",\"args\":{\"path\":\"a.rs\"}}\nnow I will read it";
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].args["path"], "a.rs");
    }

    #[test]
    fn closed_calls_before_an_unclosed_one_all_parse() {
        let text = "```tool\n{\"name\":\"read\",\"args\":{\"path\":\"a\"}}\n```\n```tool\n{\"name\":\"list\",\"args\":{\"path\":\".\"}}";
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[1].name, "list");
    }

    #[test]
    fn brace_in_json_string_does_not_fool_extractor() {
        // A `}` inside a string literal must not close the object early.
        let text = "```tool\n{\"name\":\"write\",\"args\":{\"content\":\"fn main() {}\"}}";
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args["content"], "fn main() {}");
    }

    #[test]
    fn extract_json_object_finds_balanced_span() {
        assert_eq!(extract_json_object("x {\"a\":1} y"), Some("{\"a\":1}"));
        assert_eq!(
            extract_json_object("{\"a\":{\"b\":2}} tail"),
            Some("{\"a\":{\"b\":2}}")
        );
        assert_eq!(extract_json_object("no object"), None);
        // Unbalanced (missing close) yields None rather than panicking.
        assert_eq!(extract_json_object("{\"a\":1"), None);
    }

    /// The payload's own strings may contain ```; cutting the fence at the first one
    /// truncated the JSON mid-string and dropped the call with no error anywhere —
    /// `edit` on any file holding a code fence (markdown, doc comments) just did
    /// nothing.
    #[test]
    fn backticks_inside_edit_args_do_not_end_the_fence() {
        let text = "```tool\n{\"name\":\"edit\",\"args\":{\"path\":\"README.md\",\"old\":\"see ```rust\\nfoo\\n```\",\"new\":\"see ```rust\\nbar\\n```\"},\"id\":\"c1\"}\n```\n";
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "edit");
        assert_eq!(calls[0].args["old"], "see ```rust\nfoo\n```");
        assert_eq!(calls[0].args["new"], "see ```rust\nbar\n```");
    }

    #[test]
    fn call_after_a_backtick_carrying_call_still_parses() {
        let text = "```tool\n{\"name\":\"edit\",\"args\":{\"path\":\"a.md\",\"old\":\"```\",\"new\":\"x\"}}\n```\n```tool\n{\"name\":\"read\",\"args\":{\"path\":\"b.rs\"}}\n```";
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "edit");
        assert_eq!(calls[1].name, "read");
        assert_eq!(calls[1].args["path"], "b.rs");
    }

    #[test]
    fn strip_think_blocks_drops_closed_and_unclosed_reasoning() {
        assert_eq!(strip_think_blocks("a<think>x</think>b"), "ab");
        assert_eq!(strip_think_blocks("a<thinking>x</thinking>b"), "ab"); // Unclosed (still streaming) — everything after the opener is reasoning.
        assert_eq!(strip_think_blocks("a<think>x"), "a");
        assert_eq!(strip_think_blocks("no tags"), "no tags");
    }

    /// A fence the model sketched while thinking is not a call it made.
    #[test]
    fn visible_tool_calls_ignore_fences_drafted_in_reasoning() {
        let text = "<think>\nmaybe ```tool\n{\"name\":\"delete\",\"args\":{\"path\":\"src\"}}\n```\nno, too risky\n</think>\n```tool\n{\"name\":\"read\",\"args\":{\"path\":\"src/main.rs\"}}\n```";
        let calls = visible_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read");
        // Drafts alone commit nothing.
        assert!(
            visible_tool_calls("<think>```tool\n{\"name\":\"read\",\"args\":{}}\n```</think>")
                .is_empty()
        );
    }

    /// Endpoints that put the whole reply on the reasoning channel emit no visible
    /// fence at all; once the turn is done, that reasoning text *is* the reply.
    #[test]
    fn committed_tool_calls_fall_back_to_reasoning_only_replies() {
        let reasoning_only =
            "<think>\n```tool\n{\"name\":\"read\",\"args\":{\"path\":\"a.rs\"}}\n```\n</think>";
        assert!(visible_tool_calls(reasoning_only).is_empty());
        let calls = committed_tool_calls(reasoning_only);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read");
        // With a visible call present, reasoning drafts stay ignored.
        let both = "<think>```tool\n{\"name\":\"delete\",\"args\":{\"path\":\"src\"}}\n```</think>\n```tool\n{\"name\":\"read\",\"args\":{\"path\":\"a.rs\"}}\n```";
        let calls = committed_tool_calls(both);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read");
    }

    #[test]
    fn extract_no_tool_calls_returns_empty() {
        assert!(extract_tool_calls("just plain text").is_empty());
        assert!(extract_tool_calls("").is_empty());
    }

    #[test]
    fn closed_thinking_calls_recover_call_after_block_closes() {
        let text = "<thinking>\nI'll delegate this audit.\n<tool>\n{\"name\":\"workflow\",\"args\":{\"action\":\"agent\",\"prompt\":\"audit\"},\"id\":\"c1\"}\n</tool>\n</thinking>";
        let calls = closed_thinking_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "workflow");
        assert_eq!(calls[0].args["action"], "agent");
    }

    #[test]
    fn closed_thinking_calls_ignore_open_blocks_and_visible_content() {
        // Unclosed thinking: still deliberating, nothing committed.
        assert!(closed_thinking_calls(
            "<thinking>\n<tool>\n{\"name\":\"read\",\"args\":{}}\n</tool>"
        )
        .is_empty());
        // Visible prose after the block: the reply is still being written.
        assert!(closed_thinking_calls(
            "<thinking>\n<tool>\n{\"name\":\"read\",\"args\":{}}\n</tool>\n</thinking>\nDone."
        )
        .is_empty());
        // A visible fence beats a thinking-block call.
        assert!(closed_thinking_calls(
            "<thinking>\n<tool>\n{\"name\":\"delete\",\"args\":{}}\n</tool>\n</thinking>\n<tool>\n{\"name\":\"read\",\"args\":{}}\n</tool>"
        )
        .is_empty());
    }

    #[test]
    fn closed_thinking_calls_support_think_variant() {
        let text = "<think>\n<tool>\n{\"name\":\"list\",\"args\":{}}\n</tool>\n</think>";
        let calls = closed_thinking_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "list");
    }

    #[test]
    fn strip_tool_blocks_removes_tool_fences() {
        let text = r#"before
```tool
{"name": "read_file", "args": {"path": "x"}}
```
after"#;
        let stripped = strip_tool_blocks(text);
        assert_eq!(stripped, "before\nafter");
    }

    #[test]
    fn strip_tool_blocks_preserves_non_tool_fences() {
        let text = r#"before ```tool
{"x": "y"}
```
after"#;
        let stripped = strip_tool_blocks(text);
        assert_eq!(stripped, "before after");
    }

    #[test]
    fn strip_tool_blocks_removes_a_call_carrying_backticks_whole() {
        let text = "before\n```tool\n{\"name\":\"edit\",\"args\":{\"path\":\"a.md\",\"old\":\"```sh\\nmake\\n```\",\"new\":\"x\"}}\n```\nafter";
        assert_eq!(strip_tool_blocks(text), "before\nafter");
    }

    #[test]
    fn strip_tool_blocks_handles_unclosed_block() {
        let text = r#"before
```tool
{"name": "read_file", "args": {"path": "x"}}"#;
        let stripped = strip_tool_blocks(text);
        // Unclosed blocks retain everything before the marker plus the marker content
        assert!(stripped.contains("before"));
        assert!(stripped.contains("read_file"));
    }

    #[test]
    fn literal_inline_tool_examples_are_not_executable() {
        for text in [
            "Mention <tool>{\"name\":\"delete\",\"args\":{}}</tool> literally.",
            "Use `<tool>{\"name\":\"delete\",\"args\":{}}</tool>` in docs.",
            "Legacy ```` ```tool {\"name\":\"delete\",\"args\":{}} ``` ```` docs.",
        ] {
            assert!(
                extract_tool_calls(text).is_empty(),
                "executed literal: {text}"
            );
        }
    }

    #[test]
    fn extract_tool_calls_parses_tool_tag() {
        let text = "Sure.\n<tool>\n{\"name\": \"list\", \"args\": {\"path\": \".\"}, \"id\": \"c1\"}\n</tool>\nmore";
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "list");
        assert_eq!(calls[0].id.as_deref(), Some("c1"));
    }

    /// The bug this whole change fixes: a `write` whose `content` contains a code
    /// fence (```` ``` ````) or even a literal `</tool>` must not close the call
    /// early. Brace-balanced payload extraction keeps the whole call intact.
    #[test]
    fn tool_tag_survives_fence_and_close_marker_inside_content() {
        let text = "<tool>\n{\"name\":\"write\",\"args\":{\"path\":\"a.md\",\"content\":\"```json\\n[1]\\n```\\nliteral </tool> too\"}}\n</tool>";
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 1, "call dropped: {calls:?}");
        assert_eq!(calls[0].name, "write");
        let content = calls[0].args["content"].as_str().unwrap();
        assert!(content.contains("```json"));
        assert!(content.contains("</tool>"));
    }

    #[test]
    fn multiple_tool_tags_all_parse() {
        let text = "<tool>{\"name\":\"read\",\"args\":{\"path\":\"a\"}}</tool>\n<tool>{\"name\":\"list\",\"args\":{\"path\":\".\"}}</tool>";
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[1].name, "list");
    }

    #[test]
    fn strip_tool_blocks_removes_tool_tag_whole() {
        let text = "before\n<tool>\n{\"name\":\"write\",\"args\":{\"path\":\"a.md\",\"content\":\"```sh\\nmake\\n```\"}}\n</tool>\nafter";
        assert_eq!(strip_tool_blocks(text), "before\nafter");
    }

    #[test]
    fn legacy_fence_still_parses_for_old_sessions() {
        let text = "```tool\n{\"name\":\"read\",\"args\":{\"path\":\"a.rs\"}}\n```";
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read");
    }

    #[test]
    fn streaming_parser_accumulates_and_extracts() {
        let mut p = StreamingParser::new();
        let (text, calls) = p.push("hello ");
        assert_eq!(text, "hello ");
        assert!(calls.is_empty());
    }

    #[test]
    fn streaming_parser_extracts_tool_call() {
        let mut p = StreamingParser::new();
        let (display, calls) =
            p.push("<tool>\n{\"name\": \"read_file\", \"args\": {\"path\": \"x\"}}\n</tool> rest");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(display, " rest");
    }

    #[test]
    fn streaming_parser_tool_json_with_id() {
        let mut p = StreamingParser::new();
        let mut all_calls = Vec::new();
        let (_, c) = p.push(
            r#"before <tool> {"name":"list_dir","args":{"path":"."},"id":"call_1"} </tool> after"#,
        );
        all_calls.extend(c);
        assert_eq!(all_calls.len(), 1);
        assert_eq!(all_calls[0].name, "list_dir");
        assert_eq!(all_calls[0].id.as_deref(), Some("call_1"));
    }

    #[test]
    fn streaming_parser_tool_without_newline_after_marker() {
        let mut p = StreamingParser::new();
        let (text, calls) =
            p.push(r#">>> <tool> {"name":"read_file","args":{"path":"x"}} </tool> done"#);
        assert_eq!(text, ">>>  done");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
    }

    #[test]
    fn safe_flush_point_holds_back_partial_marker() {
        assert_eq!(safe_flush_point("hello <to"), 6);
        assert_eq!(safe_flush_point("no marker here"), 14);
        assert_eq!(safe_flush_point(""), 0);
    }

    #[test]
    fn plan_read_guesses_finds_read_mentions_and_plan_bullets() {
        let text = r#"Plan: read the executor and cache modules first.
- src/agent/executor.rs
- src/agent/file_cache.rs
1. inspect src/agent/tools.rs
Then I will read("src/agent/parser.rs") and read(path = "src/domain/session.rs").
```tool
{"name": "read", "args": {"path": "src/agent/mod.rs"}}
```"#;
        let guesses = plan_read_guesses(text);
        assert_eq!(
            guesses,
            vec![
                "src/agent/executor.rs",
                "src/agent/file_cache.rs",
                "src/agent/tools.rs",
                "src/agent/parser.rs",
                "src/domain/session.rs",
            ]
        );
    }

    #[test]
    fn plan_read_guesses_dedupes_and_filters_noise() {
        let text = r#"Plan:
- src/main.rs
- src/main.rs (again)
- README.md is fine to skip
- https://example.com/x.rs
- src/foo.txt (not a source path token)
1. step: run cargo build
* src/generated with no extension
- "src/quoted.rs"
- src/lib.rs?.
- `src/backtick.rs`"#;
        let guesses = plan_read_guesses(text);
        assert_eq!(
            guesses,
            vec![
                "src/main.rs",
                "README.md",
                "src/quoted.rs",
                "src/lib.rs",
                "src/backtick.rs",
            ]
        );
    }

    #[test]
    fn plan_read_guesses_handles_multibyte_text_at_scan_boundary() {
        // The 64-byte speculative scan window used to slice directly at byte 64.
        // A box-drawing character crossing that boundary caused a UTF-8 panic.
        let text = format!("read{}─ trailing", "a".repeat(63));
        assert!(plan_read_guesses(&text).is_empty());
    }

    #[test]
    fn plan_read_guesses_ignores_reasoning_drafts() {
        let text =
            "plan: <thinking>maybe read src/secret.rs first</thinking> visible\n- src/open.rs";
        let guesses = plan_read_guesses(text);
        assert_eq!(guesses, vec!["src/open.rs"]);
    }
}
