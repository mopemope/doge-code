use crate::planning::task_types::*;

pub(crate) fn decompose_file_operation(input: &str) -> Vec<TaskStep> {
    if input.contains("読") || input.contains("read") || input.contains("表示") {
        vec![
            TaskStep::new(
                "identify_target".to_string(),
                "Identify target file".to_string(),
                StepType::Analysis,
                vec!["find_file".to_string()],
            )
            .with_duration(30),
            TaskStep::new(
                "read_file".to_string(),
                "Read file".to_string(),
                StepType::Implementation,
                vec!["fs_read".to_string()],
            )
            .with_dependencies(vec!["identify_target".to_string()])
            .with_duration(30),
        ]
    } else {
        vec![
            TaskStep::new(
                "prepare_content".to_string(),
                "Prepare write content".to_string(),
                StepType::Planning,
                vec![],
            )
            .with_duration(60),
            TaskStep::new(
                "write_file".to_string(),
                "Write to file".to_string(),
                StepType::Implementation,
                vec!["fs_write".to_string()],
            )
            .with_dependencies(vec!["prepare_content".to_string()])
            .with_duration(60),
        ]
    }
}
