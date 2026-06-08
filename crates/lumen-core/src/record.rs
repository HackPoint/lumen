use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Record {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub timestamp: String,
    pub cwd: Option<String>,
    pub message: Message
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub id: String,
    pub model: Option<String>,
    pub role: String,
    pub usage: Option<Usage>,
    #[serde(default)]
    pub content: Vec<ContentBlock>
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "tool_use")]
    ToolUse { input: serde_json::Value },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub struct Usage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cache_creation: Option<CacheCreation>,
    pub server_tool_use: Option<ServerToolUse>,
    pub service_tier: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CacheCreation {
    pub ephemeral_1h_input_tokens: Option<i64>,
    pub ephemeral_5m_input_tokens: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct ServerToolUse {
    pub web_search_requests: Option<i64>,
    pub web_fetch_requests: Option<i64>,
}

impl Record {
    /// Frozen contract: only assistant lines with usage count.
    pub fn is_billable(&self) -> bool {
        self.message.role == "assistant" && self.message.usage.is_some()
    }
}

impl Message {
    pub fn text_output(&self) -> String {
        self.content.iter().filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            ContentBlock::Thinking { thinking } => Some(thinking.clone()),
            ContentBlock::ToolUse { input } => Some(input.to_string()),
            _ => None,
        }).collect::<Vec<_>>().join(" ")
    }
}