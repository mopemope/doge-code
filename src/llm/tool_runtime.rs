use crate::llm::types::ToolDef;
use crate::tools::FsTools;

const MAX_ITERS: usize = 128;

pub struct ToolRuntime<'a> {
    pub tools: Vec<ToolDef>,
    pub fs: &'a FsTools,
    // repomap is delegated to FsTools, removed here
    pub max_iters: usize,
}

impl<'a> ToolRuntime<'a> {
    pub fn new(fs: &'a FsTools) -> Self {
        // repomap parameter removed
        Self {
            tools: crate::llm::tool_def::default_tools_def(),
            fs,
            max_iters: MAX_ITERS,
        }
    }
}
