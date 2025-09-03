use crate::planning::task_types::*;

pub(crate) fn decompose_search_task(_input: &str) -> Vec<TaskStep> {
    vec![
        TaskStep::new(
            "define_search_criteria".to_string(),
            "Define search criteria".to_string(),
            StepType::Planning,
            vec![],
        )
        .with_duration(30),
        TaskStep::new(
            "execute_search".to_string(),
            "Execute search".to_string(),
            StepType::Implementation,
            vec!["search_text".to_string(), "find_file".to_string()],
        )
        .with_dependencies(vec!["define_search_criteria".to_string()])
        .with_duration(60),
        TaskStep::new(
            "analyze_results".to_string(),
            "Analyze search results".to_string(),
            StepType::Analysis,
            vec!["get_symbol_info".to_string()],
        )
        .with_dependencies(vec!["execute_search".to_string()])
        .with_duration(90),
    ]
}
