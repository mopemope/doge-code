use crate::config::{AppConfig, McpServerConfig};
use crate::llm::LlmErrorKind;
use crate::mcp::client::McpClient;
use crate::tools::budget::head_tail_truncate;
use anyhow::{Result, anyhow};
use futures::stream::{FuturesUnordered, StreamExt};
use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult, Tool};
use serde_json::{Map as JsonMap, Value as JsonValue, json};
use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tokio::sync::{Mutex as AsyncMutex, RwLock};
use tokio::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Serialized remote output is kept below the global 8,000-character cap with
/// a little room for the envelope and warning fields.
const REMOTE_RESULT_BUDGET_CHARS: usize = 7_000;
const MAX_SUMMARY_CHARS: usize = 320;
const REMOTE_SERVER_RETRY_BACKOFF: Duration = Duration::from_secs(5);
const UNLIMITED_DISCOVERY_GRACE: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct RemoteToolInfo {
    pub alias: String,
    pub remote_name: String,
    pub server_name: String,
    pub description: Option<String>,
    pub parameters: JsonValue,
    pub strict: Option<bool>,
    /// MCP output schema is retained for future structured-result validation.
    pub output_schema: Option<JsonValue>,
    /// MCP annotations are hints only and are never used for retry decisions.
    pub annotations: Option<JsonValue>,
    /// Config retained for an explicit reconnect on the next invocation.
    /// It is never rendered in `Debug` or logging output.
    config: McpServerConfig,
    healthy: Arc<AtomicBool>,
    server_list_changed: Arc<AtomicBool>,
    // Keep one connection per server. The mutex also makes reconnect/replace
    // atomic for stdio and legacy transports; rmcp's concurrent HTTP requests
    // are not guessed at across mixed server configurations.
    client: Arc<AsyncMutex<McpClient>>,
}

impl std::fmt::Debug for RemoteToolInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteToolInfo")
            .field("alias", &self.alias)
            .field("remote_name", &self.remote_name)
            .field("server_name", &self.server_name)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteServerFailure {
    pub server_name: String,
    pub error: String,
}

#[derive(Clone)]
struct RemoteServerConnection {
    name: String,
    config: McpServerConfig,
    client: Arc<AsyncMutex<McpClient>>,
    healthy: Arc<AtomicBool>,
}

impl std::fmt::Debug for RemoteServerConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteServerConnection")
            .field("name", &self.name)
            .field("healthy", &self.healthy)
            .finish_non_exhaustive()
    }
}

#[derive(Default, Clone)]
pub struct RemoteToolRegistry {
    pub tools: Vec<RemoteToolInfo>,
    pub lookup: HashMap<String, RemoteToolInfo>,
    pub connected_servers: Vec<String>,
    pub failed_servers: Vec<RemoteServerFailure>,
    /// One flag per retained server connection, including zero-tool servers.
    tool_list_changed: Vec<Arc<AtomicBool>>,
    /// Connections are retained even when a server currently exposes zero
    /// tools, so list-change notifications and later reconnects remain live.
    server_connections: Vec<RemoteServerConnection>,
}

impl std::fmt::Debug for RemoteToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteToolRegistry")
            .field("tool_count", &self.tools.len())
            .field("connected_servers", &self.connected_servers)
            .field("failed_servers", &self.failed_servers)
            .field("server_connection_count", &self.server_connections.len())
            .finish()
    }
}

/// Normalized result of a completed remote MCP tool call.
///
/// `success` is derived from the MCP `CallToolResult.is_error` field, never
/// from whether the transport delivered a response.
#[derive(Debug, Clone)]
pub struct RemoteToolOutcome {
    pub server_name: String,
    pub tool_name: String,
    pub success: bool,
    pub is_error: bool,
    pub value: JsonValue,
    pub summary: String,
}

#[derive(Clone)]
pub struct RemoteToolManager {
    remote_tools: Arc<RwLock<Option<RemoteToolRegistry>>>,
    initialization_lock: Arc<tokio::sync::Mutex<()>>,
    retry_not_before: Arc<AsyncMutex<HashMap<String, Instant>>>,
    /// Incremented after each completed discovery pass. Concurrent callers
    /// that observed the previous generation share that pass instead of
    /// immediately retrying a failed server one-by-one.
    refresh_generation: Arc<AtomicU64>,
    config: Arc<AppConfig>,
}

impl std::fmt::Debug for RemoteToolManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteToolManager")
            .field("remote_tools", &self.remote_tools)
            .finish_non_exhaustive()
    }
}

impl RemoteToolManager {
    pub fn new(config: Arc<AppConfig>) -> Self {
        Self {
            remote_tools: Arc::new(RwLock::new(None)),
            initialization_lock: Arc::new(tokio::sync::Mutex::new(())),
            retry_not_before: Arc::new(AsyncMutex::new(HashMap::new())),
            refresh_generation: Arc::new(AtomicU64::new(0)),
            config,
        }
    }

    /// Ensure discovery has completed, aggregating concurrent callers into one
    /// build. A server failure is recorded in the registry but does not abort
    /// discovery for other servers.
    pub async fn ensure_remote_tools(&self) -> Result<()> {
        self.ensure_remote_tools_with_cancellation(None).await
    }

