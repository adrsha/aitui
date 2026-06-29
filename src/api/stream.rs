use super::models::StreamChunk;

/// Parsed result of one SSE line.
pub enum SseParsed {
    Done,
    Chunk(StreamChunk),
}

/// Parse a single SSE `data:` line and return the parsed event, if any.
pub fn parse_sse_line(line: &str) -> Option<SseParsed> {
    let line = line.trim();

    if line == "data: [DONE]" {
        return Some(SseParsed::Done);
    }

    if let Some(json) = line.strip_prefix("data: ") {
        if let Ok(chunk) = serde_json::from_str::<StreamChunk>(json) {
            return Some(SseParsed::Chunk(chunk));
        }
    }

    None
}
