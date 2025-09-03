use crate::planning::task_types::*;

pub(crate) fn decompose_code_edit(_input: &str) -> Vec<TaskStep> {
    vec![
        TaskStep::new(
            "analyze_target".to_string(),
            "Analyze target code".to_string(),
            StepType::Analysis,
            vec!["fs_read".to_string(), "get_symbol_info".to_string()],
        )
        .with_duration(60)
        .with_validation(vec!["File exists".to_string()]),
        TaskStep::new(
            "plan_changes".to_string(),
            "Create change plan".to_string(),
            StepType::Planning,
            vec!["get_symbol_info".to_string()],
        )
        .with_dependencies(vec!["analyze_target".to_string()])
        .with_duration(90),
        TaskStep::new(
            "implement_changes".to_string(),
            "Implement changes".to_string(),
            StepType::Implementation,
            vec!["edit".to_string()],
        )
        .with_dependencies(vec!["plan_changes".to_string()])
        .with_duration(180)
        .with_validation(vec!["No syntax errors".to_string()]),
        TaskStep::new(
            "validate_changes".to_string(),
            "Validate changes".to_string(),
            StepType::Validation,
            vec!["execute_bash".to_string()],
        )
        .with_dependencies(vec!["implement_changes".to_string()])
        .with_duration(120)
        .with_validation(vec!["Compilation successful".to_string()]),
    ]
}
