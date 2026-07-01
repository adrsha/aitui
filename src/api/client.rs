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
    /// The stream finished cleanly.
    Done,
    /// A network or protocol error occurred.
    Error(String),
}

pub struct ApiClient {
    client: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl ApiClient {
    pub fn new(endpoint: impl Into<String>, api_key: impl Into<String>) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
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
    pub fn stream(
        &self,
        request: ChatRequest,
    ) -> anyhow::Result<mpsc::Receiver<StreamEvent>> {
        let url = format!("{}/v1/chat/completions", self.endpoint);
        let headers = self.auth_headers()?;
        let client = self.client.clone();

        let (tx, rx) = mpsc::channel(256);

        tokio::spawn(async move {
            let result = stream_inner(client, url, headers, request, tx.clone()).await;
            if let Err(e) = result {
                // If the receiver is gone we don't care about the send error.
                let _ = tx.send(StreamEvent::Error(e.to_string())).await;
            }
        });

        Ok(rx)
    }

    /// Generate an image via `/v1/images/generations` (image models can't be sent
    /// to chat completions — they 503). Spawns a task that saves the result to a
    /// file and reports it back over the same `StreamEvent` channel the chat path
    /// uses, so the UI/agent loop treats it like any other turn.
    pub fn generate_image(&self, model: &str, prompt: &str) -> anyhow::Result<mpsc::Receiver<StreamEvent>> {
        let url = format!("{}/v1/images/generations", self.endpoint);
        let headers = self.auth_headers()?;
        let client = self.client.clone();
        let request = ImageRequest::new(model, prompt);

        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(async move {
            let result = image_inner(client, url, headers, request, tx.clone()).await;
            if let Err(e) = result {
                let _ = tx.send(StreamEvent::Error(e.to_string())).await;
            }
        });
        Ok(rx)
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
        return Err(anyhow::anyhow!("Image API returned neither b64_json nor url"));
    };

    // Save under ./aitui-images/ in the session's working directory.
    let dir = std::path::PathBuf::from("aitui-images");
    std::fs::create_dir_all(&dir)
        .map_err(|e| anyhow::anyhow!("Cannot create {}: {}", dir.display(), e))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("img-{}.png", stamp));
    std::fs::write(&path, &bytes)
        .map_err(|e| anyhow::anyhow!("Cannot write {}: {}", path.display(), e))?;

    let mut msg = format!("🖼 Image saved → `{}`", path.display());
    if let Some(revised) = first.revised_prompt {
        if !revised.trim().is_empty() {
            msg.push_str(&format!("\n\n**Revised prompt:** {}", revised.trim()));
        }
    }
    let _ = tx.send(StreamEvent::Token(msg)).await;
    let _ = tx.send(StreamEvent::Done).await;
    Ok(())
}

async fn stream_inner(
    client: reqwest::Client,
    url: String,
    headers: HeaderMap,
    request: ChatRequest,
    tx: mpsc::Sender<StreamEvent>,
) -> anyhow::Result<()> {
    use futures_util::StreamExt;

    let response = client
        .post(&url)
        .headers(headers)
        .json(&request)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Request failed: {}", e))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("API error {}: {}", status, body));
    }

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    // Native tool-call fragments, accumulated by index across deltas.
    let mut tool_acc: Vec<AccCall> = Vec::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| anyhow::anyhow!("Stream read error: {}", e))?;
        let text = String::from_utf8_lossy(&chunk);
        buffer.push_str(&text);

        // Process all complete lines in the buffer.
        while let Some(newline_pos) = buffer.find('\n') {
            let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
            buffer = buffer[newline_pos + 1..].to_string();

            match parse_sse_line(&line) {
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
                                let _ = tx.send(StreamEvent::Token(content)).await;
                            }
                        }
                        if let Some(r) = choice.delta.reasoning.or(choice.delta.reasoning_content) {
                            if !r.is_empty() {
                                let _ = tx.send(StreamEvent::Reasoning(r)).await;
                            }
                        }
                        // Accumulate native tool-call fragments by index.
                        if let Some(tcs) = choice.delta.tool_calls {
                            accumulate_tool_calls(&mut tool_acc, tcs);
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
}

/// Merge a batch of streamed `tool_calls` fragments into the by-index accumulator.
fn accumulate_tool_calls(acc: &mut Vec<AccCall>, deltas: Vec<super::models::ToolCallDelta>) {
    for d in deltas {
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

/// Emit each accumulated tool call as a synthesized ```` ```tool ```` block token,
/// so the rest of the app (parse_blocks → execute → render) handles native calls
/// through the same path as fenced ones.
async fn flush_tool_calls(acc: &[AccCall], tx: &mpsc::Sender<StreamEvent>) {
    for call in acc {
        if call.name.trim().is_empty() {
            continue;
        }
        let fence = synth_tool_fence(&call.name, &call.args, &call.id);
        let _ = tx.send(StreamEvent::Token(fence)).await;
    }
}

/// Build a ```` ```tool ```` block from a native tool call. The streamed
/// `arguments` is a JSON string; parse it into an object (falling back to `{}` so a
/// malformed payload still produces a runnable call that surfaces the error).
fn synth_tool_fence(name: &str, args: &str, id: &str) -> String {
    let args_val: serde_json::Value =
        serde_json::from_str(args.trim()).unwrap_or_else(|_| serde_json::json!({}));
    let obj = serde_json::json!({ "name": name, "args": args_val, "id": id });
    format!("\n```tool\n{}\n```\n", serde_json::to_string(&obj).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::models::{FnDelta, ToolCallDelta};

    #[test]
    fn accumulate_and_synth_native_tool_call() {
        let mut acc = Vec::new();
        // arguments streamed across two deltas; name+id only on the first.
        accumulate_tool_calls(&mut acc, vec![ToolCallDelta {
            index: 0,
            id: Some("call_1".into()),
            function: Some(FnDelta { name: Some("read_file".into()), arguments: Some("{\"path\":".into()) }),
        }]);
        accumulate_tool_calls(&mut acc, vec![ToolCallDelta {
            index: 0,
            id: None,
            function: Some(FnDelta { name: None, arguments: Some("\"a.rs\"}".into()) }),
        }]);
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

    #[test]
    fn synth_tool_fence_bad_args_still_parses() {
        let fence = synth_tool_fence("list_dir", "not-json", "x");
        let calls = crate::agent::parser::extract_tool_calls(&fence);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "list_dir");
    }
}
