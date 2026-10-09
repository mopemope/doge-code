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

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ReasoningPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<serde_json::Value>,
}

impl std::fmt::Debug for ReasoningPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReasoningPayload([REDACTED])")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    #[serde(default, flatten)]
    pub reasoning: ReasoningPayload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_state: Option<crate::features::openai_subscription::ProviderState>,
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
    #[serde(default)]
    pub finish_reason: Option<String>,
}

/// Some compatible endpoints omit finish_reason. Preserve that compatibility,
/// but never accept an explicitly incomplete or mismatched generation.
pub(crate) fn validate_completion(reason: Option<&str>, has_tools: bool) -> anyhow::Result<()> {
    match reason {
        None => Ok(()),
        Some("stop") if !has_tools => Ok(()),
        Some("tool_calls") if has_tools => Ok(()),
        _ => Err(anyhow::anyhow!(crate::llm::LlmErrorKind::Incomplete)
            .context("provider did not return a complete response")),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CompletionTokensDetails {
    #[serde(default)]
    pub reasoning_tokens: Option<u32>,
    /// Preserve unknown provider fields without failing deserialization.
    #[serde(default, flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PromptTokensDetails {
    #[serde(default)]
    pub cached_tokens: Option<u32>,
    #[serde(default)]
    pub cache_write_tokens: Option<u32>,
    /// Preserve unknown provider fields (e.g. `audio_tokens`,
    /// `text_tokens`, `image_tokens`, future metrics) without failing
    /// deserialization.
    #[serde(default, flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    #[serde(default)]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
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
    fn test_completion_gate_rejects_incomplete_and_mismatched_responses() {
        for reason in ["length", "content_filter", "", "unknown"] {
            assert!(validate_completion(Some(reason), false).is_err());
            assert!(validate_completion(Some(reason), true).is_err());
        }
        assert!(validate_completion(Some("stop"), false).is_ok());
        assert!(validate_completion(Some("tool_calls"), true).is_ok());
        assert!(validate_completion(Some("stop"), true).is_err());
        assert!(validate_completion(Some("tool_calls"), false).is_err());
        assert!(
            validate_completion(None, true).is_ok(),
            "legacy compatible endpoint"
        );
    }

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

    #[test]
    fn usage_deserializes_with_cached_tokens() {
        let payload = r#"{"prompt_tokens":10000,"completion_tokens":100,"total_tokens":10100,"prompt_tokens_details":{"cached_tokens":8000}}"#;
        let usage: Usage = serde_json::from_str(payload).unwrap();
        assert_eq!(
            usage
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens),
            Some(8000)
        );
    }

    #[test]
    fn usage_deserializes_with_explicit_zero_cached_tokens() {
        let payload = r#"{"prompt_tokens":10000,"completion_tokens":100,"total_tokens":10100,"prompt_tokens_details":{"cached_tokens":0}}"#;
        let usage: Usage = serde_json::from_str(payload).unwrap();
        // Explicit zero must stay Some(0), never normalized to None.
        assert_eq!(
            usage
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens),
            Some(0)
        );
        assert!(usage.prompt_tokens_details.is_some());
    }

    #[test]
    fn usage_deserializes_without_prompt_details() {
        let payload = r#"{"prompt_tokens":10000,"completion_tokens":100,"total_tokens":10100}"#;
        let usage: Usage = serde_json::from_str(payload).unwrap();
        assert!(usage.prompt_tokens_details.is_none());
    }

    #[test]
    fn usage_deserializes_with_cache_write_tokens() {
        let payload = r#"{"prompt_tokens":10000,"completion_tokens":100,"total_tokens":10100,"prompt_tokens_details":{"cached_tokens":0,"cache_write_tokens":9000}}"#;
        let usage: Usage = serde_json::from_str(payload).unwrap();
        let details = usage.prompt_tokens_details.as_ref().expect("details");
        assert_eq!(details.cached_tokens, Some(0));
        assert_eq!(details.cache_write_tokens, Some(9000));
    }

    #[test]
    fn usage_deserializes_with_unknown_prompt_detail_fields() {
        let payload = r#"{"prompt_tokens":10000,"completion_tokens":100,"total_tokens":10100,"prompt_tokens_details":{"cached_tokens":100,"some_future_metric":123}}"#;
        let usage: Usage = serde_json::from_str(payload).unwrap();
        let details = usage.prompt_tokens_details.as_ref().expect("details");
        assert_eq!(details.cached_tokens, Some(100));
        assert_eq!(
            details.extra.get("some_future_metric"),
            Some(&serde_json::json!(123))
        );
    }
}
