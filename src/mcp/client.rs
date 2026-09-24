use reqwest::Url;
use rmcp::{
    ClientHandler,
    model::{
        CallToolRequest, CallToolRequestParams, CallToolResponse, CancelledNotification,
        CancelledNotificationParam, ClientConfig, ClientRequest, ListToolsRequest,
        PaginatedRequestParams, ProtocolVersion, RequestId, ServerPeerInfo, ServerResult, Tool,
    },
    service::{
        ClientInitializeError, ClientLifecycleMode, NotificationContext, PeerRequestOptions,
        RoleClient,
    },
    transport::{
        StreamableHttpClientTransport, TokioChildProcess,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use std::collections::HashSet;
use std::future::Future;
use std::io;
use std::path::Path;
use std::process::Stdio;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::time::{Duration, Instant, timeout};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::config::{McpConfigError, McpServerConfig, McpTransport};

const MAX_STDERR_LINE_BYTES: usize = 16 * 1024;
const CANCEL_NOTIFICATION_SEND_TIMEOUT: Duration = Duration::from_millis(250);

/// Errors raised by the MCP protocol/transport boundary.
///
/// A completed `CallToolResult` with `is_error = true` is deliberately not
/// represented by this enum: that is a tool-level result and is normalized by
/// `RemoteToolManager` into an unsuccessful Doge tool outcome.
#[derive(Debug, thiserror::Error)]
pub enum McpClientError {
    #[error("invalid MCP configuration: {0}")]
    Config(#[from] McpConfigError),
    #[error("MCP connection failed: {0}")]
    Connection(String),
    #[error("MCP protocol/transport failure during {operation}: {message}")]
    Protocol {
        operation: &'static str,
        message: String,
    },
    #[error("MCP {operation} timed out")]
    Timeout { operation: &'static str },
    #[error("MCP {operation} cancelled")]
    Cancelled { operation: &'static str },
    #[error("MCP server returned an unsupported response: {0}")]
    UnsupportedResponse(String),
}

impl McpClientError {
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled { .. })
    }
}

/// Client handler used for all outbound MCP connections.
///
/// Doge-Code does not implement elicitation UI or the Tasks lifecycle yet, so
/// the default client capabilities are intentionally conservative. The
/// handler still receives list-change notifications so the registry can
/// invalidate its next snapshot.
#[derive(Debug, Clone)]
pub struct DogeMcpClientHandler {
    server_name: String,
    tool_list_changed: Arc<AtomicBool>,
}

impl DogeMcpClientHandler {
    pub fn new(server_name: impl Into<String>) -> Self {
        Self {
            server_name: server_name.into(),
            tool_list_changed: Arc::new(AtomicBool::new(false)),
        }
    }

    fn list_changed_flag(&self) -> Arc<AtomicBool> {
        self.tool_list_changed.clone()
    }
}

impl ClientHandler for DogeMcpClientHandler {
    fn get_info(&self) -> ClientConfig {
        // Do not advertise elicitation or Tasks support: Doge-Code has no UI or
        // durable task manager for those protocol features yet.
        ClientConfig::default()
    }

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.tool_list_changed.store(true, Ordering::Release);
        debug!(
            server = %self.server_name,
            "remote MCP server reported tools/list_changed"
        );
    }
}

/// Normalize the legacy host:port form while validating the URL scheme.
pub(crate) fn normalize_http_uri(address: &str) -> Result<String, String> {
    let trimmed = address.trim();
    if trimmed.is_empty() {
        return Err("MCP HTTP address cannot be empty".into());
    }

    let with_scheme = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("http://{trimmed}")
    };

    // Do not echo the raw address in parser errors: URLs may contain
    // credentials, query tokens, or other deployment secrets.
    let mut url = Url::parse(&with_scheme).map_err(|_| "invalid MCP HTTP address".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!(
            "unsupported MCP HTTP URL scheme '{}'; expected http or https",
            url.scheme()
        ));
    }
    if url.host_str().is_none() {
        return Err("MCP HTTP address must include a host".into());
    }

    if url.path() == "/" {
        url.set_path("/mcp");
    }

    Ok(url.to_string())
}

