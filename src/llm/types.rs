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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
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
}
