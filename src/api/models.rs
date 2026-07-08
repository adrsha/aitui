use serde::{Deserialize, Serialize};

/// A single message in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: MessageContent,
    /// True for assistant turns produced by the offline mock backend. Mock turns
    /// remain visible in the UI, but are omitted from live API context/transcripts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mock: bool,
    /// How long this response/tool result took to finish, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Time to first result (first token/reasoning byte) in ms — how long the
    /// assistant took to return *anything*, distinct from the full-stream duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_ms: Option<u64>,
    /// Native function-calling: an assistant turn's structured tool calls. Only
    /// set on the wire (built by `api_messages` in native mode); stored sessions
    /// keep tool calls as fenced text, so this defaults to None on load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ApiToolCall>>,
    /// Native function-calling: the id of the call this `role:"tool"` message answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// A structured tool call in the OpenAI `tools` protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String, // always "function"
    pub function: ApiFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiFunction {
    pub name: String,
    /// JSON-encoded arguments **string** (per the OpenAI spec), not an object.
    pub arguments: String,
}

impl ApiToolCall {
    pub fn function(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: "function".into(),
            function: ApiFunction {
                name: name.into(),
                arguments: arguments.into(),
            },
        }
    }
}

/// Content can be a plain string or a list of parts (for multimodal).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrl },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String, // data:image/png;base64,...
}

impl ChatMessage {
    /// A plain text message with no native tool metadata.
    fn plain(role: &str, content: MessageContent) -> Self {
        Self {
            role: role.to_string(),
            content,
            mock: false,
            duration_ms: None,
            first_ms: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self::plain("user", MessageContent::Text(text.into()))
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self::plain("assistant", MessageContent::Text(text.into()))
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self::plain("system", MessageContent::Text(text.into()))
    }

    /// A tool-result message. Stored with role "tool" for distinct rendering;
    /// `Session::api_messages` re-maps it (to "user" in fenced mode, or to a native
    /// `role:"tool"` with `tool_call_id` in native mode) when sending to the API.
    pub fn tool(text: impl Into<String>) -> Self {
        Self::plain("tool", MessageContent::Text(text.into()))
    }

    pub fn user_with_image(text: &str, base64_data: &str, mime_type: &str) -> Self {
        let data_url = format!("data:{};base64,{}", mime_type, base64_data);
        let parts = vec![
            ContentPart::ImageUrl {
                image_url: ImageUrl { url: data_url },
            },
            ContentPart::Text {
                text: text.to_string(),
            },
        ];
        Self::plain("user", MessageContent::Parts(parts))
    }
}

/// The request body sent to the OpenAI-compatible endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Ask the endpoint to append a final usage frame to the stream so we can
    /// report token counts. Ignored by servers that don't support it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    /// Reasoning effort for reasoning-capable models (e.g. "low"/"medium"/"high"
    /// for GPT-5 / o-series). Omitted when None so non-reasoning models are fine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Native function-calling tool schemas (`agent::tool_schemas()`). Omitted for
    /// non-agent turns / endpoints without tool support.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    /// Let the model emit several independent tool calls in ONE turn so the app
    /// runs them as a batch (one round-trip instead of one per call). Omitted for
    /// non-agent turns / endpoints that don't support it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StreamOptions {
    pub include_usage: bool,
}

impl ChatRequest {
    pub fn new(model: &str, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.to_string(),
            messages,
            stream: true,
            max_tokens: None,
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            reasoning_effort: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
        }
    }

    /// Set the reasoning effort ("low"/"medium"/"high"); None clears it.
    pub fn with_reasoning_effort(mut self, effort: Option<String>) -> Self {
        self.reasoning_effort = effort;
        self
    }

    /// Attach native function-calling tool schemas with `tool_choice:"required"`.
    pub fn with_tools(mut self, schemas: serde_json::Value) -> Self {
        self.tools = Some(schemas);
        self.tool_choice = Some(serde_json::Value::String("auto".to_string()));
        self.parallel_tool_calls = Some(true);
        self
    }
}