fn safe_http_log_target(address: &str) -> String {
    let Ok(mut url) = Url::parse(address) else {
        return "<invalid-url>".to_string();
    };
    // Never log user info, query parameters, or fragments. They can contain
    // bearer tokens or other credentials supplied by a deployment.
    url.set_username("").ok();
    url.set_password(None).ok();
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn connection_error(_error: impl std::fmt::Display) -> McpClientError {
    // SDK transport errors can embed a full URL or a response-body preview.
    // Keep that material out of logs and status strings; callers still get a
    // typed connection error and the SDK's cancellation/timeout distinctions.
    McpClientError::Connection("MCP connection negotiation failed".to_string())
}

fn initialize_error(error: ClientInitializeError) -> McpClientError {
    match error {
        ClientInitializeError::Cancelled => McpClientError::Cancelled {
            operation: "connect",
        },
        other => connection_error(other),
    }
}

fn service_error(operation: &'static str, error: rmcp::ServiceError) -> McpClientError {
    match error {
        rmcp::ServiceError::Cancelled { .. } => McpClientError::Cancelled { operation },
        rmcp::ServiceError::Timeout { .. } => McpClientError::Timeout { operation },
        rmcp::ServiceError::McpError(_) => McpClientError::Protocol {
            operation,
            message: "MCP server returned a JSON-RPC protocol error".to_string(),
        },
        rmcp::ServiceError::TransportSend(_) => McpClientError::Protocol {
            operation,
            message: "MCP transport send failed".to_string(),
        },
        rmcp::ServiceError::TransportClosed => McpClientError::Protocol {
            operation,
            message: "MCP transport closed unexpectedly".to_string(),
        },
        rmcp::ServiceError::UnexpectedResponse => McpClientError::Protocol {
            operation,
            message: "MCP server returned an unexpected response type".to_string(),
        },
        rmcp::ServiceError::SubscriptionLagged { .. }
        | rmcp::ServiceError::InputRequiredRoundsExceeded { .. } => McpClientError::Protocol {
            operation,
            message: "MCP service returned an unsupported lifecycle response".to_string(),
        },
        _other => McpClientError::Protocol {
            operation,
            message: format!("MCP service operation failed ({})", operation),
        },
    }
}

async fn send_cancellation_notification_bounded(
    peer: &rmcp::Peer<RoleClient>,
    request_id: RequestId,
    reason: &str,
) {
    let notification = CancelledNotification::new(CancelledNotificationParam::new(
        Some(request_id),
        Some(reason.to_string()),
    ));
    let _ = timeout(
        CANCEL_NOTIFICATION_SEND_TIMEOUT,
        peer.send_notification(notification.into()),
    )
    .await;
}

async fn send_request_with_limits(
    client: &rmcp::service::RunningService<RoleClient, DogeMcpClientHandler>,
    request: ClientRequest,
    timeout_ms: u64,
    cancel_token: Option<&CancellationToken>,
    operation: &'static str,
) -> Result<ServerResult, McpClientError> {
    if cancel_token.is_some_and(CancellationToken::is_cancelled) {
        return Err(McpClientError::Cancelled { operation });
    }

    let deadline = (timeout_ms > 0).then(|| Instant::now() + Duration::from_millis(timeout_ms));
    let remaining = || {
        deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_default()
    };
    let options = if timeout_ms == 0 {
        PeerRequestOptions::no_options()
    } else {
        PeerRequestOptions::with_timeout(remaining())
    };
    let send_future = async {
        if let Some(token) = cancel_token {
            tokio::select! {
                biased;
                _ = token.cancelled() => Err(McpClientError::Cancelled { operation }),
                result = client.send_cancellable_request(request, options) => {
                    result.map_err(|error| service_error(operation, error))
                },
            }
        } else {
            client
                .send_cancellable_request(request, options)
                .await
                .map_err(|error| service_error(operation, error))
        }
    };
    let handle = if deadline.is_some() {
        match timeout(remaining(), send_future).await {
            Ok(result) => result?,
            Err(_) => {
                return Err(McpClientError::Timeout { operation });
            }
        }
    } else {
        send_future.await?
    };

    // Keep the peer/id before consuming the handle. If the agent cancels, send
    // the MCP cancellation notification explicitly instead of merely dropping
    // the local future (which could leave a side-effecting tool running).
    let peer = handle.peer.clone();
    let request_id = handle.id.clone();
    let response = async {
        if deadline.is_none() {
            return handle
                .await_response()
                .await
                .map_err(|error| service_error(operation, error));
        }

        let remaining = remaining();
        if remaining.is_zero() {
            send_cancellation_notification_bounded(&peer, request_id.clone(), "request timeout")
                .await;
            return Err(McpClientError::Timeout { operation });
        }
        match timeout(remaining, handle.await_response()).await {
            Ok(result) => result.map_err(|error| service_error(operation, error)),
            Err(_) => {
                send_cancellation_notification_bounded(
                    &peer,
                    request_id.clone(),
                    "request timeout",
                )
                .await;
                Err(McpClientError::Timeout { operation })
            }
        }
    };

    if let Some(token) = cancel_token {
        tokio::select! {
            biased;
            _ = token.cancelled() => {
                send_cancellation_notification_bounded(
                    &peer,
                    request_id,
                    "agent cancellation",
                )
                .await;
                Err(McpClientError::Cancelled { operation })
            }
            result = response => result,
        }
    } else {
        response.await
    }
}

async fn read_bounded_line<R>(reader: &mut R, max_bytes: usize) -> io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    let mut bytes = Vec::with_capacity(max_bytes.min(1024));
    let mut truncated = false;

    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if bytes.is_empty() && !truncated {
                return Ok(None);
            }
            break;
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        let chunk = &available[..consumed];
        let remaining = max_bytes.saturating_sub(bytes.len());
        let copy_len = chunk.len().min(remaining);
        bytes.extend_from_slice(&chunk[..copy_len]);
        if copy_len < chunk.len() {
            truncated = true;
        }
        reader.consume(consumed);

        if newline.is_some() {
            break;
        }
    }

    if truncated {
        bytes.extend_from_slice(b"...[stderr truncated]");
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

fn spawn_bounded_stderr(stderr: tokio::process::ChildStderr, server_name: String) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        loop {
            match read_bounded_line(&mut reader, MAX_STDERR_LINE_BYTES).await {
                Ok(Some(line)) => {
                    let chars = line.chars().count();
                    let was_truncated = line.ends_with("...[stderr truncated]");
                    warn!(
                        target: "mcp::client",
                        server = %server_name,
                        chars,
                        truncated = was_truncated,
                        "MCP server stderr output"
                    );
                }
                Ok(None) => break,
                Err(error) => {
                    warn!(
                        target: "mcp::client",
                        server = %server_name,
                        error = %error,
                        "failed to read MCP server stderr"
                    );
                    break;
                }
            }
        }
    });
}

