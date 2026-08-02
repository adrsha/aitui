use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use tokio::sync::mpsc;

use super::models::{ChatRequest, ImageRequest, ImageResponse};
use super::stream::{parse_sse_line, SseParsed};

/// Events sent from the streaming task back to the UI event loop.
#[derive(Debug)]
pub enum StreamEvent {
    /// A text delta from the model.
    Token(String),
    /// A reasoning ("thinking") delta, when the endpoint streams it separately.
    Reasoning(String),
    /// Final token accounting, when the endpoint reports it.
    Usage(super::models::Usage),
    /// A native structured tool call started streaming; the runnable call is still
    /// emitted as a synthesized `<tool>` block once complete.
    ToolCallStarted(String),
    /// A generated image was fully written and is ready for terminal preview.
    ImageReady(std::path::PathBuf),
    /// The non-streaming image-generation request failed.
    ImageError(String),
    /// The stream finished cleanly.
    Done,
    /// A network or protocol error occurred.
    Error(String),
}

#[derive(Clone)]
pub struct ApiClient {
    client: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl ApiClient {
    pub fn new(endpoint: impl Into<String>, api_key: impl Into<String>) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            // Fail fast on a dead connection instead of hanging, but do NOT set a
            // total request timeout — streamed replies are long-lived and a global
            // timeout would kill a slow-but-healthy generation.
            .connect_timeout(std::time::Duration::from_secs(20))
            // Keep the socket alive so idle gateways don't silently drop the stream.
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            // Force HTTP/1.1 for SSE. Many gateways (Cloudflare et al.) send an
            // HTTP/2 RST_STREAM mid-response — the "stream error received: unexpected
            // EOF" failure — on long streamed replies. HTTP/1.1 chunked transfer is
            // what SSE is built for and doesn't hit that reset path.
            .http1_only()
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build HTTP client: {}", e))?;

        // Trim trailing slashes so `{endpoint}/v1/...` never doubles up (a `//`
        // path 404s / returns empty on many gateways).
        let endpoint = endpoint.into().trim_end_matches('/').to_string();

        Ok(Self {
            client,
            endpoint,
            api_key: api_key.into(),
        })
    }

    fn auth_headers(&self) -> anyhow::Result<HeaderMap> {
        let mut headers = HeaderMap::new();

        let auth_value = format!("Bearer {}", self.api_key);
        let auth_header = HeaderValue::from_str(&auth_value)
            .map_err(|e| anyhow::anyhow!("Invalid API key format: {}", e))?;

        headers.insert(AUTHORIZATION, auth_header);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        Ok(headers)
    }

    /// Spawn a tokio task that streams the response and sends tokens over the
    /// returned channel. The caller drives the channel via recv().
    pub fn stream(&self, request: ChatRequest) -> anyhow::Result<mpsc::Receiver<StreamEvent>> {
        let url = format!("{}/v1/chat/completions", self.endpoint);
        let headers = self.auth_headers()?;
        let client = self.client.clone();

        let (tx, rx) = mpsc::channel(256);

        tokio::spawn(async move {
            // Retry transient connection/stream failures with backoff — but only
            // while nothing has been emitted yet. Once tokens (or tool fragments)
            // have gone out, replaying the request would duplicate them, so a
            // mid-stream drop is surfaced as an error instead of retried.
            const MAX_ATTEMPTS: u32 = 4;
            let mut attempt: u32 = 0;
            loop {
                match stream_inner(
                    client.clone(),
                    url.clone(),
                    headers.clone(),
                    request.clone(),
                    tx.clone(),
                    STREAM_IDLE_TIMEOUT,
                )
                .await
                {
                    Ok(()) => return,
                    Err(fail) => {
                        attempt += 1;
                        if fail.retryable && !fail.emitted && attempt < MAX_ATTEMPTS {
                            // 0.5s, 1s, 2s exponential backoff.
                            let backoff = std::time::Duration::from_millis(500u64 << (attempt - 1));
                            tokio::time::sleep(backoff).await;
                            continue;
                        }
                        // If the receiver is gone we don't care about the send error.
                        let _ = tx.send(StreamEvent::Error(fail.err.to_string())).await;
                        return;
                    }
                }
            }
        });

        Ok(rx)
    }

