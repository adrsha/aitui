use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use tokio::sync::mpsc;

use super::models::ChatRequest;
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
                        // Finish reason signals stream end even without [DONE].
                        if choice.finish_reason.is_some() {
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
    let _ = tx.send(StreamEvent::Done).await;
    Ok(())
}
