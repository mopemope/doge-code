use crate::planning::task_types::*;

pub(crate) fn decompose_refactoring(_input: &str) -> Vec<TaskStep> {
    vec![
        TaskStep::new(
            "analyze_current_structure".to_string(),
            "Analyze current code structure in detail".to_string(),
            StepType::Analysis,
            vec!["search_repomap".to_string(), "fs_read".to_string()],
        )
        .with_duration(180),
        TaskStep::new(
            "identify_refactoring_opportunities".to_string(),
            "Identify refactoring opportunities".to_string(),
            StepType::Analysis,
            vec!["get_symbol_info".to_string()],
        )
        .with_dependencies(vec!["analyze_current_structure".to_string()])
        .with_duration(120),
        TaskStep::new(
            "design_new_structure".to_string(),
            "Design new structure".to_string(),
            StepType::Planning,
            vec!["get_symbol_info".to_string()],
        )
        .with_dependencies(vec!["identify_refactoring_opportunities".to_string()])
        .with_duration(240),
        TaskStep::new(
            "implement_refactoring".to_string(),
            "Implement refactoring".to_string(),
            StepType::Implementation,
            vec!["edit".to_string(), "fs_write".to_string()],
        )
        .with_dependencies(vec!["design_new_structure".to_string()])
        .with_duration(600),
        TaskStep::new(
            "update_dependencies".to_string(),
            "Update dependencies".to_string(),
            StepType::Implementation,
            vec!["search_text".to_string(), "edit".to_string()],
        )
        .with_dependencies(vec!["implement_refactoring".to_string()])
        .with_duration(300),
        TaskStep::new(
            "run_comprehensive_tests".to_string(),
            "Execute comprehensive tests".to_string(),
            StepType::Validation,
            vec!["execute_bash".to_string()],
        )
        .with_dependencies(vec!["update_dependencies".to_string()])
        .with_duration(240),
    ]
}
