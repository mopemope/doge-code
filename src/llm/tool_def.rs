use crate::llm::types::ToolDef;
use crate::tools;

pub fn default_tools_def() -> Vec<ToolDef> {
    vec![
        tools::list::tool_def(),
        tools::read::tool_def(),
        tools::search_text::tool_def(),
        tools::write::tool_def(),
        tools::search_repomap::tool_def(),
        tools::execute::tool_def(),
        tools::process::tool_def(),
        tools::shell::tool_def(),
        tools::edit::tool_def(),
        tools::apply_patch::tool_def(),
        tools::find_file::tool_def(),
        tools::read_many::tool_def(),
        tools::plan::plan_write_tool_def(),
        tools::plan::plan_read_tool_def(),
        tools::requirements::requirements_write_tool_def(),
        tools::requirements::requirements_read_tool_def(),
        tools::undo::undo_tool_def(),
        tools::memory::read_memory_tool_def(),
        tools::memory::write_memory_tool_def(),
        tools::memory::list_memories_tool_def(),
        tools::memory::search_memory_tool_def(),
        tools::doc::tool_def(),
        tools::workflow::run_workflow_tool_def(),
        tools::task::tool_def(),
        tools::provenance::tool_def(),
        tools::impact::tool_def(),
        tools::observation::tool_def(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_observation_read_schema_is_small_and_stable() {
        // Fixed prompt ratchet: the new stable tool adds a small deliberate
        // cost. Keep the schema concise; fail if it grows unexpectedly.
        let def = crate::tools::observation::tool_def();
        let rendered = serde_json::to_string(&def).expect("serialize tool def");
        assert!(
            rendered.len() < 1_500,
            "observation_read schema grew: {} bytes",
            rendered.len()
        );
        // Full inventory stays parseable and observation_read is appended last.
        let all = default_tools_def();
        assert_eq!(all.last().unwrap().function.name, "observation_read");
        let all_json = serde_json::to_string(&all).expect("serialize all");
        assert!(all_json.contains("observation_read"));
    }
}