    /// Ensure discovery has completed, aggregating concurrent callers into one
    /// build while propagating agent cancellation during connection/listing.
    pub async fn ensure_remote_tools_with_cancellation(
        &self,
        cancel_token: Option<&CancellationToken>,
    ) -> Result<()> {
        if cancel_token.is_some_and(CancellationToken::is_cancelled) {
            return Err(anyhow!(LlmErrorKind::Cancelled));
        }
        let observed_generation = self.refresh_generation.load(Ordering::Acquire);
        if !self.registry_needs_refresh().await {
            return Ok(());
        }

        let _initializing = if let Some(token) = cancel_token {
            tokio::select! {
                biased;
                _ = token.cancelled() => return Err(anyhow!(LlmErrorKind::Cancelled)),
                guard = self.initialization_lock.lock() => guard,
            }
        } else {
            self.initialization_lock.lock().await
        };
        if cancel_token.is_some_and(CancellationToken::is_cancelled) {
            return Err(anyhow!(LlmErrorKind::Cancelled));
        }
        if self.refresh_generation.load(Ordering::Acquire) != observed_generation
            || !self.registry_needs_refresh().await
        {
            return Ok(());
        }

        // Keep a snapshot so a failed refresh can retain the last known-good
        // tools for an individual server while other servers are refreshed.
        // Cancellation returns before the replacement write, so the previous
        // registry remains untouched in that case as well.
        let previous = self.remote_tools.read().await.clone();
        let registry = self
            .build_remote_registry(cancel_token, previous.as_ref())
            .await?;
        self.record_retry_deadlines(&registry.failed_servers).await;
        *self.remote_tools.write().await = Some(registry);
        self.refresh_generation.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    async fn registry_needs_refresh(&self) -> bool {
        let state = {
            let guard = self.remote_tools.read().await;
            guard.as_ref().map(|registry| {
                (
                    registry.tool_list_changed.clone(),
                    registry
                        .failed_servers
                        .iter()
                        .map(|failure| failure.server_name.clone())
                        .collect::<Vec<_>>(),
                )
            })
        };
        let Some((flags, failed_servers)) = state else {
            return true;
        };
        if flags.iter().any(|flag| flag.load(Ordering::Acquire)) {
            return true;
        }
        if failed_servers.is_empty() {
            return false;
        }

        let now = Instant::now();
        let retry_not_before = self.retry_not_before.lock().await;
        failed_servers.iter().any(|server_name| {
            retry_not_before
                .get(server_name)
                .map(|deadline| *deadline <= now)
                .unwrap_or(true)
        })
    }

    async fn record_retry_deadlines(&self, failed_servers: &[RemoteServerFailure]) {
        let now = Instant::now();
        let failed_names = failed_servers
            .iter()
            .map(|failure| failure.server_name.as_str())
            .collect::<HashSet<_>>();
        let mut retry_not_before = self.retry_not_before.lock().await;
        retry_not_before.retain(|server_name, _| failed_names.contains(server_name.as_str()));
        for failure in failed_servers {
            retry_not_before.insert(
                failure.server_name.clone(),
                now + REMOTE_SERVER_RETRY_BACKOFF,
            );
        }
    }

    /// Invalidate the cached registry. The next runtime build performs a fresh
    /// discovery pass; this is also useful for explicit refresh callers.
    pub async fn invalidate_remote_tools(&self) {
        let _initializing = self.initialization_lock.lock().await;
        *self.remote_tools.write().await = None;
        self.retry_not_before.lock().await.clear();
        self.refresh_generation.fetch_add(1, Ordering::AcqRel);
    }

    async fn build_remote_registry(
        &self,
        cancel_token: Option<&CancellationToken>,
        previous: Option<&RemoteToolRegistry>,
    ) -> Result<RemoteToolRegistry> {
        crate::config::validate_mcp_server_names(&self.config.mcp_servers)
            .map_err(|error| anyhow!("invalid MCP server configuration: {error}"))?;
        let mut discovered = Vec::new();
        let mut failed_servers = Vec::new();
        let mut connected_servers = Vec::new();
        let mut tool_list_changed = Vec::new();
        let mut server_connections = Vec::new();
        let mut preserved_tools = Vec::new();

        // Connect and discover independent servers concurrently. Results are
        // sorted below before aliases/registry order is assigned, so completion
        // order cannot change the exposed tool set. A server configured with an
        // unlimited timeout gets a bounded registration grace period so it
        // cannot prevent healthy/local tools from being published forever.
        let enabled_servers = self
            .config
            .mcp_servers
            .iter()
            .enumerate()
            .filter(|(_, server_cfg)| server_cfg.enabled)
            .map(|(config_index, server_cfg)| (config_index, server_cfg.clone()))
            .collect::<Vec<_>>();
        let has_unlimited_server = enabled_servers.iter().any(|(_, server_cfg)| {
            server_cfg.connect_timeout_ms == 0 || server_cfg.list_timeout_ms == 0
        });
        let discovery_deadline =
            has_unlimited_server.then(|| Instant::now() + UNLIMITED_DISCOVERY_GRACE);
        let mut pending = FuturesUnordered::new();
        for (config_index, server_cfg) in enabled_servers.iter().cloned() {
            let cancel_token = cancel_token.cloned();
            pending.push(async move {
                let server_name = server_cfg.name.clone();
                let client =
                    McpClient::from_config_with_cancellation(&server_cfg, cancel_token.as_ref())
                        .await
                        .map_err(|error| DiscoveryFailure {
                            config_index,
                            server_name: server_name.clone(),
                            config: server_cfg.clone(),
                            error,
                        })?;
                let client = Arc::new(AsyncMutex::new(client));
                let list_changed = client.lock().await.tool_list_changed_flag();
                let healthy = Arc::new(AtomicBool::new(true));
                let tools = client
                    .lock()
                    .await
                    .list_tools_with_cancellation(cancel_token.as_ref())
                    .await
                    .map_err(|error| DiscoveryFailure {
                        config_index,
                        server_name: server_name.clone(),
                        config: server_cfg.clone(),
                        error,
                    })?;
                Ok::<_, DiscoveryFailure>((
                    config_index,
                    server_name,
                    client,
                    list_changed,
                    healthy,
                    server_cfg,
                    tools,
                ))
            });
        }

        let mut discovery_results = Vec::new();
        let mut completed_servers = HashSet::new();
        loop {
            let next_result = if let Some(token) = cancel_token {
                if let Some(deadline) = discovery_deadline {
                    tokio::select! {
                        biased;
                        _ = token.cancelled() => return Err(anyhow!(LlmErrorKind::Cancelled)),
                        result = pending.next() => result,
                        _ = tokio::time::sleep_until(deadline) => break,
                    }
                } else {
                    tokio::select! {
                        biased;
                        _ = token.cancelled() => return Err(anyhow!(LlmErrorKind::Cancelled)),
                        result = pending.next() => result,
                    }
                }
            } else if let Some(deadline) = discovery_deadline {
                tokio::select! {
                    result = pending.next() => result,
                    _ = tokio::time::sleep_until(deadline) => break,
                }
            } else {
                pending.next().await
            };
            let Some(result) = next_result else {
                break;
            };
            match &result {
                Ok((config_index, _, _, _, _, _, _)) => {
                    completed_servers.insert(*config_index);
                }
                Err(failure) => {
                    completed_servers.insert(failure.config_index);
                }
            }
            discovery_results.push(result);
        }

        for (config_index, config) in &enabled_servers {
            if completed_servers.contains(config_index) {
                continue;
            }
            let failure = DiscoveryFailure {
                config_index: *config_index,
                server_name: config.name.clone(),
                config: config.clone(),
                error: crate::mcp::client::McpClientError::Timeout {
                    operation: "discovery",
                },
            };
            warn!(
                target: "mcp::remote_tools",
                server = %failure.server_name,
                "remote MCP server exceeded the discovery registration grace period"
            );
            discovery_results.push(Err(failure));
        }

        for result in discovery_results {
            let (config_index, server_name, client, list_changed, healthy, config, tools) =
                match result {
                    Ok(result) => result,
                    Err(failure) => {
                        if failure.error.is_cancelled() {
                            return Err(anyhow!(LlmErrorKind::Cancelled));
                        }
                        warn!(
                            target: "mcp::remote_tools",
                            server = %failure.server_name,
                            error = %failure.error,
                            "failed to discover remote MCP server"
                        );
                        failed_servers.push(RemoteServerFailure {
                            server_name: failure.server_name.clone(),
                            error: failure.error.to_string(),
                        });
                        if let Some(previous) = previous {
                            preserve_failed_server(
                                previous,
                                &failure,
                                &mut preserved_tools,
                                &mut server_connections,
                                &mut tool_list_changed,
                            );
                        }
                        continue;
                    }
                };
            connected_servers.push(server_name.clone());
            tool_list_changed.push(list_changed.clone());
            server_connections.push(RemoteServerConnection {
                name: server_name.clone(),
                config: config.clone(),
                client: client.clone(),
                healthy: healthy.clone(),
            });
            discovered.extend(tools.into_iter().map(|tool| DiscoveredTool {
                config_index,
                server_name: server_name.clone(),
                tool,
                config: config.clone(),
                client: client.clone(),
                healthy: healthy.clone(),
                list_changed: list_changed.clone(),
            }));
        }

        // Sort before assigning aliases. This is important: assigning a base
        // alias to whichever colliding tool happened to arrive first would make
        // aliases depend on server/list order.
        discovered.sort_by(|left, right| {
            left.server_name
                .cmp(&right.server_name)
                .then_with(|| left.tool.name.cmp(&right.tool.name))
                .then_with(|| left.config_index.cmp(&right.config_index))
        });

        let mut identities = discovered
            .iter()
            .map(|item| (item.server_name.clone(), item.tool.name.to_string()))
            .collect::<Vec<_>>();
        identities.extend(
            preserved_tools
                .iter()
                .map(|info| (info.server_name.clone(), info.remote_name.clone())),
        );
        identities.sort();
        identities.dedup();
        let mut registry = RemoteToolRegistry {
            connected_servers: {
                connected_servers.sort();
                connected_servers.dedup();
                connected_servers
            },
            failed_servers,
            tool_list_changed,
            server_connections,
            ..RemoteToolRegistry::default()
        };
        let mut aliases = AliasRegistry::from_identities(&identities);
        let mut seen = HashSet::new();

        for item in discovered {
            let remote_name = item.tool.name.to_string();
            let identity = (item.server_name.clone(), remote_name.clone());
            if !seen.insert(identity) {
                continue;
            }
            let alias = aliases.alias_for(&item.server_name, &remote_name);
            let parameters = JsonValue::Object(item.tool.input_schema.as_ref().clone());
            let output_schema = item
                .tool
                .output_schema
                .as_ref()
                .map(|schema| JsonValue::Object(schema.as_ref().clone()));
            let annotations = item
                .tool
                .annotations
                .as_ref()
                .and_then(|annotations| serde_json::to_value(annotations).ok());

            let mut description_parts = Vec::new();
            if let Some(description) = &item.tool.description {
                description_parts.push(description.to_string());
            }
            if description_parts.is_empty()
                && let Some(title) = item.tool.title.as_ref()
            {
                description_parts.push(title.clone());
            }
            if description_parts.is_empty()
                && let Some(annotations) = &item.tool.annotations
                && let Some(title) = &annotations.title
            {
                description_parts.push(title.clone());
            }
            let description = if description_parts.is_empty() {
                Some(format!(
                    "Remote MCP tool '{}' provided by server '{}'",
                    remote_name, item.server_name
                ))
            } else {
                Some(format!(
                    "Remote MCP tool '{}' on server '{}': {}",
                    remote_name,
                    item.server_name,
                    description_parts.join(" ")
                ))
            };

            let info = RemoteToolInfo {
                alias: alias.clone(),
                remote_name,
                server_name: item.server_name,
                description,
                parameters,
                strict: None,
                output_schema,
                annotations,
                config: item.config,
                healthy: item.healthy,
                server_list_changed: item.list_changed,
                client: item.client,
            };
            registry.lookup.insert(alias, info.clone());
            registry.tools.push(info);
        }

        // A failed refresh must not erase tools that were usable immediately
        // before the failure. Fresh entries win if a server somehow reports
        // both a result and a failure for the same identity.
        for mut info in preserved_tools {
            let identity = (info.server_name.clone(), info.remote_name.clone());
            if !seen.insert(identity) {
                continue;
            }
            let alias = aliases.alias_for(&info.server_name, &info.remote_name);
            info.alias = alias.clone();
            registry.lookup.insert(alias, info.clone());
            registry.tools.push(info);
        }

        registry.tools.sort_by(|left, right| {
            left.server_name
                .cmp(&right.server_name)
                .then_with(|| left.remote_name.cmp(&right.remote_name))
        });
        debug!(
            target: "mcp::remote_tools",
            count = registry.tools.len(),
            connected_servers = registry.connected_servers.len(),
            failed_servers = registry.failed_servers.len(),
            "registered remote MCP tools"
        );
        Ok(registry)
    }

    pub async fn remote_tools_snapshot(&self) -> Vec<RemoteToolInfo> {
        self.remote_tools
            .read()
            .await
            .as_ref()
            .map(|registry| registry.tools.clone())
            .unwrap_or_default()
    }

    pub async fn registry_status(&self) -> Option<RemoteToolRegistry> {
        self.remote_tools.read().await.clone()
    }

    /// Call one remote tool exactly once. There is deliberately no retry loop:
    /// MCP tools may have non-idempotent side effects.
    pub async fn call_remote_tool(
        &self,
        alias: &str,
        args: &JsonValue,
        cancel_token: Option<CancellationToken>,
    ) -> Result<Option<RemoteToolOutcome>> {
        self.ensure_remote_tools_with_cancellation(cancel_token.as_ref())
            .await?;

        let tool = {
            let guard = self.remote_tools.read().await;
            guard
                .as_ref()
                .and_then(|registry| registry.lookup.get(alias).cloned())
        };
        let Some(tool) = tool else {
            return Ok(None);
        };

        let arguments: Option<JsonMap<String, JsonValue>> = match args {
            JsonValue::Null => None,
            JsonValue::Object(map) => Some(map.clone()),
            other => {
                return Err(anyhow!(
                    "remote MCP tool '{}' expects object arguments, received {}",
                    tool.alias,
                    json_type_name(other)
                ));
            }
        };

        let mut params = CallToolRequestParams::new(tool.remote_name.clone());
        params.arguments = arguments;

        let client = tool.client.clone();
        if !tool.healthy.load(Ordering::Acquire) {
            let reconnected =
                McpClient::from_config_with_cancellation(&tool.config, cancel_token.as_ref()).await;
            let reconnected = match reconnected {
                Ok(client) => client,
                Err(error) if error.is_cancelled() => {
                    return Err(anyhow!(LlmErrorKind::Cancelled));
                }
                Err(error) => return Err(anyhow!(error)),
            };
            let mut client_guard = if let Some(token) = cancel_token.as_ref() {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => return Err(anyhow!(LlmErrorKind::Cancelled)),
                    guard = client.lock() => guard,
                }
            } else {
                client.lock().await
            };
            *client_guard = reconnected;
            tool.healthy.store(true, Ordering::Release);
            // Force the next registry snapshot to capture the replacement
            // client's list-change flag and schema set.
            tool.server_list_changed.store(true, Ordering::Release);
        }

        let response = if let Some(token) = cancel_token.as_ref() {
            let client_guard = tokio::select! {
                biased;
                _ = token.cancelled() => return Err(anyhow!(LlmErrorKind::Cancelled)),
                guard = client.lock() => guard,
            };
            client_guard
                .call_tool_once(params, cancel_token.as_ref())
                .await
        } else {
            let client_guard = client.lock().await;
            client_guard.call_tool_once(params, None).await
        };

        match response {
            Ok(CallToolResponse::Complete(result)) => Ok(Some(normalize_call_tool_result(
                &tool.server_name,
                &tool.remote_name,
                result,
            ))),
            Ok(CallToolResponse::InputRequired(_)) => Ok(Some(RemoteToolOutcome::unsupported(
                &tool.server_name,
                &tool.remote_name,
                "remote MCP tool requires interactive input, but Doge-Code does not support MCP input-required flows yet",
            ))),
            Ok(CallToolResponse::Task(_)) => Ok(Some(RemoteToolOutcome::unsupported(
                &tool.server_name,
                &tool.remote_name,
                "server returned MCP Task, but remote task execution is not enabled",
            ))),
            Ok(_) => Ok(Some(RemoteToolOutcome::unsupported(
                &tool.server_name,
                &tool.remote_name,
                "server returned an unsupported MCP tool response variant",
            ))),
            Err(error) if error.is_cancelled() => Err(anyhow!(LlmErrorKind::Cancelled)),
            Err(error) => {
                tool.healthy.store(false, Ordering::Release);
                warn!(
                    target: "mcp::remote_tools",
                    server = %tool.server_name,
                    tool = %tool.remote_name,
                    error = %error,
                    "remote MCP tool call failed at protocol or transport boundary"
                );
                Err(anyhow!(error))
            }
        }
    }
}

