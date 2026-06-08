use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Record {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub timestamp: String,
    pub cwd: Option<String>,
    pub message: Message,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub id: String,
    pub model: Option<String>,
    pub role: String,
    pub usage: Option<Usage>,
    #[serde(default)]
    pub content: Vec<ContentBlock>,
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
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                ContentBlock::Thinking { thinking } => Some(thinking.clone()),
                ContentBlock::ToolUse { input } => Some(input.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASSISTANT_LINE: &str = r#"{
        "sessionId": "sess-test",
        "timestamp": "2025-01-01T00:00:00Z",
        "message": {
            "id": "msg-001",
            "model": "claude-sonnet-4-6",
            "role": "assistant",
            "content": [{"type": "text", "text": "hello"}],
            "usage": {
                "input_tokens": 100,
                "output_tokens": 20,
                "cache_read_input_tokens": 500,
                "cache_creation_input_tokens": 0
            }
        }
    }"#;

    const USER_LINE: &str = r#"{
        "sessionId": "sess-test",
        "timestamp": "2025-01-01T00:00:00Z",
        "message": {
            "id": "msg-002",
            "role": "user",
            "content": [{"type": "text", "text": "hi"}]
        }
    }"#;

    #[test]
    fn assistant_with_usage_is_billable() {
        let r: Record = serde_json::from_str(ASSISTANT_LINE).unwrap();
        assert!(r.is_billable());
    }

    #[test]
    fn user_message_is_not_billable() {
        let r: Record = serde_json::from_str(USER_LINE).unwrap();
        assert!(!r.is_billable());
    }

    #[test]
    fn assistant_without_usage_is_not_billable() {
        let json = r#"{
            "sessionId": "s", "timestamp": "2025-01-01T00:00:00Z",
            "message": {"id": "m", "role": "assistant", "content": []}
        }"#;
        let r: Record = serde_json::from_str(json).unwrap();
        assert!(!r.is_billable());
    }

    #[test]
    fn user_with_usage_is_not_billable() {
        // Unusual but must not be counted — only assistant role bills
        let json = r#"{
            "sessionId": "s", "timestamp": "2025-01-01T00:00:00Z",
            "message": {
                "id": "m", "role": "user", "content": [],
                "usage": {"input_tokens": 5, "output_tokens": 0,
                          "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}
            }
        }"#;
        let r: Record = serde_json::from_str(json).unwrap();
        assert!(!r.is_billable());
    }

    #[test]
    fn deserialize_usage_fields() {
        let r: Record = serde_json::from_str(ASSISTANT_LINE).unwrap();
        let u = r.message.usage.as_ref().unwrap();
        assert_eq!(u.input_tokens, 100);
        assert_eq!(u.output_tokens, 20);
        assert_eq!(u.cache_read_input_tokens, 500);
        assert_eq!(u.cache_creation_input_tokens, 0);
    }

    #[test]
    fn unknown_content_type_deserializes_as_other() {
        let json = r#"{
            "sessionId": "s", "timestamp": "2025-01-01T00:00:00Z",
            "message": {"id": "m", "role": "user",
                        "content": [{"type": "image", "data": "..."}]}
        }"#;
        let r: Record = serde_json::from_str(json).unwrap();
        assert!(matches!(r.message.content[0], ContentBlock::Other));
    }

    #[test]
    fn bad_json_returns_err_not_panic() {
        let result = serde_json::from_str::<Record>("this is not json at all");
        assert!(result.is_err());
    }

    #[test]
    fn text_output_joins_text_and_thinking_blocks() {
        let r: Record = serde_json::from_str(ASSISTANT_LINE).unwrap();
        assert_eq!(r.message.text_output(), "hello");
    }
}
