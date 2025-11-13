use crate::llm::tool_def::default_tools_def;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use crate::tools::{FsTools, RemoteToolInfo};
use anyhow::Result;
use tracing::debug;

const MAX_ITERS: usize = 256;

pub struct ToolRuntime<'a> {
    pub tools: Vec<ToolDef>,
    pub fs: &'a FsTools,
    // repomap is delegated to FsTools, removed here
    pub max_iters: usize,
}

impl<'a> ToolRuntime<'a> {
    pub async fn build(fs: &'a FsTools) -> Result<Self> {
        fs.ensure_remote_tools().await?;
        let remote_tools = fs.remote_tools_snapshot().await;

        let mut tools = default_tools_def();
        append_remote_tools(&mut tools, &remote_tools);

        debug!(
            count = remote_tools.len(),
            "ToolRuntime registered remote MCP tools"
        );

        Ok(Self {
            tools,
            fs,
            max_iters: MAX_ITERS,
        })
    }
}

fn append_remote_tools(tools: &mut Vec<ToolDef>, remote: &[RemoteToolInfo]) {
    for info in remote {
        let description = info.description.clone().unwrap_or_else(|| {
            format!(
                "Remote MCP tool '{}' from server '{}'",
                info.remote_name, info.server_name
            )
        });

        tools.push(ToolDef {
            kind: "function".into(),
            function: ToolFunctionDef {
                name: info.alias.clone(),
                description,
                parameters: info.parameters.clone(),
                strict: info.strict,
            },
        });
    }
}