fn should_subscribe_to_tool_list<S>(client: &rmcp::service::RunningService<RoleClient, S>) -> bool
where
    S: rmcp::Service<RoleClient>,
{
    client.peer_info().is_some_and(|peer| {
        peer.protocol_version.as_str() >= ProtocolVersion::V_2026_07_28.as_str()
            && peer
                .capabilities
                .tools
                .as_ref()
                .and_then(|tools| tools.list_changed)
                == Some(true)
    })
}

fn http_client_lifecycle(connect_timeout_ms: u64) -> ClientLifecycleMode {
    if connect_timeout_ms == 0 {
        // rmcp's Auto mode has a fixed discovery probe timeout. For an
        // explicitly unlimited connection, use the legacy lifecycle so the
        // configured zero timeout is not silently capped by that probe.
        ClientLifecycleMode::Initialize
    } else {
        ClientLifecycleMode::Auto {
            preferred_versions: vec![
                ProtocolVersion::V_2026_07_28,
                ProtocolVersion::V_2025_11_25,
                ProtocolVersion::V_2025_06_18,
                ProtocolVersion::V_2025_03_26,
                ProtocolVersion::V_2024_11_05,
            ],
            legacy_version: Some(ProtocolVersion::LATEST),
        }
    }
}

fn spawn_tool_list_subscription(
    peer: rmcp::Peer<RoleClient>,
    tool_list_changed: Arc<AtomicBool>,
    server_name: String,
) {
    tokio::spawn(async move {
        let filter = rmcp::model::SubscriptionFilter::builder()
            .tools_list_changed()
            .build();
        let mut subscription = match peer.listen(filter).await {
            Ok(subscription) => subscription,
            Err(_error) => {
                tool_list_changed.store(true, Ordering::Release);
                warn!(
                    target: "mcp::client",
                    server = %server_name,
                    "failed to subscribe to remote MCP tool list changes"
                );
                return;
            }
        };
        loop {
            match subscription.next().await {
                Ok(Some(_)) => {
                    tool_list_changed.store(true, Ordering::Release);
                }
                Ok(None) => {
                    tool_list_changed.store(true, Ordering::Release);
                    break;
                }
                Err(_error) => {
                    tool_list_changed.store(true, Ordering::Release);
                    warn!(
                        target: "mcp::client",
                        server = %server_name,
                        "remote MCP tool list subscription ended"
                    );
                    break;
                }
            }
        }
    });
}

