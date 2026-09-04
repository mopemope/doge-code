use crate::llm::tool_def::default_tools_def;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use crate::tools::FsTools;
use crate::tools::remote_tools::RemoteToolInfo;
use anyhow::Result;
use tokio_util::sync::CancellationToken;
use tracing::debug;

const MAX_ITERS: usize = 256;

pub struct ToolRuntime<'a> {
    pub tools: Vec<ToolDef>,
    pub fs: &'a FsTools,
    // repomap is delegated to FsTools, removed here
    pub max_iters: usize,
    /// LLM client used by the `task` sub-agent (same client as the main loop,
    /// so token usage accumulates in one place).
    pub subagent_client: Option<crate::llm::client_core::OpenAIClient>,
    /// Model id passed to the sub-agent loop.
    pub subagent_model: String,
    /// Cancellation token propagated to the sub-agent loop.
    pub cancel_token: Option<CancellationToken>,
}

impl<'a> ToolRuntime<'a> {
    pub async fn build(
        fs: &'a FsTools,
        subagent_client: Option<crate::llm::client_core::OpenAIClient>,
        subagent_model: impl Into<String>,
        cancel_token: Option<CancellationToken>,
    ) -> Result<Self> {
        fs.get_remote_tool_manager().ensure_remote_tools().await?;
        let remote_tools = fs.get_remote_tool_manager().remote_tools_snapshot().await;

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
            subagent_client,
            subagent_model: subagent_model.into(),
            cancel_token,
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