fn preserve_failed_server(
    previous: &RemoteToolRegistry,
    failure: &DiscoveryFailure,
    preserved_tools: &mut Vec<RemoteToolInfo>,
    server_connections: &mut Vec<RemoteServerConnection>,
    tool_list_changed: &mut Vec<Arc<AtomicBool>>,
) {
    let old_tools = previous
        .tools
        .iter()
        .filter(|info| info.server_name == failure.server_name && info.config == failure.config)
        .cloned()
        .collect::<Vec<_>>();
    let old_connection = previous
        .server_connections
        .iter()
        .find(|connection| {
            connection.name == failure.server_name && connection.config == failure.config
        })
        .cloned();

    if old_connection.is_none() && old_tools.is_empty() {
        return;
    }

    let client = old_connection
        .as_ref()
        .map(|connection| connection.client.clone())
        .or_else(|| old_tools.first().map(|info| info.client.clone()));
    let config = old_connection
        .as_ref()
        .map(|connection| connection.config.clone())
        .or_else(|| old_tools.first().map(|info| info.config.clone()))
        .unwrap_or_else(|| failure.config.clone());
    let Some(client) = client else {
        return;
    };

    // Do not mutate the previous registry's shared Arcs while a refresh is in
    // progress. The retained state is explicitly unhealthy and requests one
    // more discovery pass after the fallback registry is installed.
    let healthy = Arc::new(AtomicBool::new(false));
    // The failure itself is tracked by the retry deadline. Do not leave a
    // sticky list-change flag on the retained snapshot, or every subsequent
    // call would bypass the backoff and rebuild immediately.
    let list_changed = Arc::new(AtomicBool::new(false));

    if !server_connections
        .iter()
        .any(|connection| connection.name == failure.server_name && connection.config == config)
    {
        server_connections.push(RemoteServerConnection {
            name: failure.server_name.clone(),
            config,
            client: client.clone(),
            healthy: healthy.clone(),
        });
        tool_list_changed.push(list_changed.clone());
    }

    for mut info in old_tools {
        info.client = client.clone();
        info.healthy = healthy.clone();
        info.server_list_changed = list_changed.clone();
        preserved_tools.push(info);
    }
}

