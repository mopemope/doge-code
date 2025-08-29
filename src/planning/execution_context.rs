use crate::planning::task_types::StepResult;
use std::collections::HashMap;

/// Execution context
#[derive(Debug)]
pub struct ExecutionContext {
    completed_steps: HashMap<String, StepResult>,
    artifacts: Vec<String>,
    start_time: chrono::DateTime<chrono::Utc>,
}

impl Default for ExecutionContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionContext {
    pub fn new() -> Self {
        Self {
            completed_steps: HashMap::new(),
            artifacts: Vec::new(),
            start_time: chrono::Utc::now(),
        }
    }

    pub fn mark_completed(&mut self, step_id: &str, result: StepResult) {
        self.completed_steps.insert(step_id.to_string(), result);
    }

    pub fn is_completed(&self, step_id: &str) -> bool {
        self.completed_steps.contains_key(step_id)
    }

    pub fn get_result(&self, step_id: &str) -> Option<&StepResult> {
        self.completed_steps.get(step_id)
    }

    pub fn completed_steps(&self) -> Vec<StepResult> {
        self.completed_steps.values().cloned().collect()
    }

    pub fn artifacts(&self) -> Vec<String> {
        self.artifacts.clone()
    }

    pub fn add_artifact(&mut self, artifact: String) {
        self.artifacts.push(artifact);
    }

    pub fn start_time(&self) -> chrono::DateTime<chrono::Utc> {
        self.start_time
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::task_types::StepResult;

    #[test]
    fn test_execution_context() {
        let mut context = ExecutionContext::new();

        // Initial state
        assert!(!context.is_completed("step1"));
        assert!(context.completed_steps().is_empty());

        // Record step completion
        let result = StepResult {
            step_id: "step1".to_string(),
            success: true,
            output: "Success".to_string(),
            artifacts: vec!["file1.rs".to_string()],
            duration: 150,
            error_message: None,
        };

        context.mark_completed("step1", result.clone());

        // Verify completion state
        assert!(context.is_completed("step1"));
        assert_eq!(context.completed_steps().len(), 1);
        assert_eq!(context.get_result("step1").unwrap().step_id, "step1");

        // Add artifacts
        context.add_artifact("new_file.rs".to_string());
        assert!(context.artifacts().contains(&"new_file.rs".to_string()));
    }
}
