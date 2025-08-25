use serde::{Deserialize, Serialize};

/// Enumeration representing task types
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskType {
    /// Simple file operations (read, write, search)
    SimpleFileOperation,
    /// Small edits to a single file
    SimpleCodeEdit,
    /// Search and investigation tasks
    SimpleSearch,
    /// Changes to multiple files
    MultiFileEdit,
    /// Refactoring
    Refactoring,
    /// New feature implementation
    FeatureImplementation,
    /// Architecture changes
    ArchitecturalChange,
    /// Large-scale refactoring
    LargeRefactoring,
    /// Project structure changes
    ProjectRestructure,
    /// Unknown or unclassifiable
    Unknown,
}

/// Risk level
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

/// Step types
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepType {
    /// Analysis and investigation
    Analysis,
    /// Planning and design
    Planning,
    /// Implementation
    Implementation,
    /// Validation and testing
    Validation,
    /// Post-processing and cleanup
    Cleanup,
}

/// Task classification result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskClassification {
    pub task_type: TaskType,
    pub complexity_score: f32,
    pub estimated_steps: usize,
    pub risk_level: RiskLevel,
    pub required_tools: Vec<String>,
    pub confidence: f32,
}

/// Execution step
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStep {
    pub id: String,
    pub description: String,
    pub step_type: StepType,
    pub dependencies: Vec<String>,
    pub estimated_duration: u64, // seconds
    pub required_tools: Vec<String>,
    pub validation_criteria: Vec<String>,
    pub prompt_template: Option<String>,
}

/// Task execution plan
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlan {
    pub id: String,
    pub original_request: String,
    pub classification: TaskClassification,
    pub steps: Vec<TaskStep>,
    pub total_estimated_duration: u64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Step execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub step_id: String,
    pub success: bool,
    pub output: String,
    pub artifacts: Vec<String>, // Generated file paths, etc.
    pub duration: u64,
    pub error_message: Option<String>,
}

/// Task execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub plan_id: String,
    pub success: bool,
    pub completed_steps: Vec<StepResult>,
    pub total_duration: u64,
    pub final_message: String,
}

impl TaskType {
    /// Returns the complexity of the task type (0.0-1.0)
    pub fn base_complexity(&self) -> f32 {
        match self {
            TaskType::SimpleFileOperation => 0.1,
            TaskType::SimpleCodeEdit => 0.2,
            TaskType::SimpleSearch => 0.15,
            TaskType::MultiFileEdit => 0.4,
            TaskType::Refactoring => 0.6,
            TaskType::FeatureImplementation => 0.5,
            TaskType::ArchitecturalChange => 0.8,
            TaskType::LargeRefactoring => 0.9,
            TaskType::ProjectRestructure => 1.0,
            TaskType::Unknown => 0.5,
        }
    }

    /// Returns the estimated number of steps
    pub fn estimated_steps(&self) -> usize {
        match self {
            TaskType::SimpleFileOperation => 2,
            TaskType::SimpleCodeEdit => 3,
            TaskType::SimpleSearch => 2,
            TaskType::MultiFileEdit => 5,
            TaskType::Refactoring => 6,
            TaskType::FeatureImplementation => 7,
            TaskType::ArchitecturalChange => 10,
            TaskType::LargeRefactoring => 12,
            TaskType::ProjectRestructure => 15,
            TaskType::Unknown => 4,
        }
    }

    /// Returns the risk level
    pub fn risk_level(&self) -> RiskLevel {
        match self {
            TaskType::SimpleFileOperation => RiskLevel::Low,
            TaskType::SimpleCodeEdit => RiskLevel::Low,
            TaskType::SimpleSearch => RiskLevel::Low,
            TaskType::MultiFileEdit => RiskLevel::Medium,
            TaskType::Refactoring => RiskLevel::Medium,
            TaskType::FeatureImplementation => RiskLevel::Medium,
            TaskType::ArchitecturalChange => RiskLevel::High,
            TaskType::LargeRefactoring => RiskLevel::High,
            TaskType::ProjectRestructure => RiskLevel::Critical,
            TaskType::Unknown => RiskLevel::Medium,
        }
    }
}

impl TaskStep {
    /// Creates a new step
    pub fn new(
        id: String,
        description: String,
        step_type: StepType,
        required_tools: Vec<String>,
    ) -> Self {
        Self {
            id,
            description,
            step_type,
            dependencies: Vec::new(),
            estimated_duration: 60, // Default 1 minute
            required_tools,
            validation_criteria: Vec::new(),
            prompt_template: None,
        }
    }

    /// Adds dependencies
    pub fn with_dependencies(mut self, dependencies: Vec<String>) -> Self {
        self.dependencies = dependencies;
        self
    }

    /// Sets the estimated time
    pub fn with_duration(mut self, duration_secs: u64) -> Self {
        self.estimated_duration = duration_secs;
        self
    }

    /// Adds validation criteria
    pub fn with_validation(mut self, criteria: Vec<String>) -> Self {
        self.validation_criteria = criteria;
        self
    }

    /// Sets the prompt template
    pub fn with_prompt_template(mut self, template: String) -> Self {
        self.prompt_template = Some(template);
        self
    }
}
