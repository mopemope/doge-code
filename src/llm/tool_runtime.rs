use crate::llm::tool_catalog::ToolCatalog;
use crate::llm::tool_def::default_tools_def;
use crate::llm::types::ToolDef;
use crate::provenance::ProvenanceAttribution;
use crate::tools::FsTools;
use anyhow::Result;
use tokio_util::sync::CancellationToken;
use tracing::debug;

const MAX_ITERS: usize = 256;

pub struct ToolRuntime<'a> {
    pub tool_catalog: ToolCatalog,
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
    /// Per-turn provenance attribution (directive id, if any). Propagated to
    /// every tool execution in this turn; never stored globally.
    pub attribution: ProvenanceAttribution,
    /// Conversation-owned Observation Store handle. Cloned from the owning
    /// `HistoryManager` per agent run so `observation_read` retrieves only
    /// this conversation's offloaded results without global state.
    pub observation_store: crate::llm::observation::SharedObservationStore,
}

impl<'a> ToolRuntime<'a> {
    pub async fn build(
        fs: &'a FsTools,
        subagent_client: Option<crate::llm::client_core::OpenAIClient>,
        subagent_model: impl Into<String>,
        cancel_token: Option<CancellationToken>,
    ) -> Result<Self> {
        Self::build_with_attribution(
            fs,
            subagent_client,
            subagent_model,
            cancel_token,
            ProvenanceAttribution::none(),
        )
        .await
    }

    pub async fn build_with_attribution(
        fs: &'a FsTools,
        subagent_client: Option<crate::llm::client_core::OpenAIClient>,
        subagent_model: impl Into<String>,
        cancel_token: Option<CancellationToken>,
        attribution: ProvenanceAttribution,
    ) -> Result<Self> {
        let remote_manager = fs.get_remote_tool_manager();
        if fs.config.mcp_servers.iter().any(|server| server.enabled) {
            remote_manager
                .ensure_remote_tools_with_cancellation(cancel_token.as_ref())
                .await?;
        } else {
            // Preserve runtime construction semantics when no remote server
            // is configured; the per-tool cancellation path still handles an
            // already-cancelled agent token.
            remote_manager.ensure_remote_tools().await?;
        }
        let remote_tools = fs.get_remote_tool_manager().remote_tools_snapshot().await;

        // MCP discovery timing is unchanged; the catalog only controls when
        // discovered schemas become LLM-visible.
        let tool_catalog =
            ToolCatalog::from_parts(default_tools_def(), &remote_tools, &fs.config.tool_routing);

        let active_count = tool_catalog.active_count().await;
        let deferred = tool_catalog.is_deferred();
        debug!(
            count = remote_tools.len(),
            active = active_count,
            deferred,
            "ToolRuntime registered remote MCP tools"
        );

        Ok(Self {
            tool_catalog,
            fs,
            max_iters: MAX_ITERS,
            subagent_client,
            subagent_model: subagent_model.into(),
            cancel_token,
            attribution,
            observation_store: crate::llm::observation::new_shared_store(),
        })
    }

    /// Test-only constructor from an explicit catalog (no MCP discovery).
    #[cfg(test)]
    pub fn from_catalog_for_test(fs: &'a FsTools, tool_catalog: ToolCatalog) -> Self {
        Self {
            tool_catalog,
            fs,
            max_iters: MAX_ITERS,
            subagent_client: None,
            subagent_model: "test-model".to_string(),
            cancel_token: None,
            attribution: ProvenanceAttribution::none(),
            observation_store: crate::llm::observation::new_shared_store(),
        }
    }

    /// Attach the conversation-owned Observation Store handle. Called once
    /// per agent run so `observation_read` sees this run's offloads.
    pub fn set_observation_store(
        &mut self,
        handle: crate::llm::observation::SharedObservationStore,
    ) {
        self.observation_store = handle;
    }

    /// Currently LLM-visible tool schemas, in stable name order.
    /// Must be re-fetched every agent-loop iteration so `tool_search`
    /// activations appear in the next request.
    pub async fn active_tool_defs(&self) -> Vec<ToolDef> {
        self.tool_catalog.active_tool_defs().await
    }

    /// Fail-closed gate: only active tools may execute. `tool_search`
    /// itself is active exactly when deferred routing has something to find.
    pub async fn is_tool_active(&self, name: &str) -> bool {
        self.tool_catalog.is_active(name).await
    }

    /// Whether the catalog knows a tool at all (active or deferred).
    pub fn knows_tool(&self, name: &str) -> bool {
        self.tool_catalog.contains(name)
    }
}