struct DiscoveredTool {
    config_index: usize,
    server_name: String,
    tool: Tool,
    config: McpServerConfig,
    client: Arc<AsyncMutex<McpClient>>,
    healthy: Arc<AtomicBool>,
    list_changed: Arc<AtomicBool>,
}

struct DiscoveryFailure {
    config_index: usize,
    server_name: String,
    config: McpServerConfig,
    error: crate::mcp::client::McpClientError,
}

#[derive(Default)]
struct AliasRegistry {
    by_alias: HashMap<String, (String, String)>,
    by_identity: HashMap<(String, String), String>,
}

impl AliasRegistry {
    fn from_identities(identities: &[(String, String)]) -> Self {
        let mut groups: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for (server, tool) in identities {
            let base = format!(
                "mcp_{}_{}",
                sanitize_identifier(server),
                sanitize_identifier(tool)
            );
            groups
                .entry(base)
                .or_default()
                .push((server.clone(), tool.clone()));
        }

        let mut registry = Self::default();
        let mut bases = groups.into_iter().collect::<Vec<_>>();
        bases.sort_by(|left, right| left.0.cmp(&right.0));
        for (base, mut identities) in bases {
            identities.sort();
            identities.dedup();
            let collision = identities.len() > 1;
            for identity in identities {
                let mut candidate = if collision || registry.by_alias.contains_key(&base) {
                    Self::hashed_candidate(&base, &identity, &registry.by_alias)
                } else {
                    base.clone()
                };
                // The loop is defensive against a future hash collision.
                while registry.by_alias.contains_key(&candidate) {
                    candidate = format!("{candidate}_{}", stable_hash(&identity.0, &identity.1));
                }
                registry
                    .by_alias
                    .insert(candidate.clone(), identity.clone());
                registry.by_identity.insert(identity, candidate);
            }
        }
        registry
    }

