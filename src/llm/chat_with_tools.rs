use serde::{Deserialize, Serialize};

use crate::llm::types::{ChatMessage, ToolCall, ToolDef, Usage};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequestWithTools {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>, // {"type":"auto"}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChoiceMessageWithTools {
    pub role: String,
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChoiceWithTools {
    pub index: usize,
    pub message: ChoiceMessageWithTools,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponseWithTools {
    pub id: Option<String>,
    pub choices: Vec<ChoiceWithTools>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMessagePayload {
    pub role: String,    // "tool"
    pub content: String, // JSON string content
    pub tool_call_id: Option<String>,
    pub name: Option<String>,
}
