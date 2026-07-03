// `StreamingParser` and `strip_tool_blocks` are kept for live in-stream tool
// parsing; the app currently extracts tool calls once a turn completes.
#![allow(dead_code)]

use super::tools::ToolCall;

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
                // Looking for closing ```
                if let Some(end_pos) = self.buffer.find("```") {
                    let json_str = self.buffer[..end_pos].trim().to_string();
                    self.buffer = self.buffer[end_pos + 3..].to_string();
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
                // Looking for ```tool opening
                if let Some(start_pos) = self.buffer.find("```tool") {
                    // Everything before the marker goes to display
                    display_text.push_str(&self.buffer[..start_pos]);
                    let after_marker = &self.buffer[start_pos + 7..];
                    // Skip the newline after ```tool
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
/// (i.e. not in the middle of a potential ``` marker).
fn safe_flush_point(buf: &str) -> usize {
    // If the buffer ends with a partial backtick sequence, hold it back.
    // Guard every cut with `is_char_boundary`: a buffer ending in a multi-byte
    // char (emoji / CJK / accents) would otherwise panic on the slice below,
    // crashing mid-stream. The marker `"```tool\n"` is pure ASCII, so a real
    // partial match can only start on a char boundary anyway.
    for suffix_len in (1..=7.min(buf.len())).rev() {
        let cut = buf.len() - suffix_len;
        if !buf.is_char_boundary(cut) {
            continue;
        }
        let suffix = &buf[cut..];
        if "```tool\n".starts_with(suffix) {
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
    let bytes = s.as_bytes();
    let start = s.find('{')?;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for i in start..bytes.len() {
        let b = bytes[i];
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
                    return Some(&s[start..=i]);
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

    while let Some(start) = remaining.find("```tool") {
        let after = &remaining[start + 7..];
        let content_start = if after.starts_with('\n') { 1 } else { 0 };
        let content = &after[content_start..];
        if let Some(end) = content.find("```") {
            let json_str = content[..end].trim();
            if let Some(call) = parse_tool_json(json_str) {
                calls.push(call);
            }
            remaining = &content[end + 3..];
        } else {
            // Unclosed block — the stream was cut mid-fence. Recover the call from
            // whatever trailing content we have rather than dropping it silently.
            if let Some(call) = parse_tool_json(content) {
                calls.push(call);
            }
            break;
        }
    }

    calls
}

/// Strip ```tool ... ``` blocks from text for display purposes.
pub fn strip_tool_blocks(text: &str) -> String {
    let mut result = String::new();
    let mut remaining = text;

    while let Some(start) = remaining.find("```tool") {
        result.push_str(&remaining[..start]);
        let after = &remaining[start + 7..];
        let content_start = if after.starts_with('\n') { 1 } else { 0 };
        let content = &after[content_start..];
        if let Some(end) = content.find("```") {
            remaining = &content[end + 3..];
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
        for tail in ["🚀", "日本語", "café", "a🚀", "```🚀"] {
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
        assert_eq!(safe_flush_point("hello ```"), "hello ".len());
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
        let text = "sure, listing:\n```tool\n{\"name\":\"list\",\"args\":{\"path\":\".\",\"depth\":2}}";
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

    #[test]
    fn extract_no_tool_calls_returns_empty() {
        assert!(extract_tool_calls("just plain text").is_empty());
        assert!(extract_tool_calls("").is_empty());
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
            p.push("```tool\n{\"name\": \"read_file\", \"args\": {\"path\": \"x\"}}\n``` rest");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(display, " rest");
    }

    #[test]
    fn streaming_parser_tool_json_with_id() {
        let mut p = StreamingParser::new();
        let mut all_calls = Vec::new();
        let (_, c) = p.push(
            r#"before ```tool {"name":"list_dir","args":{"path":"."},"id":"call_1"} ``` after"#,
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
            p.push(r#">>> ```tool {"name":"read_file","args":{"path":"x"}} ``` <<<"#);
        assert_eq!(text, ">>>  <<<");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
    }

    #[test]
    fn safe_flush_point_holds_back_partial_marker() {
        assert_eq!(safe_flush_point("hello ```"), 6);
        assert_eq!(safe_flush_point("no marker here"), 14);
        assert_eq!(safe_flush_point(""), 0);
    }
}