    fn hashed_candidate(
        base: &str,
        identity: &(String, String),
        aliases: &HashMap<String, (String, String)>,
    ) -> String {
        let digest = stable_hash(&identity.0, &identity.1);
        for length in [6usize, 12, 16, 32, 64] {
            let candidate = format!("{base}_{}", &digest[..length]);
            if !aliases.contains_key(&candidate) {
                return candidate;
            }
        }
        format!("{base}_{digest}")
    }

    fn alias_for(&mut self, server: &str, tool: &str) -> String {
        let identity = (server.to_string(), tool.to_string());
        if let Some(alias) = self.by_identity.get(&identity) {
            return alias.clone();
        }

        let base = format!(
            "mcp_{}_{}",
            sanitize_identifier(server),
            sanitize_identifier(tool)
        );
        let candidate = if self.by_alias.contains_key(&base) {
            Self::hashed_candidate(&base, &identity, &self.by_alias)
        } else {
            base
        };
        self.by_alias.insert(candidate.clone(), identity.clone());
        self.by_identity.insert(identity, candidate.clone());
        candidate
    }
}

fn stable_hash(server: &str, tool: &str) -> String {
    blake3::hash(format!("{server}\0{tool}").as_bytes())
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn sanitize_identifier(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    for (idx, ch) in input.chars().enumerate() {
        let mapped = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else if matches!(ch, '_' | '-') {
            ch
        } else {
            '_'
        };
        if idx == 0 && mapped.is_ascii_digit() {
            result.push('t');
        }
        result.push(mapped);
    }
    if result.is_empty() {
        "tool".to_string()
    } else {
        result
    }
}

/// Normalize a completed MCP result into Doge's bounded, structured contract.
pub fn normalize_call_tool_result(
    server_name: &str,
    tool_name: &str,
    result: CallToolResult,
) -> RemoteToolOutcome {
    let is_error = result.is_error.unwrap_or(false);
    let success = !is_error;
    let content = serde_json::to_value(&result.content).unwrap_or_else(|_| json!([]));

    let mut normalized = JsonMap::new();
    normalized.insert("ok".to_string(), json!(success));
    normalized.insert("server".to_string(), json!(server_name));
    normalized.insert("tool".to_string(), json!(tool_name));
    normalized.insert("is_error".to_string(), json!(is_error));
    normalized.insert("content".to_string(), content);
    if let Some(structured_content) = result.structured_content {
        normalized.insert("structured_content".to_string(), structured_content);
    }
    if let Some(result_type) = result.result_type
        && let Ok(result_type) = serde_json::to_value(result_type)
    {
        normalized.insert("result_type".to_string(), result_type);
    }
    normalized.insert("warnings".to_string(), json!([]));

    let (value, truncated) = enforce_remote_budget(JsonValue::Object(normalized));
    let summary = build_summary(server_name, tool_name, success, &value, truncated);
    RemoteToolOutcome {
        server_name: server_name.to_string(),
        tool_name: tool_name.to_string(),
        success,
        is_error,
        value,
        summary,
    }
}

impl RemoteToolOutcome {
    fn unsupported(server_name: &str, tool_name: &str, message: &str) -> Self {
        let value = json!({
            "ok": false,
            "server": server_name,
            "tool": tool_name,
            "is_error": true,
            "error": {
                "kind": "unsupported_mcp_response",
                "message": message,
            },
            "warnings": [message],
        });
        let (value, truncated) = enforce_remote_budget(value);
        let summary = build_summary(server_name, tool_name, false, &value, truncated);
        Self {
            server_name: server_name.to_string(),
            tool_name: tool_name.to_string(),
            success: false,
            is_error: true,
            value,
            summary,
        }
    }
}

fn compact_remote_envelope(value: &JsonValue) -> JsonValue {
    let Some(object) = value.as_object() else {
        return json!({
            "ok": false,
            "warnings": ["remote MCP result was truncated because it exceeded the output budget"],
        });
    };

    let mut compact = JsonMap::new();
    compact.insert(
        "ok".to_string(),
        object.get("ok").cloned().unwrap_or(JsonValue::Bool(false)),
    );
    compact.insert(
        "is_error".to_string(),
        object
            .get("is_error")
            .cloned()
            .unwrap_or(JsonValue::Bool(false)),
    );
    for key in ["server", "tool"] {
        if let Some(JsonValue::String(value)) = object.get(key) {
            compact.insert(
                key.to_string(),
                JsonValue::String(head_tail_truncate(value, MAX_SUMMARY_CHARS).text),
            );
        }
    }
    compact.insert(
        "warnings".to_string(),
        json!(["remote MCP result was truncated because it exceeded the output budget"]),
    );
    compact.insert("truncated".to_string(), JsonValue::Bool(true));
    JsonValue::Object(compact)
}

fn enforce_remote_budget(mut value: JsonValue) -> (JsonValue, bool) {
    let serialized = match serde_json::to_string(&value) {
        Ok(serialized) => serialized,
        Err(_) => {
            return (
                json!({
                    "ok": false,
                    "warnings": ["remote MCP result could not be serialized"],
                }),
                true,
            );
        }
    };
    if serialized.chars().count() <= REMOTE_RESULT_BUDGET_CHARS {
        return (value, false);
    }

    let truncated =
        crate::llm::truncate_tool_output_to_budget(serialized, REMOTE_RESULT_BUDGET_CHARS);
    if let Ok(mut parsed) = serde_json::from_str::<JsonValue>(&truncated) {
        let preserves_contract = parsed.get("ok").is_some() && parsed.get("is_error").is_some();
        if let Some(object) = parsed.as_object_mut() {
            object.insert(
                "warnings".to_string(),
                json!([
                    "remote MCP result was truncated because it exceeded the output budget; large fields were shortened"
                ]),
            );
        }
        let after_warning = serde_json::to_string(&parsed).unwrap_or_default();
        if preserves_contract {
            if after_warning.chars().count() <= REMOTE_RESULT_BUDGET_CHARS {
                return (parsed, true);
            }
            value = parsed;
        }
    }

    // A pathological object made entirely of many non-string fields may still
    // exceed the budget after field-level shrinking. Keep a small valid JSON
    // envelope, preserving the completed result's success semantics rather
    // than slicing JSON or turning a successful tool into a failure.
    if serde_json::to_string(&value)
        .map(|serialized| serialized.chars().count() > REMOTE_RESULT_BUDGET_CHARS)
        .unwrap_or(true)
    {
        value = compact_remote_envelope(&value);
    }
    (value, true)
}

fn build_summary(
    server_name: &str,
    tool_name: &str,
    success: bool,
    value: &JsonValue,
    truncated: bool,
) -> String {
    let status = if success { "success" } else { "error" };
    let detail = value
        .get("structured_content")
        .and_then(|structured| serde_json::to_string(structured).ok())
        .or_else(|| first_text_content(value))
        .unwrap_or_else(|| format!("{server_name}/{tool_name}"));
    let detail = head_tail_truncate(&detail, MAX_SUMMARY_CHARS).text;
    let truncation_note = if truncated { " [truncated]" } else { "" };
    format!("Remote MCP {status}: {detail}{truncation_note}")
}

fn first_text_content(value: &JsonValue) -> Option<String> {
    value
        .get("content")
        .and_then(JsonValue::as_array)
        .and_then(|blocks| {
            blocks.iter().find_map(|block| {
                block
                    .get("type")
                    .and_then(JsonValue::as_str)
                    .filter(|kind| *kind == "text")
                    .and_then(|_| block.get("text"))
                    .and_then(JsonValue::as_str)
                    .map(ToOwned::to_owned)
            })
        })
}

fn json_type_name(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "boolean",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, McpServerConfig, McpTransport};
    use rmcp::model::{CallToolResult, ContentBlock, TextContent};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn result(is_error: Option<bool>) -> CallToolResult {
        let mut result =
            CallToolResult::success(vec![ContentBlock::Text(TextContent::new("hello"))]);
        result.is_error = is_error;
        result
    }

    #[test]
    fn test_is_error_none_is_success() {
        let outcome = normalize_call_tool_result("github", "echo", result(None));
        assert!(outcome.success);
        assert!(!outcome.is_error);
        assert_eq!(outcome.value["ok"], true);
    }

    #[test]
    fn test_is_error_false_is_success() {
        let outcome = normalize_call_tool_result("github", "echo", result(Some(false)));
        assert!(outcome.success);
        assert_eq!(outcome.value["is_error"], false);
    }

    #[test]
    fn test_is_error_true_is_tool_failure_not_rust_error() {
        let outcome = normalize_call_tool_result("github", "create_issue", result(Some(true)));
        assert!(!outcome.success);
        assert!(outcome.is_error);
        assert_eq!(outcome.value["ok"], false);
        assert_eq!(outcome.value["content"][0]["text"], "hello");
    }

    #[test]
    fn test_structured_content_and_all_blocks_are_preserved() {
        let mut value = result(Some(false));
        value.structured_content = Some(json!({"id": 123, "status": "ok"}));
        value.content.push(ContentBlock::image("data", "image/png"));
        let outcome = normalize_call_tool_result("github", "echo", value);
        assert_eq!(outcome.value["structured_content"]["id"], 123);
        assert_eq!(outcome.value["content"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn test_large_result_is_bounded_and_valid_json() {
        let mut value = result(Some(false));
        value.content = vec![ContentBlock::text("x".repeat(50_000))];
        let outcome = normalize_call_tool_result("github", "large", value);
        let serialized = serde_json::to_string(&outcome.value).expect("valid JSON");
        assert!(serialized.chars().count() <= 8_000);
        assert!(serialized.contains("truncated"));
        assert!(serde_json::from_str::<JsonValue>(&serialized).is_ok());
    }

    #[test]
    fn test_pathological_numeric_result_keeps_tool_success_semantics() {
        let mut value = result(Some(false));
        let mut numbers = JsonMap::new();
        for index in 0..10_000 {
            numbers.insert(format!("key-{index}"), json!(index));
        }
        value.structured_content = Some(JsonValue::Object(numbers));
        let outcome = normalize_call_tool_result("github", "large_numeric", value);
        let serialized = serde_json::to_string(&outcome.value).expect("valid JSON");
        assert!(serialized.chars().count() <= 8_000);
        assert!(outcome.success);
        assert_eq!(outcome.value["ok"], true, "value: {}", outcome.value);
        assert_eq!(outcome.value["is_error"], false, "value: {}", outcome.value);
    }

    #[tokio::test]
    async fn test_tool_list_change_flag_invalidates_next_registry_snapshot() {
        let temp = tempfile::tempdir().expect("tempdir");
        let app = Arc::new(AppConfig {
            project_root: temp.path().to_path_buf(),
            ..AppConfig::default()
        });
        let handle =
            crate::mcp::server::spawn_mcp_server("127.0.0.1:0", app, Arc::new(RwLock::new(None)))
                .await
                .expect("local MCP server should bind");
        let mut config = AppConfig {
            project_root: temp.path().to_path_buf(),
            ..AppConfig::default()
        };
        config.mcp_servers = vec![McpServerConfig {
            name: "local".to_string(),
            enabled: true,
            address: Some(format!("http://{}/mcp", handle.local_addr())),
            transport: McpTransport::Http,
            ..McpServerConfig::default()
        }];
        let manager = RemoteToolManager::new(Arc::new(config));
        manager
            .ensure_remote_tools()
            .await
            .expect("initial discovery");
        let before = manager.registry_status().await.expect("registry");
        before.tool_list_changed[0].store(true, Ordering::Release);
        manager
            .ensure_remote_tools()
            .await
            .expect("refresh discovery");
        let after = manager.registry_status().await.expect("refreshed registry");
        assert!(!Arc::ptr_eq(
            &before.tools[0].client,
            &after.tools[0].client
        ));
        handle.shutdown().await.expect("server shutdown");
    }

    #[tokio::test]
    async fn test_failed_refresh_retains_last_known_tools() {
        let temp = tempfile::tempdir().expect("tempdir");
        let app = Arc::new(AppConfig {
            project_root: temp.path().to_path_buf(),
            ..AppConfig::default()
        });
        let handle = crate::mcp::server::spawn_mcp_server(
            "127.0.0.1:0",
            app.clone(),
            Arc::new(RwLock::new(None)),
        )
        .await
        .expect("local MCP server should bind");
        let mut config = (*app).clone();
        config.mcp_servers = vec![McpServerConfig {
            name: "local".to_string(),
            enabled: true,
            address: Some(format!("http://{}/mcp", handle.local_addr())),
            transport: McpTransport::Http,
            connect_timeout_ms: 100,
            list_timeout_ms: 100,
            call_timeout_ms: 100,
            ..McpServerConfig::default()
        }];
        let manager = RemoteToolManager::new(Arc::new(config));
        manager
            .ensure_remote_tools()
            .await
            .expect("initial discovery");
        let before = manager.registry_status().await.expect("registry");
        assert!(
            before
                .tools
                .iter()
                .any(|tool| tool.alias == "mcp_local_say_hello")
        );

        // Make the next discovery attempt fail. The old connection and tool
        // definitions should remain available as a stale-but-callable snapshot.
        handle.shutdown().await.expect("server shutdown");
        before.tool_list_changed[0].store(true, Ordering::Release);
        manager
            .ensure_remote_tools()
            .await
            .expect("refresh should retain the previous registry");
        let after = manager.registry_status().await.expect("fallback registry");
        assert!(
            after
                .tools
                .iter()
                .any(|tool| tool.alias == "mcp_local_say_hello")
        );
        assert!(
            after
                .failed_servers
                .iter()
                .any(|failure| failure.server_name == "local")
        );
        assert!(after.connected_servers.is_empty());
        assert!(!after.tools[0].healthy.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn test_broken_remote_server_does_not_hide_healthy_server() {
        let temp = tempfile::tempdir().expect("tempdir");
        let app = Arc::new(AppConfig {
            project_root: temp.path().to_path_buf(),
            ..AppConfig::default()
        });
        let handle = crate::mcp::server::spawn_mcp_server(
            "127.0.0.1:0",
            app.clone(),
            Arc::new(RwLock::new(None)),
        )
        .await
        .expect("local MCP server should bind");
        let mut config = (*app).clone();
        config.mcp_servers = vec![
            McpServerConfig {
                name: "broken".to_string(),
                enabled: true,
                address: Some("http://127.0.0.1:1/mcp".to_string()),
                transport: McpTransport::Http,
                connect_timeout_ms: 25,
                list_timeout_ms: 25,
                call_timeout_ms: 25,
                ..McpServerConfig::default()
            },
            McpServerConfig {
                name: "healthy".to_string(),
                enabled: true,
                address: Some(format!("http://{}/mcp", handle.local_addr())),
                transport: McpTransport::Http,
                connect_timeout_ms: 5_000,
                list_timeout_ms: 5_000,
                call_timeout_ms: 5_000,
                ..McpServerConfig::default()
            },
        ];
        let manager = RemoteToolManager::new(Arc::new(config));
        manager.ensure_remote_tools().await.expect("healthy server");
        let status = manager.registry_status().await.expect("registry status");
        assert!(
            status
                .connected_servers
                .iter()
                .any(|name| name == "healthy")
        );
        assert!(
            status
                .failed_servers
                .iter()
                .any(|failure| failure.server_name == "broken")
        );
        assert!(
            status
                .tools
                .iter()
                .any(|tool| tool.alias == "mcp_healthy_say_hello")
        );
        let first_healthy_client = status
            .tools
            .iter()
            .find(|tool| tool.alias == "mcp_healthy_say_hello")
            .expect("healthy tool")
            .client
            .clone();

        // A failed server is retried with backoff; a second immediate ensure
        // must not rebuild healthy connections or re-run discovery.
        manager.ensure_remote_tools().await.expect("backoff ensure");
        let after_backoff = manager.registry_status().await.expect("registry status");
        let second_healthy_client = after_backoff
            .tools
            .iter()
            .find(|tool| tool.alias == "mcp_healthy_say_hello")
            .expect("healthy tool")
            .client
            .clone();
        assert!(Arc::ptr_eq(&first_healthy_client, &second_healthy_client));
        handle.shutdown().await.expect("server shutdown");
    }

    #[tokio::test]
    async fn test_remote_manager_preserves_tool_error_semantics_end_to_end() {
        let temp = tempfile::tempdir().expect("tempdir");
        let app = Arc::new(AppConfig {
            project_root: temp.path().to_path_buf(),
            ..AppConfig::default()
        });
        let handle = crate::mcp::server::spawn_mcp_server(
            "127.0.0.1:0",
            app.clone(),
            Arc::new(RwLock::new(None)),
        )
        .await
        .expect("local MCP server should bind");
        let mut config = (*app).clone();
        config.mcp_servers = vec![McpServerConfig {
            name: "local".to_string(),
            enabled: true,
            address: Some(format!("http://{}/mcp", handle.local_addr())),
            transport: McpTransport::Http,
            ..McpServerConfig::default()
        }];
        let manager = RemoteToolManager::new(Arc::new(config));
        manager.ensure_remote_tools().await.expect("discovery");

        let success = manager
            .call_remote_tool("mcp_local_say_hello", &json!({}), None)
            .await
            .expect("remote call")
            .expect("known remote tool");
        assert!(success.success);
        assert_eq!(success.value["ok"], true);

        let failure = manager
            .call_remote_tool(
                "mcp_local_fs_read",
                &json!({ "path": temp.path().join("missing.txt").to_string_lossy() }),
                None,
            )
            .await
            .expect("remote tool-level error")
            .expect("known remote tool");
        assert!(!failure.success);
        assert_eq!(failure.value["ok"], false);
        assert_eq!(failure.value["is_error"], true);

        handle.shutdown().await.expect("server shutdown");
    }

    #[test]
    fn test_aliases_are_stable_when_list_order_changes() {
        fn aliases(order: &[(&str, &str)]) -> Vec<(String, String)> {
            let identities = order
                .iter()
                .map(|(server, tool)| ((*server).to_string(), (*tool).to_string()))
                .collect::<Vec<_>>();
            let mut registry = AliasRegistry::from_identities(&identities);
            identities
                .into_iter()
                .map(|(server, tool)| {
                    let alias = registry.alias_for(&server, &tool);
                    (format!("{server}/{tool}"), alias)
                })
                .collect()
        }
        let first = aliases(&[("my_server", "foo"), ("my-server", "foo")]);
        let second = aliases(&[("my-server", "foo"), ("my_server", "foo")]);
        let mut first_by_identity = first.clone();
        let mut second_by_identity = second.clone();
        first_by_identity.sort();
        second_by_identity.sort();
        assert_eq!(first_by_identity, second_by_identity);
        assert_ne!(first[0].1, first[1].1);
    }

    #[test]
    fn test_alias_collision_uses_stable_hash_not_ordinal() {
        let mut registry = AliasRegistry::default();
        let first = registry.alias_for("a", "x.y");
        let second = registry.alias_for("a", "x/y");
        assert_ne!(first, second);
        assert!(second.starts_with("mcp_a_x_y_"));
        assert_eq!(registry.alias_for("a", "x.y"), first);
    }
}
