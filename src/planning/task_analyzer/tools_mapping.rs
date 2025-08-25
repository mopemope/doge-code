use crate::planning::task_types::TaskType;
use std::collections::HashMap;

pub(crate) fn initialize_tool_mappings(tool_mappings: &mut HashMap<TaskType, Vec<String>>) {
    tool_mappings.insert(
        TaskType::SimpleFileOperation,
        vec![
            "fs_read".to_string(),
            "fs_write".to_string(),
            "fs_list".to_string(),
        ],
    );

    tool_mappings.insert(
        TaskType::SimpleSearch,
        vec![
            "search_text".to_string(),
            "find_file".to_string(),
            "get_symbol_info".to_string(),
        ],
    );

    tool_mappings.insert(
        TaskType::SimpleCodeEdit,
        vec![
            "fs_read".to_string(),
            "edit".to_string(),
            "execute_bash".to_string(),
        ],
    );

    tool_mappings.insert(
        TaskType::MultiFileEdit,
        vec![
            "fs_read".to_string(),
            "edit".to_string(),
            "search_text".to_string(),
            "execute_bash".to_string(),
        ],
    );

    tool_mappings.insert(
        TaskType::Refactoring,
        vec![
            "fs_read".to_string(),
            "fs_write".to_string(),
            "edit".to_string(),
            "search_text".to_string(),
            "get_symbol_info".to_string(),
            "execute_bash".to_string(),
        ],
    );

    tool_mappings.insert(
        TaskType::FeatureImplementation,
        vec![
            "fs_read".to_string(),
            "fs_write".to_string(),
            "edit".to_string(),
            "get_symbol_info".to_string(),
            "execute_bash".to_string(),
        ],
    );
}

pub(crate) fn get_required_tools(
    tool_mappings: &HashMap<TaskType, Vec<String>>,
    task_type: &TaskType,
) -> Vec<String> {
    tool_mappings
        .get(task_type)
        .cloned()
        .unwrap_or_else(|| vec!["fs_read".to_string()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::task_types::TaskType;
    use std::collections::HashMap;

    #[test]
    fn test_get_required_tools_default_and_specific() {
        let mut mapping = HashMap::new();
        initialize_tool_mappings(&mut mapping);

        let tools = get_required_tools(&mapping, &TaskType::SimpleSearch);
        assert!(tools.contains(&"search_text".to_string()));

        let tools = get_required_tools(&mapping, &TaskType::Unknown);
        assert_eq!(tools, vec!["fs_read".to_string()]);
    }
}
