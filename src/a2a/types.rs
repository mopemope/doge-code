use crate::llm::types::ToolDef;
use serde::{Deserialize, Serialize};

/// Represents an agent's identity and capabilities (The Agent Card)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    /// Unique identifier for the agent (UUID)
    pub uuid: String,

    /// Display name of the agent
    pub name: String,

    /// Concise description of the agent's role and expertise
    pub description: String,

    /// List of high-level capabilities provided by the agent
    pub capabilities: Vec<AgentCapability>,

    /// List of tools exposed by the agent (MCP format)
    pub tools: Vec<ToolDef>,

    /// Version of the A2A protocol supported
    pub protocol_version: String,
}

/// Represents a specific capability of an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCapability {
    /// Name of the capability (e.g., "code_analysis", "file_manipulation")
    pub name: String,

    /// Description of what this capability entails
    pub description: String,

    /// Keywords or tags associated with this capability
    pub tags: Vec<String>,
}

impl AgentCard {
    pub fn new(uuid: String, name: String, description: String) -> Self {
        Self {
            uuid,
            name,
            description,
            capabilities: Vec::new(),
            tools: Vec::new(),
            protocol_version: "0.1.0".to_string(), // Draft version
        }
    }
}
