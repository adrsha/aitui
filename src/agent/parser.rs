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
    // If the buffer ends with a partial backtick sequence, hold it back
    for suffix_len in (1..=7.min(buf.len())).rev() {
        let suffix = &buf[buf.len() - suffix_len..];
        if "```tool\n".starts_with(suffix) {
            return buf.len() - suffix_len;
        }
    }
    buf.len()
}

fn parse_tool_json(s: &str) -> Option<ToolCall> {
    // Try strict parse first
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
        let name = v.get("name").and_then(|n| n.as_str()).map(|s| s.to_string())?;
        let args = v.get("args").or_else(|| v.get("arguments")).cloned()
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
        let id = v.get("id").and_then(|i| i.as_str()).map(|s| s.to_string());
        return Some(ToolCall { name, args, id });
    }
    // Try to extract just the name for partial JSON
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
