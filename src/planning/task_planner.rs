use crate::planning::task_types::*;
use uuid::Uuid;

/// Create task plan
pub fn create_task_plan(
    original_request: String,
    classification: TaskClassification,
    steps: Vec<TaskStep>,
) -> TaskPlan {
    let total_duration = steps.iter().map(|s| s.estimated_duration).sum();

    TaskPlan {
        id: Uuid::new_v4().to_string(),
        original_request,
        classification,
        steps,
        total_estimated_duration: total_duration,
        created_at: chrono::Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_task_plan() {
        let classification = TaskClassification {
            task_type: TaskType::LargeRefactoring,
            complexity_score: 0.8,
            estimated_steps: 3,
            risk_level: RiskLevel::Medium,
            required_tools: vec!["fs_read".to_string(), "edit".to_string()],
            confidence: 0.85,
        };

        let steps = vec![
            TaskStep::new(
                "step1".to_string(),
                "Step 1".to_string(),
                StepType::Analysis,
                vec!["fs_read".to_string()],
            )
            .with_duration(300),
            TaskStep::new(
                "step2".to_string(),
                "Step 2".to_string(),
                StepType::Implementation,
                vec!["edit".to_string()],
            )
            .with_duration(600),
        ];

        let plan = create_task_plan("Refactor the codebase".to_string(), classification, steps);

        assert_eq!(plan.original_request, "Refactor the codebase");
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.total_estimated_duration, 900); // 300 + 600
        assert!(!plan.id.is_empty());
    }
}