    /// Generate an image via `/v1/images/generations`. Spawns a task that saves
    /// the result and reports the completed path over the normal stream channel.
    pub fn generate_image(
        &self,
        model: &str,
        prompt: &str,
    ) -> anyhow::Result<mpsc::Receiver<StreamEvent>> {
        let url = format!("{}/v1/images/generations", self.endpoint);
        let headers = self.auth_headers()?;
        let client = self.client.clone();
        let request = ImageRequest::new(model, prompt);

        let dir = std::path::PathBuf::from("aitui-images");
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let out_path = dir.join(format!("img-{}.png", stamp));
        let out_str = out_path.to_string_lossy().to_string();

        let (tx, rx) = mpsc::channel(8);
        let out_path2 = out_str.clone();
        tokio::spawn(async move {
            let result = image_inner(client, url, headers, request, out_path2, tx.clone()).await;
            if let Err(e) = result {
                let _ = tx.send(StreamEvent::ImageError(e.to_string())).await;
            }
        });
        Ok(rx)
    }

    /// One-shot, non-streaming chat completion — used by the access-policy judge,
    /// which needs a single short reply rather than a token stream. Returns the
    /// assistant message text (empty string if the endpoint returns no content).
    pub async fn complete(&self, request: ChatRequest) -> anyhow::Result<String> {
        let url = format!("{}/v1/chat/completions", self.endpoint);
        let headers = self.auth_headers()?;

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .json(&request)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Completion request failed: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Completion API error {}: {}", status, body));
        }

        let parsed: super::models::ChatResponse = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse completion response: {}", e))?;

        Ok(parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default())
    }

    /// Fetch available models from the /v1/models endpoint.
    /// Returns a sorted list of model IDs on success.
    pub async fn fetch_models(&self) -> anyhow::Result<Vec<String>> {
        let url = format!("{}/v1/models", self.endpoint);
        let headers = self.auth_headers()?;

        let response = self
            .client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Models request failed: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Models API error {}: {}", status, body));
        }

        let body: ModelsResponse = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse models response: {}", e))?;

        let mut ids: Vec<String> = body.data.into_iter().map(|m| m.id).collect();
        ids.sort();
        Ok(ids)
    }
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

async fn image_inner(
    client: reqwest::Client,
    url: String,
    headers: HeaderMap,
    request: ImageRequest,
    path_str: String,
    tx: mpsc::Sender<StreamEvent>,
) -> anyhow::Result<()> {
    use base64::Engine;

    let response = client
        .post(&url)
        .headers(headers)
        .json(&request)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Image request failed: {}", e))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("Image API error {}: {}", status, body));
    }

    // Cancelled while the image was still coming down? Stop before decode/write.
    if tx.is_closed() {
        return Ok(());
    }

    let parsed: ImageResponse = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse image response: {}", e))?;

    let first = parsed
        .data
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("Image API returned no images"))?;

    // Get the PNG bytes: either inline base64 (gpt-image) or a URL to fetch (dall-e).
    let bytes: Vec<u8> = if let Some(b64) = first.b64_json {
        base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|e| anyhow::anyhow!("Bad base64 image data: {}", e))?
    } else if let Some(img_url) = &first.url {
        let r = client
            .get(img_url)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Fetching generated image failed: {}", e))?;
        r.bytes()
            .await
            .map_err(|e| anyhow::anyhow!("Reading generated image failed: {}", e))?
            .to_vec()
    } else {
        return Err(anyhow::anyhow!(
            "Image API returned neither b64_json nor url"
        ));
    };

    let image = image::load_from_memory(&bytes)
        .map_err(|e| anyhow::anyhow!("Generated image could not be decoded: {}", e))?;
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| anyhow::anyhow!("Generated image could not be encoded as PNG: {}", e))?;

    let path = std::path::PathBuf::from(&path_str);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("Cannot create {}: {}", parent.display(), e))?;
    }
    std::fs::write(&path, &png)
        .map_err(|e| anyhow::anyhow!("Cannot write {}: {}", path.display(), e))?;

    let mut msg = format!("Image saved → `{}`", path.display());
    if let Some(revised) = first.revised_prompt {
        if !revised.trim().is_empty() {
            msg.push_str(&format!("\n\n**Revised prompt:** {}", revised.trim()));
        }
    }
    let _ = tx.send(StreamEvent::Token(msg)).await;
    let _ = tx.send(StreamEvent::ImageReady(path)).await;
    let _ = tx.send(StreamEvent::Done).await;
    Ok(())
}