fn stdio_command(config: &McpServerConfig) -> Result<(String, Vec<String>), McpClientError> {
    if let Some(command) = &config.command {
        return Ok((command.clone(), config.args.clone()));
    }
    let legacy = config.address.as_deref().ok_or_else(|| {
        McpClientError::Config(McpConfigError::MissingStdioCommand {
            name: config.name.clone(),
        })
    })?;
    let mut parts = legacy.split_whitespace();
    let program = parts.next().ok_or_else(|| {
        McpClientError::Config(McpConfigError::MissingStdioCommand {
            name: config.name.clone(),
        })
    })?;
    Ok((program.to_string(), parts.map(str::to_string).collect()))
}

/// Represents one negotiated outbound MCP connection.
///
/// The SDK owns protocol negotiation, transport I/O, and request correlation.
/// This type deliberately knows nothing about Doge's `ToolOutput` contract.
pub struct McpClient {
    client: rmcp::service::RunningService<RoleClient, DogeMcpClientHandler>,
    config: McpServerConfig,
    tool_list_changed: Arc<AtomicBool>,
}

impl McpClient {
    /// Create and negotiate an MCP connection from a validated server config.
    pub async fn from_config(config: &McpServerConfig) -> Result<Self, McpClientError> {
        Self::from_config_with_cancellation(config, None).await
    }

    /// Create and negotiate an MCP connection while honoring an agent
    /// cancellation token during the handshake.
    pub async fn from_config_with_cancellation(
        config: &McpServerConfig,
        cancel_token: Option<&CancellationToken>,
    ) -> Result<Self, McpClientError> {
        config.validate()?;

        let handler = DogeMcpClientHandler::new(config.name.clone());
        let tool_list_changed = handler.list_changed_flag();
        let mut normalized_config = config.clone();
        let client = match config.transport {
            McpTransport::Stdio => {
                let legacy_syntax = config.command.is_none();
                let (program, args) = stdio_command(config)?;
                if legacy_syntax {
                    warn!(
                        target: "mcp::client",
                        server = %config.name,
                        "deprecated MCP stdio address command syntax; migrate to command and args"
                    );
                }

                let mut command = Command::new(&program);
                command.args(&args);
                for (key, value) in &config.env {
                    command.env(key, value);
                }
                command.stdin(Stdio::piped()).stdout(Stdio::piped());

                let basename = Path::new(&program)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("<program>");
                debug!(
                    target: "mcp::client",
                    server = %config.name,
                    transport = "stdio",
                    command_basename = %basename,
                    arg_count = args.len(),
                    "starting remote MCP stdio server"
                );

                let (transport, stderr) = TokioChildProcess::builder(command)
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(connection_error)?;
                if let Some(stderr) = stderr {
                    spawn_bounded_stderr(stderr, config.name.clone());
                }

                // Most stdio MCP servers use the long-established initialize
                // lifecycle. Avoid paying the SDK's 10-second discover-probe
                // fallback on every legacy stdio launch; Streamable HTTP uses
                // Auto below for modern discovery.
                with_connection_timeout(
                    &config.name,
                    config.connect_timeout_ms,
                    cancel_token,
                    rmcp::service::serve_client_with_lifecycle(
                        handler,
                        transport,
                        ClientLifecycleMode::Initialize,
                    ),
                )
                .await?
            }
            McpTransport::Http => {
                let address = config.address.as_deref().ok_or_else(|| {
                    McpClientError::Config(McpConfigError::MissingHttpAddress {
                        name: config.name.clone(),
                    })
                })?;
                let http_uri = normalize_http_uri(address).map_err(|reason| {
                    McpClientError::Config(McpConfigError::InvalidHttpAddress {
                        name: config.name.clone(),
                        reason,
                    })
                })?;
                normalized_config.address = Some(http_uri.clone());
                let log_target = safe_http_log_target(&http_uri);
                debug!(
                    target: "mcp::client",
                    server = %config.name,
                    transport = "http",
                    endpoint = %log_target,
                    "starting remote MCP HTTP server"
                );
                let transport = StreamableHttpClientTransport::from_config(
                    StreamableHttpClientTransportConfig::with_uri(http_uri)
                        // A tools/call may have side effects. Do not let the
                        // transport transparently replay an ordinary POST when
                        // a session expires.
                        .reinit_on_expired_session(false),
                );

                // The SDK's Auto lifecycle probes the 2026-07-28 discovery
                // boundary first, then falls back to the legacy initialize
                // handshake for older servers. No tool calls are retried here.
                with_connection_timeout(
                    &config.name,
                    config.connect_timeout_ms,
                    cancel_token,
                    rmcp::service::serve_client_with_lifecycle(
                        handler,
                        transport,
                        http_client_lifecycle(config.connect_timeout_ms),
                    ),
                )
                .await?
            }
        };

        if should_subscribe_to_tool_list(&client) {
            spawn_tool_list_subscription(
                client.peer().clone(),
                tool_list_changed.clone(),
                config.name.clone(),
            );
        }

        Ok(Self {
            client,
            config: normalized_config,
            tool_list_changed,
        })
    }

