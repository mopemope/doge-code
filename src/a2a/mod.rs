pub mod types;

use crate::config::AppConfig;
use crate::llm::types::ToolDef;
use uuid::Uuid;

// Re-export types
pub use types::{AgentCapability, AgentCard};

/// Helper to generate an Agent Card from configuration
pub fn generate_agent_card(config: &AppConfig, tools: Vec<ToolDef>) -> AgentCard {
    let project_name = config
        .project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("default");

    let agent_name = format!("Doge-Code ({})", project_name);

    let mut card = AgentCard::new(
        Uuid::new_v4().to_string(), // TODO: Persist UUID in config or file
        agent_name,
        "An intelligent coding agent capable of reading, analyzing, and modifying codebases."
            .to_string(),
    );

    // Add capabilities (could be dynamic based on config)
    card.capabilities.push(AgentCapability {
        name: "code_analysis".to_string(),
        description: "Analyze code structure and logic".to_string(),
        tags: vec!["analysis".to_string(), "parsing".to_string()],
    });

    card.capabilities.push(AgentCapability {
        name: "file_manipulation".to_string(),
        description: "Read and write files in the project".to_string(),
        tags: vec!["fs".to_string(), "io".to_string()],
    });

    // Add tools
    card.tools = tools;

    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    #[test]
    fn test_generate_agent_card() {
        let config = AppConfig::default();
        let tools = vec![];
        let card = generate_agent_card(&config, tools);

        assert!(!card.uuid.is_empty());
        assert!(card.name.starts_with("Doge-Code"));
        assert!(!card.capabilities.is_empty());
        assert!(card.capabilities.iter().any(|c| c.name == "code_analysis"));
        assert_eq!(card.protocol_version, "0.1.0");
    }
}
