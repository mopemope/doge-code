use serde::{Deserialize, Serialize};

/// LLM decomposition result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmDecompositionResult {
    pub reasoning: String,
    pub steps: Vec<LlmTaskStep>,
    pub complexity_assessment: String,
    pub risks: Vec<String>,
    pub prerequisites: Vec<String>,
}

/// Single step emitted (or inferred) by the LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmTaskStep {
    pub id: String,
    pub description: String,
    /// "analysis", "planning", "implementation", "validation", "cleanup"
    pub step_type: String,
    pub dependencies: Vec<String>,
    pub estimated_duration_minutes: u32,
    pub required_tools: Vec<String>,
    pub validation_criteria: Vec<String>,
    pub detailed_instructions: String,
    pub potential_issues: Vec<String>,
}

/// Context information about the project used for decomposition prompts
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectContext {
    pub project_type: String,
    pub main_languages: Vec<String>,
    pub key_files: Vec<String>,
    pub architecture_notes: String,
    pub recent_changes: Vec<String>,
}