/// Whether a model id is an image-generation model, which must be sent to
/// `/v1/images/generations` rather than `/v1/chat/completions`.
pub fn is_image_model(model: &str) -> bool {
    let m = model.to_lowercase();
    // Match the model name itself, not any substring: a chat model whose id merely
    // *contains* "image" (e.g. a vision/captioning chat model) must NOT be routed to
    // the image endpoint. Use the final path segment so a vendor prefix like
    // "openai/gpt-image-1" still resolves, and anchor to known image families.
    let name = m.rsplit('/').next().unwrap_or(&m);
    name.starts_with("gpt-image") || name.starts_with("dall-e") || name.starts_with("dalle")
}

/// Request body for `/v1/images/generations`. `response_format` is intentionally
/// omitted: `gpt-image-*` always returns base64 and rejects the field, while
/// `dall-e` defaults to a URL — the response parser handles both.
#[derive(Debug, Serialize)]
pub struct ImageRequest {
    pub model: String,
    pub prompt: String,
    pub n: u32,
    pub size: String,
}

impl ImageRequest {
    pub fn new(model: &str, prompt: &str) -> Self {
        Self {
            model: model.to_string(),
            prompt: prompt.to_string(),
            n: 1,
            size: "1024x1024".to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ImageResponse {
    #[serde(default)]
    pub data: Vec<ImageData>,
}

#[derive(Debug, Deserialize)]
pub struct ImageData {
    #[serde(default)]
    pub b64_json: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub revised_prompt: Option<String>,
}

/// A non-streaming chat completion response (used by the access-policy judge,
/// which needs a single short answer, not a token stream).
#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    #[serde(default)]
    pub choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
pub struct ChatChoice {
    pub message: ChatResponseMessage,
}

#[derive(Debug, Deserialize)]
pub struct ChatResponseMessage {
    #[serde(default)]
    pub content: Option<String>,
}

/// Token accounting reported by the endpoint at the end of a stream.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
}

/// A single SSE data line decoded from the stream.
#[derive(Debug, Deserialize)]
pub struct StreamChunk {
    #[serde(default)]
    pub choices: Vec<StreamChoice>,
    /// Present only on the final usage frame (when `stream_options.include_usage`).
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
pub struct StreamChoice {
    pub delta: DeltaContent,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeltaContent {
    pub content: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub reasoning_content: Option<String>,
    /// Native function-calling: tool-call fragments, streamed and accumulated by index.
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCallDelta>>,
}

/// One streamed fragment of a native tool call. `id`/`function.name` arrive on the
/// first fragment; `function.arguments` is streamed in pieces to be concatenated.
#[derive(Debug, Deserialize)]
pub struct ToolCallDelta {
    #[serde(default)]
    pub index: usize,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<FnDelta>,
}

#[derive(Debug, Deserialize)]
pub struct FnDelta {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_models_detected_chat_models_not() {
        assert!(is_image_model("gpt-image-1"));
        assert!(is_image_model("gpt-image-2"));
        assert!(is_image_model("dall-e-3"));
        assert!(is_image_model("DALL-E-2"));
        assert!(!is_image_model("gpt-4o"));
        assert!(!is_image_model("claude-sonnet-4-6"));
        assert!(!is_image_model("gemini-2.5-flash"));
    }

    #[test]
    fn vendor_prefixed_image_model_detected() {
        assert!(is_image_model("openai/gpt-image-1"));
        assert!(is_image_model("azure/dall-e-3"));
    }

    #[test]
    fn chat_model_merely_containing_image_is_not_routed() {
        // These are chat/vision models — a substring match would misroute them to
        // the image endpoint and strip their tool access.
        assert!(!is_image_model("qwen2-vl-image-instruct"));
        assert!(!is_image_model("llava-image-captioner"));
        assert!(!is_image_model("some-provider/image-reasoner-v2"));
        assert!(!is_image_model("gpt-4o-image-understanding"));
    }
}
