use serde::{Deserialize, Serialize};

/// A single message in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: MessageContent,
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
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: MessageContent::Text(text.into()),
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: MessageContent::Text(text.into()),
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: MessageContent::Text(text.into()),
        }
    }

    /// A tool-result message. Stored with role "tool" for distinct rendering;
    /// `Session::api_messages` re-maps it to "user" when sending to the API so
    /// OpenAI-compatible endpoints accept it.
    pub fn tool(text: impl Into<String>) -> Self {
        Self {
            role: "tool".to_string(),
            content: MessageContent::Text(text.into()),
        }
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
        Self {
            role: "user".to_string(),
            content: MessageContent::Parts(parts),
        }
    }

}

/// The request body sent to the OpenAI-compatible endpoint.
#[derive(Debug, Serialize)]
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
}

#[derive(Debug, Serialize)]
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
            stream_options: Some(StreamOptions { include_usage: true }),
            reasoning_effort: None,
        }
    }

    /// Set the reasoning effort ("low"/"medium"/"high"); None clears it.
    pub fn with_reasoning_effort(mut self, effort: Option<String>) -> Self {
        self.reasoning_effort = effort;
        self
    }
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
}