    /// Return the negotiated peer information.
    pub async fn get_server_info(&self) -> Result<ServerPeerInfo, McpClientError> {
        self.client
            .peer_info()
            .as_deref()
            .cloned()
            .ok_or_else(|| McpClientError::Protocol {
                operation: "get_server_info",
                message: "server information is not available".to_string(),
            })
    }

    /// List every tool, following all MCP pagination cursors.
    pub async fn list_tools(&self) -> Result<Vec<Tool>, McpClientError> {
        self.list_tools_with_cancellation(None).await
    }

    /// List every tool with an optional agent cancellation token.
    pub async fn list_tools_with_cancellation(
        &self,
        cancel_token: Option<&CancellationToken>,
    ) -> Result<Vec<Tool>, McpClientError> {
        let deadline = (self.config.list_timeout_ms > 0)
            .then(|| Instant::now() + Duration::from_millis(self.config.list_timeout_ms));
        let mut cursor = None;
        let mut seen_cursors = HashSet::new();
        let mut tools = Vec::new();
        loop {
            let timeout_ms = if let Some(deadline) = deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(McpClientError::Timeout {
                        operation: "tools/list",
                    });
                }
                remaining.as_millis().max(1).min(u64::MAX as u128) as u64
            } else {
                0
            };
            let request = ClientRequest::ListToolsRequest(ListToolsRequest::with_param(
                PaginatedRequestParams::default().with_cursor(cursor.clone()),
            ));
            let response = send_request_with_limits(
                &self.client,
                request,
                timeout_ms,
                cancel_token,
                "tools/list",
            )
            .await?;
            let ServerResult::ListToolsResult(page) = response else {
                return Err(McpClientError::Protocol {
                    operation: "tools/list",
                    message: "MCP server returned an unexpected tools/list response".to_string(),
                });
            };
            tools.extend(page.tools);
            let Some(next_cursor) = page.next_cursor else {
                break;
            };
            if !seen_cursors.insert(next_cursor.clone()) {
                return Err(McpClientError::Protocol {
                    operation: "tools/list",
                    message: "MCP server repeated a tools/list pagination cursor".to_string(),
                });
            }
            cursor = Some(next_cursor);
        }
        info!(
            target: "mcp::client",
            server = %self.config.name,
            tool_count = tools.len(),
            "discovered remote MCP tools"
        );
        Ok(tools)
    }

    /// Send one `tools/call` request and return the SDK response boundary.
    ///
    /// We intentionally use `call_tool_once`: Doge-Code has no interactive
    /// elicitation UI, so an InputRequired response must be surfaced rather
    /// than silently driving another round trip.
    pub async fn call_tool(
        &self,
        params: CallToolRequestParams,
    ) -> Result<CallToolResponse, McpClientError> {
        self.call_tool_once(params, None).await
    }

    /// Send one `tools/call` request with optional cancellation.
    pub async fn call_tool_once(
        &self,
        params: CallToolRequestParams,
        cancel_token: Option<&CancellationToken>,
    ) -> Result<CallToolResponse, McpClientError> {
        let tool_name = params.name.to_string();
        info!(
            target: "mcp::client",
            server = %self.config.name,
            tool = %tool_name,
            "calling remote MCP tool"
        );
        // Do not log params: MCP arguments frequently contain credentials or
        // private user data.
        let request = ClientRequest::CallToolRequest(CallToolRequest::new(params));
        let response = send_request_with_limits(
            &self.client,
            request,
            self.config.call_timeout_ms,
            cancel_token,
            "tools/call",
        )
        .await?;
        match response {
            ServerResult::CallToolResult(result) => Ok(CallToolResponse::Complete(result)),
            ServerResult::InputRequiredResult(result) => {
                Ok(CallToolResponse::InputRequired(result))
            }
            ServerResult::CreateTaskResult(result) => Ok(CallToolResponse::Task(result)),
            _ => Err(McpClientError::Protocol {
                operation: "tools/call",
                message: "MCP server returned an unexpected tools/call response".to_string(),
            }),
        }
    }

    /// Shared flag set by the client handler when `tools/list_changed` arrives.
    pub fn tool_list_changed_flag(&self) -> Arc<AtomicBool> {
        self.tool_list_changed.clone()
    }

    /// Whether the SDK observed a tool-list change notification.
    pub fn tool_list_changed(&self) -> bool {
        self.tool_list_changed.load(Ordering::Acquire)
    }

    /// Atomically consume the notification flag.
    pub fn take_tool_list_changed(&self) -> bool {
        self.tool_list_changed.swap(false, Ordering::AcqRel)
    }

    pub fn get_config(&self) -> &McpServerConfig {
        &self.config
    }
}

