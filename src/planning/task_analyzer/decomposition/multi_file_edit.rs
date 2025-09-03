use crate::planning::task_types::*;

pub(crate) fn decompose_multi_file_edit(_input: &str) -> Vec<TaskStep> {
    vec![
        TaskStep::new(
            "analyze_project_structure".to_string(),
            "Analyze project structure".to_string(),
            StepType::Analysis,
            vec!["search_repomap".to_string(), "get_symbol_info".to_string()],
        )
        .with_duration(120),
        TaskStep::new(
            "identify_affected_files".to_string(),
            "Identify affected files".to_string(),
            StepType::Analysis,
            vec!["search_text".to_string()],
        )
        .with_dependencies(vec!["analyze_project_structure".to_string()])
        .with_duration(90),
        TaskStep::new(
            "plan_coordinated_changes".to_string(),
            "Create coordinated change plan".to_string(),
            StepType::Planning,
            vec!["get_symbol_info".to_string()],
        )
        .with_dependencies(vec!["identify_affected_files".to_string()])
        .with_duration(180),
        TaskStep::new(
            "implement_changes_sequentially".to_string(),
            "Implement changes sequentially".to_string(),
            StepType::Implementation,
            vec!["edit".to_string(), "fs_write".to_string()],
        )
        .with_dependencies(vec!["plan_coordinated_changes".to_string()])
        .with_duration(300),
        TaskStep::new(
            "validate_integration".to_string(),
            "Execute integration tests".to_string(),
            StepType::Validation,
            vec!["execute_bash".to_string()],
        )
        .with_dependencies(vec!["implement_changes_sequentially".to_string()])
        .with_duration(180),
    ]
}
