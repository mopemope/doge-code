use std::time::Duration;

use crate::llm::tool_def::ToolDef;
use crate::tools::FsTools;

const MAX_ITERS: usize = 128;

pub struct ToolRuntime<'a> {
    pub tools: Vec<ToolDef>,
    pub fs: &'a FsTools,
    pub max_iters: usize,
    pub request_timeout: Duration,
    pub tool_timeout: Duration,
}

impl<'a> ToolRuntime<'a> {
    pub fn default_with(fs: &'a FsTools) -> Self {
        Self {
            tools: crate::llm::tool_def::default_tools_def(),
            fs,
            max_iters: MAX_ITERS,
            request_timeout: Duration::from_secs(60 * 5),
            tool_timeout: Duration::from_secs(10 * 60),
        }
    }
}
