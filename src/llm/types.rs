use serde::{Deserialize, Serialize};

fn default_tool_arguments() -> String {
    "{}".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value, // JSON Schema object
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub kind: String, // "function"
    pub function: ToolFunctionDef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    #[serde(default = "default_tool_arguments")]
    pub arguments: String, // JSON string per OpenAI spec
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: Option<String>,
    pub r#type: String, // "function"
    pub function: ToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChoiceMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub index: usize,
    pub message: ChoiceMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CompletionTokensDetails {
    #[serde(default)]
    pub reasoning_tokens: Option<u32>,
    /// Preserve unknown provider fields without failing deserialization.
    #[serde(default, flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    #[serde(default)]
    pub completion_tokens_details: Option<CompletionTokensDetails>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub id: Option<String>,
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_call_function_defaults_arguments_to_empty_object() {
        let payload = r#"{"name":"plan_read"}"#;
        let parsed: ToolCallFunction = serde_json::from_str(payload).unwrap();
        assert_eq!(parsed.arguments, "{}");
    }

    #[test]
    fn usage_deserializes_with_reasoning_details() {
        let payload = r#"{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150,"completion_tokens_details":{"reasoning_tokens":30}}"#;
        let usage: Usage = serde_json::from_str(payload).unwrap();
        assert_eq!(
            usage
                .completion_tokens_details
                .as_ref()
                .and_then(|d| d.reasoning_tokens),
            Some(30)
        );
    }

    #[test]
    fn usage_deserializes_without_details() {
        let payload = r#"{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150}"#;
        let usage: Usage = serde_json::from_str(payload).unwrap();
        assert!(usage.completion_tokens_details.is_none());
    }

    #[test]
    fn usage_deserializes_with_zero_reasoning_tokens() {
        let payload = r#"{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150,"completion_tokens_details":{"reasoning_tokens":0}}"#;
        let usage: Usage = serde_json::from_str(payload).unwrap();
        assert_eq!(
            usage
                .completion_tokens_details
                .as_ref()
                .and_then(|d| d.reasoning_tokens),
            Some(0)
        );
    }
}
