use crate::planning::task_types::*;

pub(crate) fn decompose_feature_implementation(_input: &str) -> Vec<TaskStep> {
    vec![
        TaskStep::new(
            "analyze_requirements".to_string(),
            "Analyze requirements".to_string(),
            StepType::Analysis,
            vec!["get_symbol_info".to_string()],
        )
        .with_duration(120),
        TaskStep::new(
            "design_architecture".to_string(),
            "Design architecture".to_string(),
            StepType::Planning,
            vec!["search_repomap".to_string()],
        )
        .with_dependencies(vec!["analyze_requirements".to_string()])
        .with_duration(180),
        TaskStep::new(
            "implement_core_logic".to_string(),
            "Implement core logic".to_string(),
            StepType::Implementation,
            vec!["fs_write".to_string(), "edit".to_string()],
        )
        .with_dependencies(vec!["design_architecture".to_string()])
        .with_duration(480),
        TaskStep::new(
            "implement_interfaces".to_string(),
            "Implement interfaces".to_string(),
            StepType::Implementation,
            vec!["edit".to_string()],
        )
        .with_dependencies(vec!["implement_core_logic".to_string()])
        .with_duration(240),
        TaskStep::new(
            "add_tests".to_string(),
            "Add tests".to_string(),
            StepType::Implementation,
            vec!["fs_write".to_string()],
        )
        .with_dependencies(vec!["implement_interfaces".to_string()])
        .with_duration(300),
        TaskStep::new(
            "validate_feature".to_string(),
            "Validate feature".to_string(),
            StepType::Validation,
            vec!["execute_bash".to_string()],
        )
        .with_dependencies(vec!["add_tests".to_string()])
        .with_duration(180),
        TaskStep::new(
            "integration_test".to_string(),
            "Execute integration tests".to_string(),
            StepType::Validation,
            vec!["execute_bash".to_string()],
        )
        .with_dependencies(vec!["validate_feature".to_string()])
        .with_duration(240),
    ]
}
