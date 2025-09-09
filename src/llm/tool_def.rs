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
        tools::edit::tool_def(),
        tools::apply_patch::tool_def(),
        tools::find_file::tool_def(),
        tools::read_many::tool_def(),
        tools::todo_write::tool_def(),
    ]
}