/// A stream failure plus enough context for the caller to decide whether to retry.
struct StreamFail {
    err: anyhow::Error,
    /// True once any token/reasoning/tool fragment has been sent — a retry would
    /// duplicate it, so the caller must not replay.
    emitted: bool,
    /// True for transient connection/stream errors (safe to retry); false for
    /// hard failures like an HTTP 4xx status, where a retry is pointless.
    retryable: bool,
}

/// How long a stream may go without any bytes before it is considered dead.
/// Generous: reasoning models can think for a while, but a silent socket past
/// this is almost certainly a dropped gateway connection.
const STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

async fn stream_inner(
    client: reqwest::Client,
    url: String,
    headers: HeaderMap,
    request: ChatRequest,
    tx: mpsc::Sender<StreamEvent>,
    idle: std::time::Duration,
) -> Result<(), StreamFail> {
    use futures_util::StreamExt;

    // Tracks whether we've handed anything to the UI. Connection setup and the
    // status check happen before any emission, so those failures are replayable.
    let mut emitted = false;
    macro_rules! fail {
        ($retryable:expr, $err:expr) => {
            return Err(StreamFail {
                err: $err,
                emitted,
                retryable: $retryable,
            })
        };
    }

    let response = match client
        .post(&url)
        .headers(headers)
        .json(&request)
        .send()
        .await
    {
        Ok(r) => r,
        // Connect/timeout/reset before we even have a response: transient, retry.
        Err(e) => fail!(true, anyhow::anyhow!("Request failed: {}", e)),
    };

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        // 408/429/5xx are transient (server busy / rate limited); 4xx is a hard
        // client error (bad key, bad request) that will fail identically on retry.
        let retryable =
            status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error();
        fail!(retryable, anyhow::anyhow!("API error {}: {}", status, body));
    }

    let mut stream = response.bytes_stream();
    // Raw byte buffer: SSE lines are newline-delimited and a UTF-8 char never
    // contains a 0x0A byte, so splitting on b'\n' can't cut a char in half. Decoding
    // whole lines (not per-chunk) avoids the mangling `from_utf8_lossy` caused when a
    // multi-byte char straddled two network chunks.
    let mut buffer: Vec<u8> = Vec::new();
    // Native tool-call fragments, accumulated by index across deltas.
    let mut tool_acc: Vec<AccCall> = Vec::new();

    loop {
        // Cancellation: when the app drops the receiver (`CancelStream`), stop
        // reading. Dropping the response here closes the connection, so a cancelled
        // request really aborts instead of quietly downloading to the end.
        let next = tokio::select! {
            biased;
            _ = tx.closed() => return Ok(()),
            n = tokio::time::timeout(idle, stream.next()) => n,
        };
        let chunk = match next {
            Err(_elapsed) => fail!(
                !emitted,
                anyhow::anyhow!(
                    "Stream idle timeout after {}s (no bytes received)",
                    idle.as_secs()
                )
            ),
            Ok(Some(Ok(c))) => c,
            Ok(Some(Err(e))) => fail!(true, anyhow::anyhow!("Stream read error: {}", e)),
            Ok(None) => break,
        };
        buffer.extend_from_slice(&chunk);

        // Process all complete lines in the buffer.
        while let Some(newline_pos) = buffer.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = buffer.drain(..=newline_pos).collect();
            // Drop the trailing '\n' (and any '\r') and decode this whole line.
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim_end_matches('\n').trim_end_matches('\r');

            match parse_sse_line(line) {
                Some(SseParsed::Done) => {
                    flush_tool_calls(&tool_acc, &tx).await;
                    let _ = tx.send(StreamEvent::Done).await;
                    return Ok(());
                }
                Some(SseParsed::Chunk(chunk)) => {
                    if let Some(usage) = chunk.usage {
                        let _ = tx.send(StreamEvent::Usage(usage)).await;
                    }
                    for choice in chunk.choices {
                        if let Some(content) = choice.delta.content {
                            if !content.is_empty() {
                                emitted = true;
                                let _ = tx.send(StreamEvent::Token(content)).await;
                            }
                        }
                        if let Some(r) = choice.delta.reasoning.or(choice.delta.reasoning_content) {
                            if !r.is_empty() {
                                emitted = true;
                                let _ = tx.send(StreamEvent::Reasoning(r)).await;
                            }
                        }
                        // Accumulate native tool-call fragments by index.
                        if let Some(tcs) = choice.delta.tool_calls {
                            emitted = true;
                            let indices: Vec<usize> = tcs.iter().map(|tc| tc.index).collect();
                            accumulate_tool_calls(&mut tool_acc, tcs);
                            for fence in take_completed_interaction_fences(&mut tool_acc) {
                                let _ = tx.send(StreamEvent::Token(fence)).await;
                            }
                            for index in indices {
                                let Some(call) = tool_acc.get(index) else {
                                    continue;
                                };
                                if let Some(label) = preparing_tool_label(call) {
                                    let _ = tx.send(StreamEvent::ToolCallStarted(label)).await;
                                }
                            }
                        }
                        // Finish reason signals stream end even without [DONE].
                        if choice.finish_reason.is_some() {
                            flush_tool_calls(&tool_acc, &tx).await;
                            let _ = tx.send(StreamEvent::Done).await;
                            return Ok(());
                        }
                    }
                }
                None => {} // blank line or comment — skip
            }
        }
    }

    // Stream ended without [DONE]; treat as done.
    flush_tool_calls(&tool_acc, &tx).await;
    let _ = tx.send(StreamEvent::Done).await;
    Ok(())
}