async fn with_connection_timeout<F>(
    server_name: &str,
    timeout_ms: u64,
    cancel_token: Option<&CancellationToken>,
    future: F,
) -> Result<rmcp::service::RunningService<RoleClient, DogeMcpClientHandler>, McpClientError>
where
    F: Future<
        Output = Result<
            rmcp::service::RunningService<RoleClient, DogeMcpClientHandler>,
            ClientInitializeError,
        >,
    >,
{
    let future = Box::pin(future);
    let timed_future = async {
        if timeout_ms == 0 {
            future.await.map_err(initialize_error)
        } else {
            match timeout(Duration::from_millis(timeout_ms), future).await {
                Ok(Ok(service)) => Ok(service),
                Ok(Err(error)) => Err(initialize_error(error)),
                Err(_) => {
                    warn!(
                        target: "mcp::client",
                        server = %server_name,
                        "remote MCP connection timed out"
                    );
                    Err(McpClientError::Timeout {
                        operation: "connect",
                    })
                }
            }
        }
    };

    if let Some(token) = cancel_token {
        tokio::select! {
            biased;
            _ = token.cancelled() => Err(McpClientError::Cancelled { operation: "connect" }),
            result = timed_future => result,
        }
    } else {
        timed_future.await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::{
        RoleServer, ServerHandler, ServiceExt,
        model::{
            CallToolResponse, CallToolResult, CancelledNotificationParam, ContentBlock,
            ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
        },
        service::{NotificationContext, RequestContext},
    };
    use serde_json::Map as JsonMap;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn structured_stdio_command_keeps_spaces_in_program_and_arguments() {
        let config = McpServerConfig {
            name: "stdio".to_string(),
            enabled: true,
            transport: McpTransport::Stdio,
            command: Some("/tmp/My MCP Server/bin/server".to_string()),
            args: vec!["--config".to_string(), "/tmp/foo config.json".to_string()],
            address: None,
            ..McpServerConfig::default()
        };
        let (program, args) = stdio_command(&config).expect("structured command");
        assert_eq!(program, "/tmp/My MCP Server/bin/server");
        assert_eq!(args, vec!["--config", "/tmp/foo config.json"]);
    }

    #[test]
    fn unlimited_http_connection_uses_initialize_lifecycle() {
        assert!(matches!(
            http_client_lifecycle(0),
            ClientLifecycleMode::Initialize
        ));
        assert!(matches!(
            http_client_lifecycle(1),
            ClientLifecycleMode::Auto { .. }
        ));
    }

    #[test]
    fn normalize_adds_scheme_and_path_when_missing() {
        let normalized = normalize_http_uri("127.0.0.1:8000").expect("should normalize");
        assert_eq!(normalized, "http://127.0.0.1:8000/mcp");
    }

    #[test]
    fn normalize_preserves_existing_path() {
        let normalized =
            normalize_http_uri("http://127.0.0.1:8000/custom").expect("should normalize");
        assert_eq!(normalized, "http://127.0.0.1:8000/custom");
    }

    #[test]
    fn normalize_handles_https() {
        let normalized = normalize_http_uri("https://example.com/api").expect("should normalize");
        assert_eq!(normalized, "https://example.com/api");
    }

    #[test]
    fn normalize_errors_on_empty_input() {
        assert!(normalize_http_uri("").is_err());
    }

    #[test]
    fn normalize_rejects_non_http_scheme() {
        assert!(normalize_http_uri("file:///tmp/server").is_err());
        assert!(normalize_http_uri("ftp://example.com/mcp").is_err());
    }

    #[test]
    fn invalid_http_uri_error_does_not_echo_credentials() {
        let error =
            normalize_http_uri("https://user:secret@").expect_err("missing host should fail");
        assert!(!error.contains("secret"));
        assert!(!error.contains("user"));
    }

    #[test]
    fn safe_http_log_target_removes_credentials_and_query() {
        let target =
            safe_http_log_target("https://user:secret@example.com:8443/mcp?token=secret#x");
        assert_eq!(target, "https://example.com:8443/mcp");
        assert!(!target.contains("secret"));
    }

    #[tokio::test]
    async fn bounded_stderr_reader_caps_long_lines() {
        let (mut writer, reader) = tokio::io::duplex(128 * 1024);
        let payload = "x".repeat(MAX_STDERR_LINE_BYTES * 2);
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            writer.write_all(payload.as_bytes()).await.ok();
            writer.write_all(b"\n").await.ok();
        });
        let mut buffered = BufReader::new(reader);
        let line = read_bounded_line(&mut buffered, 32)
            .await
            .expect("read should succeed")
            .expect("line should exist");
        assert!(line.ends_with("...[stderr truncated]"));
        assert!(line.len() < 128);
    }

    #[test]
    fn tool_list_handler_flag_is_consumed_atomically() {
        let handler = DogeMcpClientHandler::new("test");
        let flag = handler.list_changed_flag();
        flag.store(true, Ordering::Release);
        assert!(flag.swap(false, Ordering::AcqRel));
        assert!(!flag.swap(false, Ordering::AcqRel));
    }

    #[derive(Clone, Default)]
    struct PaginatedServer {
        calls: Arc<AtomicUsize>,
    }

    impl ServerHandler for PaginatedServer {
        fn get_info(&self) -> ServerConfig {
            ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
        }

        async fn call_tool(
            &self,
            _request: rmcp::model::CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(rmcp::ErrorData::internal_error(
                "fixture protocol failure",
                None,
            ))
        }

        async fn list_tools(
            &self,
            request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, rmcp::ErrorData> {
            let cursor = request.and_then(|request| request.cursor);
            let (name, next_cursor) = match cursor.as_deref() {
                None => ("tool-a", Some("page-2".to_string())),
                Some("page-2") => ("tool-b", None),
                Some(_) => return Err(rmcp::ErrorData::invalid_params("bad cursor", None)),
            };
            let mut schema = JsonMap::new();
            schema.insert("type".to_string(), serde_json::json!("object"));
            let mut result = ListToolsResult::with_all_items(vec![Tool::new(
                name,
                "fixture tool",
                std::sync::Arc::new(schema),
            )]);
            result.next_cursor = next_cursor;
            Ok(result)
        }
    }

    #[derive(Clone)]
    struct CancellationServer {
        started: Arc<Notify>,
        cancelled: Arc<Notify>,
    }

    impl ServerHandler for CancellationServer {
        fn get_info(&self) -> ServerConfig {
            ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
        }

        async fn call_tool(
            &self,
            _request: rmcp::model::CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, rmcp::ErrorData> {
            self.started.notify_one();
            std::future::pending::<Result<CallToolResponse, rmcp::ErrorData>>().await
        }

        async fn on_cancelled(
            &self,
            _notification: CancelledNotificationParam,
            _context: NotificationContext<RoleServer>,
        ) {
            self.cancelled.notify_one();
        }
    }

    #[tokio::test]
    async fn cancellation_sends_notifications_cancelled_to_server() {
        let (server_transport, client_transport) = tokio::io::duplex(128 * 1024);
        let started = Arc::new(Notify::new());
        let cancelled = Arc::new(Notify::new());
        let server = CancellationServer {
            started: started.clone(),
            cancelled: cancelled.clone(),
        };
        let server_task = tokio::spawn(async move {
            if let Ok(running) = server.serve(server_transport).await {
                let _ = running.waiting().await;
            }
        });

        let handler = DogeMcpClientHandler::new("cancellation-fixture");
        let tool_list_changed = handler.list_changed_flag();
        let running = handler
            .serve(client_transport)
            .await
            .expect("fixture client should initialize");
        let client = McpClient {
            client: running,
            config: McpServerConfig {
                call_timeout_ms: 0,
                ..McpServerConfig::default()
            },
            tool_list_changed,
        };

        let token = CancellationToken::new();
        let cancel_token = token.clone();
        let cancel_task = tokio::spawn(async move {
            tokio::time::timeout(std::time::Duration::from_secs(5), started.notified())
                .await
                .expect("server should receive tools/call");
            cancel_token.cancel();
        });

        let result = client
            .call_tool_once(CallToolRequestParams::new("slow"), Some(&token))
            .await;
        assert!(matches!(
            result,
            Err(McpClientError::Cancelled {
                operation: "tools/call"
            })
        ));
        cancel_task
            .await
            .expect("cancellation task should complete");
        tokio::time::timeout(std::time::Duration::from_secs(5), cancelled.notified())
            .await
            .expect("server should receive notifications/cancelled");

        drop(client);
        server_task.abort();
        let _ = server_task.await;
    }

    #[derive(Clone, Default)]
    struct RepeatingCursorServer;

    impl ServerHandler for RepeatingCursorServer {
        fn get_info(&self) -> ServerConfig {
            ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
        }

        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, rmcp::ErrorData> {
            let mut schema = JsonMap::new();
            schema.insert("type".to_string(), serde_json::json!("object"));
            let mut result = ListToolsResult::with_all_items(vec![Tool::new(
                "tool",
                "fixture tool",
                std::sync::Arc::new(schema),
            )]);
            result.next_cursor = Some("same-cursor".to_string());
            Ok(result)
        }
    }

    #[tokio::test]
    async fn list_tools_rejects_repeated_pagination_cursor() {
        let (server_transport, client_transport) = tokio::io::duplex(128 * 1024);
        let server_task = tokio::spawn(async move {
            if let Ok(running) = RepeatingCursorServer.serve(server_transport).await {
                let _ = running.waiting().await;
            }
        });
        let handler = DogeMcpClientHandler::new("pagination-fixture");
        let tool_list_changed = handler.list_changed_flag();
        let running = handler
            .serve(client_transport)
            .await
            .expect("fixture client should initialize");
        let client = McpClient {
            client: running,
            config: McpServerConfig {
                list_timeout_ms: 0,
                ..McpServerConfig::default()
            },
            tool_list_changed,
        };

        let error = client
            .list_tools()
            .await
            .expect_err("a repeated cursor must not loop forever");
        assert!(matches!(
            error,
            McpClientError::Protocol {
                operation: "tools/list",
                ..
            }
        ));
        drop(client);
        server_task.abort();
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn list_tools_follows_all_sdk_pagination_pages() {
        let (server_transport, client_transport) = tokio::io::duplex(128 * 1024);
        let calls = Arc::new(AtomicUsize::new(0));
        let server = PaginatedServer {
            calls: calls.clone(),
        };
        let server_task = tokio::spawn(async move {
            if let Ok(running) = server.serve(server_transport).await {
                let _ = running.waiting().await;
            }
        });
        let handler = DogeMcpClientHandler::new("fixture");
        let tool_list_changed = handler.list_changed_flag();
        let running = handler
            .serve(client_transport)
            .await
            .expect("fixture client should initialize");
        let client = McpClient {
            client: running,
            config: McpServerConfig::default(),
            tool_list_changed,
        };

        let tools = client
            .list_tools()
            .await
            .expect("paginated list should work");
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.name.as_ref())
                .collect::<Vec<_>>(),
            vec!["tool-a", "tool-b"]
        );
        let protocol_error = client
            .call_tool(CallToolRequestParams::new("tool-a"))
            .await
            .expect_err("JSON-RPC error must remain a typed client error");
        assert!(matches!(
            protocol_error,
            McpClientError::Protocol {
                operation: "tools/call",
                ..
            }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "tools/call must not retry");
        drop(client);
        let _ = server_task.await;
    }

    #[test]
    fn complete_call_result_type_is_available_without_manual_json_parsing() {
        // This small assertion keeps the test tied to the SDK model rather
        // than to a hand-written `isError` JSON parser.
        let mut result = CallToolResult::success(vec![ContentBlock::text("ok")]);
        result.structured_content = Some(serde_json::json!({"ok": true}));
        result.is_error = None;
        assert!(result.is_error.is_none());
    }
}
