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
        tools::shell::tool_def(),
        tools::edit::tool_def(),
        tools::apply_patch::tool_def(),
        tools::find_file::tool_def(),
        tools::read_many::tool_def(),
        tools::plan::plan_write_tool_def(),
        tools::plan::plan_read_tool_def(),
        tools::undo::undo_tool_def(),
        tools::memory::read_memory_tool_def(),
        tools::memory::write_memory_tool_def(),
        tools::memory::list_memories_tool_def(),
        tools::doc::tool_def(),
        tools::history::tool_def(),
    ]
}