/// One accumulating native tool call being assembled from streamed fragments.
#[derive(Default)]
struct AccCall {
    id: String,
    name: String,
    args: String,
    emitted: bool,
}

fn preparing_tool_label(call: &AccCall) -> Option<String> {
    let name = call.name.trim();
    if name.is_empty() {
        return None;
    }
    if matches!(name, "file_management" | "web" | "interaction" | "workflow") {
        if let Some(action) = streamed_action(&call.args) {
            return Some(action);
        }
    }
    Some(name.to_string())
}

fn streamed_action(args: &str) -> Option<String> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(args) {
        return value
            .get("action")
            .and_then(|action| action.as_str())
            .filter(|action| !action.trim().is_empty())
            .map(str::to_string);
    }

    let action = args.find("\"action\"")?;
    let after_key = &args[action + "\"action\"".len()..];
    let colon = after_key.find(':')?;
    let value = after_key[colon + 1..].trim_start();
    let value = value.strip_prefix('"')?;
    let end = value.find('"')?;
    let action = &value[..end];
    (!action.is_empty()).then(|| action.to_string())
}

/// Merge a batch of streamed `tool_calls` fragments into the by-index accumulator.
fn accumulate_tool_calls(acc: &mut Vec<AccCall>, deltas: Vec<super::models::ToolCallDelta>) {
    // A single turn never has anywhere near this many parallel tool calls. The
    // index is server-controlled, so cap it: a bogus huge value would otherwise
    // make `resize_with` attempt a multi-gigabyte allocation (OOM) — and `+1`
    // could overflow `usize`.
    const MAX_TOOL_CALLS: usize = 256;
    for d in deltas {
        if d.index >= MAX_TOOL_CALLS {
            continue;
        }
        if d.index >= acc.len() {
            acc.resize_with(d.index + 1, AccCall::default);
        }
        let slot = &mut acc[d.index];
        if let Some(id) = d.id {
            slot.id = id;
        }
        if let Some(f) = d.function {
            if let Some(n) = f.name {
                slot.name.push_str(&n);
            }
            if let Some(a) = f.arguments {
                slot.args.push_str(&a);
            }
        }
    }
}

fn take_completed_interaction_fences(acc: &mut [AccCall]) -> Vec<String> {
    let mut fences = Vec::new();
    for call in acc {
        if call.emitted || call.name.trim().is_empty() {
            continue;
        }
        let Ok(args) = serde_json::from_str::<serde_json::Value>(call.args.trim()) else {
            continue;
        };
        let blocking = match call.name.as_str() {
            "interaction" => matches!(
                args.get("action").and_then(|action| action.as_str()),
                Some("ask" | "propose" | "plan")
            ),
            "workflow" => matches!(
                args.get("action").and_then(|action| action.as_str()),
                Some("propose")
            ),
            "ask" | "decide" | "plan" | "propose_step" => true,
            _ => false,
        };
        if !blocking {
            continue;
        }
        call.emitted = true;
        fences.push(synth_tool_fence(&call.name, &call.args, &call.id));
    }
    fences
}

/// Emit each accumulated tool call as a synthesized `<tool>…</tool>` block token,
/// so the rest of the app (parse_blocks → execute → render) handles native calls
/// through the same path as fenced ones.
async fn flush_tool_calls(acc: &[AccCall], tx: &mpsc::Sender<StreamEvent>) {
    for call in acc {
        if call.emitted || call.name.trim().is_empty() {
            continue;
        }
        let fence = synth_tool_fence(&call.name, &call.args, &call.id);
        let _ = tx.send(StreamEvent::Token(fence)).await;
    }
}

/// Build a `<tool>…</tool>` block from a native tool call. The streamed `arguments`
/// is a JSON string; parse it into an object (falling back to `{}` so a malformed
/// payload still produces a runnable call that surfaces the error).
///
/// The payload is brace-balanced JSON, so the parser recovers the whole object even
/// when a string value contains the closing `</tool>` marker — no escaping needed,
/// unlike the old ```` ```tool ```` fence which a code fence in the content could
/// close early.
fn synth_tool_fence(name: &str, args: &str, id: &str) -> String {
    use crate::agent::parser::{TOOL_CLOSE, TOOL_OPEN};
    let args_val: serde_json::Value =
        serde_json::from_str(args.trim()).unwrap_or_else(|_| serde_json::json!({}));
    let obj = serde_json::json!({ "name": name, "args": args_val, "id": id });
    let json = serde_json::to_string(&obj).unwrap_or_default();
    format!("\n{}\n{}\n{}\n", TOOL_OPEN, json, TOOL_CLOSE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::models::{FnDelta, ToolCallDelta};

    #[test]
    fn categorized_preparing_label_resolves_streamed_leaf_action() {
        let complete = AccCall {
            name: "file_management".into(),
            args: r#"{"action":"copy","from":"a","to":"b"}"#.into(),
            ..AccCall::default()
        };
        assert_eq!(preparing_tool_label(&complete).as_deref(), Some("copy"));

        let partial = AccCall {
            name: "web".into(),
            args: r#"{"action": "fetch""#.into(),
            ..AccCall::default()
        };
        assert_eq!(preparing_tool_label(&partial).as_deref(), Some("fetch"));
    }

    #[test]
    fn categorized_preparing_label_falls_back_until_action_arrives() {
        let call = AccCall {
            name: "file_management".into(),
            args: "{\"act".into(),
            ..AccCall::default()
        };
        assert_eq!(
            preparing_tool_label(&call).as_deref(),
            Some("file_management")
        );
    }

    #[test]
    fn completed_interaction_is_emitted_before_stream_finish_once() {
        let mut acc = Vec::new();
        accumulate_tool_calls(
            &mut acc,
            vec![ToolCallDelta {
                index: 0,
                id: Some("call_propose".into()),
                function: Some(FnDelta {
                    name: Some("interaction".into()),
                    arguments: Some(
                        r#"{"action":"propose","title":"Choose","alternatives":["#.into(),
                    ),
                }),
            }],
        );
        assert!(take_completed_interaction_fences(&mut acc).is_empty());

        accumulate_tool_calls(
            &mut acc,
            vec![ToolCallDelta {
                index: 0,
                id: None,
                function: Some(FnDelta {
                    name: None,
                    arguments: Some(r#"{"label":"A"},{"label":"B"}]}"#.into()),
                }),
            }],
        );
        let fences = take_completed_interaction_fences(&mut acc);
        assert_eq!(fences.len(), 1);
        let calls = crate::agent::parser::extract_tool_calls(&fences[0]);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].kind(), Some(crate::agent::ToolKind::ProposeStep));
        assert_eq!(calls[0].id.as_deref(), Some("call_propose"));
        assert!(take_completed_interaction_fences(&mut acc).is_empty());
    }

    #[test]
    fn normal_native_tools_remain_deferred_for_batching() {
        let mut acc = vec![AccCall {
            id: "call_read".into(),
            name: "file_management".into(),
            args: r#"{"action":"read","path":"src/main.rs"}"#.into(),
            emitted: false,
        }];
        assert!(take_completed_interaction_fences(&mut acc).is_empty());
        assert!(!acc[0].emitted);
    }

    #[test]
    fn workflow_propose_compatibility_is_emitted_early() {
        let mut acc = vec![AccCall {
            id: "call_workflow".into(),
            name: "workflow".into(),
            args: r#"{"action":"propose","title":"Choose","alternatives":[{"label":"A"},{"label":"B"}]}"#
                .into(),
            emitted: false,
        }];
        let fences = take_completed_interaction_fences(&mut acc);
        assert_eq!(fences.len(), 1);
        assert_eq!(
            crate::agent::parser::extract_tool_calls(&fences[0])[0].kind(),
            Some(crate::agent::ToolKind::ProposeStep)
        );
    }

    #[test]
    fn direct_propose_step_is_emitted_early() {
        let mut acc = vec![AccCall {
            id: "call_direct".into(),
            name: "propose_step".into(),
            args: r#"{"title":"Choose","alternatives":[{"label":"A"},{"label":"B"}]}"#.into(),
            emitted: false,
        }];
        let fences = take_completed_interaction_fences(&mut acc);
        assert_eq!(fences.len(), 1);
        assert_eq!(
            crate::agent::parser::extract_tool_calls(&fences[0])[0].kind(),
            Some(crate::agent::ToolKind::ProposeStep)
        );
    }

    #[test]
    fn accumulate_and_synth_native_tool_call() {
        let mut acc = Vec::new();
        // arguments streamed across two deltas; name+id only on the first.
        accumulate_tool_calls(
            &mut acc,
            vec![ToolCallDelta {
                index: 0,
                id: Some("call_1".into()),
                function: Some(FnDelta {
                    name: Some("read_file".into()),
                    arguments: Some("{\"path\":".into()),
                }),
            }],
        );
        accumulate_tool_calls(
            &mut acc,
            vec![ToolCallDelta {
                index: 0,
                id: None,
                function: Some(FnDelta {
                    name: None,
                    arguments: Some("\"a.rs\"}".into()),
                }),
            }],
        );
        assert_eq!(acc.len(), 1);
        assert_eq!(acc[0].name, "read_file");
        assert_eq!(acc[0].args, "{\"path\":\"a.rs\"}");

        // The synthesized fence must parse back through the normal block path.
        let fence = synth_tool_fence(&acc[0].name, &acc[0].args, &acc[0].id);
        let calls = crate::agent::parser::extract_tool_calls(&fence);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].args.get("path").unwrap(), "a.rs");
        assert_eq!(calls[0].id.as_deref(), Some("call_1"));
    }

    /// A native `edit` whose old/new carries a markdown fence: the `<tool>` block
    /// keeps the backticks verbatim (no escaping) and the payload survives the round
    /// trip byte-for-byte, because extraction is brace-balanced, not fence-delimited.
    #[test]
    fn synth_block_round_trips_fence_in_payload() {
        let args = serde_json::json!({
            "path": "docs/README.md",
            "old": "run:\n```sh\nmake\n```",
            "new": "run:\n```sh\nmake all\n```",
        })
        .to_string();
        let block = synth_tool_fence("edit", &args, "call_1");
        assert!(block.contains("<tool>") && block.contains("</tool>"));

        let calls = crate::agent::parser::extract_tool_calls(&block);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "edit");
        assert_eq!(calls[0].args["old"], "run:\n```sh\nmake\n```");
        assert_eq!(calls[0].args["new"], "run:\n```sh\nmake all\n```");
    }

    #[test]
    fn accumulate_ignores_absurd_index_without_oom() {
        // A server (or bug) sending a huge index must not trigger a giant
        // allocation or an overflow panic — the delta is dropped.
        let mut acc = Vec::new();
        accumulate_tool_calls(
            &mut acc,
            vec![ToolCallDelta {
                index: usize::MAX,
                id: Some("x".into()),
                function: Some(FnDelta {
                    name: Some("read".into()),
                    arguments: None,
                }),
            }],
        );
        assert!(acc.is_empty());
        // A reasonable index right at the cap boundary is also rejected.
        accumulate_tool_calls(
            &mut acc,
            vec![ToolCallDelta {
                index: 10_000,
                id: None,
                function: None,
            }],
        );
        assert!(acc.is_empty());
    }

    #[test]
    fn synth_tool_fence_bad_args_still_parses() {
        let fence = synth_tool_fence("list_dir", "not-json", "x");
        let calls = crate::agent::parser::extract_tool_calls(&fence);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "list_dir");
    }

    /// A local HTTP server that accepts one connection, consumes the request
    /// headers, writes a canned SSE body (or stays silent when `silent`), then
    /// holds the socket open.
    fn sse_server(silent: bool) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let handle = std::thread::spawn(move || {
            use std::io::{BufRead, BufReader, Write};
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
            }
            let mut stream = reader.into_inner();
            let mut body = String::from(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
            );
            if !silent {
                body.push_str(
                    "c1\r\ndata: {\"id\":\"x\",\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n\r\n0\r\n\r\n",
                );
            }
            if let Err(_) = stream.write_all(body.as_bytes()) {
                return;
            }
            if let Err(_) = stream.flush() {
                return;
            }
            // Hold the connection open (no more bytes) until the test ends.
            std::thread::sleep(std::time::Duration::from_secs(30));
        });
        (format!("http://{addr}"), handle)
    }

    /// Server that answers the first connection with a transient 500 and the
    /// second with a valid SSE body — for retry-with-backoff tests.
    fn retry_server() -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let handle = std::thread::spawn(move || {
            use std::io::{BufRead, BufReader, Write};
            for attempt in 0..2u32 {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                }
                let mut stream = reader.into_inner();
                if attempt == 0 {
                    let body = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n";
                    if let Err(_) = stream.write_all(body.as_bytes()) {
                        return;
                    }
                } else {
                    let body = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\nc1\r\ndata: {\"id\":\"x\",\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n\r\n0\r\n\r\n";
                    if let Err(_) = stream.write_all(body.as_bytes()) {
                        return;
                    }
                }
                if let Err(_) = stream.flush() {
                    return;
                }
            }
            // Hold the socket open until the test ends.
            std::thread::sleep(std::time::Duration::from_secs(30));
        });
        (format!("http://{addr}"), handle)
    }

    #[tokio::test]
    async fn stream_retries_transient_500_with_backoff_before_emission() {
        let (url, server) = retry_server();
        let client = ApiClient::new(&url, "test-key").unwrap();
        let mut rx = client
            .stream(ChatRequest {
                model: String::new(),
                messages: Vec::new(),
                stream: true,
                max_tokens: None,
                stream_options: None,
                reasoning_effort: None,
                reasoning_mode: None,
                tools: None,
                tool_choice: None,
                parallel_tool_calls: None,
            })
            .unwrap();
        let first = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv()).await;
        assert!(
            matches!(first, Ok(Some(StreamEvent::Token(_)))),
            "expected token after retry, got: {:?}",
            first
        );
        drop(server);
    }

    #[tokio::test]
    async fn stream_idle_timeout_fires_when_server_goes_silent() {
        let (url, server) = sse_server(true);
        let client = reqwest::Client::builder().http1_only().build().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let start = std::time::Instant::now();
        let err = stream_inner(
            client,
            format!("{}/v1/chat/completions", url),
            HeaderMap::new(),
            ChatRequest {
                model: String::new(),
                messages: Vec::new(),
                stream: true,
                max_tokens: None,
                stream_options: None,
                reasoning_effort: None,
                reasoning_mode: None,
                tools: None,
                tool_choice: None,
                parallel_tool_calls: None,
            },
            tx,
            std::time::Duration::from_millis(300),
        )
        .await
        .unwrap_err();
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
        assert!(
            err.retryable,
            "silent pre-emission failure should be retried"
        );
        assert!(
            err.err.to_string().contains("idle timeout"),
            "got: {}",
            err.err
        );
        drop(server);
    }

    #[test]
    fn stream_aborts_connection_when_receiver_is_dropped() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (url, server) = sse_server(false);
            let client = reqwest::Client::builder().http1_only().build().unwrap();
            let (tx, mut rx) = tokio::sync::mpsc::channel(16);
            let idle = std::time::Duration::from_secs(60);
            let task = tokio::spawn(async move {
                stream_inner(
                    client,
                    format!("{}/v1/chat/completions", url),
                    HeaderMap::new(),
                    ChatRequest {
                        model: String::new(),
                        messages: Vec::new(),
                        stream: true,
                        max_tokens: None,
                        stream_options: None,
                        reasoning_effort: None,
                        reasoning_mode: None,
                        tools: None,
                        tool_choice: None,
                        parallel_tool_calls: None,
                    },
                    tx,
                    idle,
                )
                .await
            });
            let first = rx.recv().await;
            assert!(
                matches!(first, Some(StreamEvent::Token(_))),
                "expected first token, got: {:?}",
                first
            );
            drop(rx); // simulate CancelStream: receiver dropped
            let start = std::time::Instant::now();
            let result = tokio::time::timeout(std::time::Duration::from_secs(10), task).await;
            assert!(start.elapsed() < std::time::Duration::from_secs(10));
            let ok = matches!(result, Ok(Ok(Ok(()))));
            assert!(ok, "dropped receiver should abort cleanly");
            drop(server);
        });
    }
}
