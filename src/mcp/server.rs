use crate::analysis::RepoMap;
use crate::config::AppConfig;
use crate::mcp::http_security::{
    MAX_MCP_REQUEST_BODY_BYTES, mcp_security_middleware, validate_local_bind_address,
};
use crate::mcp::service::{DogeMcpService, McpServiceState};
use anyhow::{Context, Result};
use rmcp::transport::streamable_http_server::{
    StreamableHttpService, session::local::LocalSessionManager,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Owned handle for a running local MCP HTTP listener.
///
/// `bind` has already succeeded when this handle exists (bind-before-spawn),
/// so callers can rely on `local_addr()` for the real port. The normal path
/// must call `shutdown().await` (or `wait().await`); `Drop` only cancels the
/// shutdown token as a safety net because async drop is impossible.
pub struct McpServerHandle {
    local_addr: SocketAddr,
    shutdown_token: CancellationToken,
    task: Option<JoinHandle<Result<()>>>,
}

impl McpServerHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Signal shutdown and wait for the server task to finish.
    pub async fn shutdown(mut self) -> Result<()> {
        self.shutdown_token.cancel();
        match self.task.take() {
            Some(task) => match task.await {
                Ok(res) => res,
                Err(e) if e.is_cancelled() => Ok(()),
                Err(e) => Err(anyhow::anyhow!("MCP server task panicked: {e}")),
            },
            None => Ok(()),
        }
    }

    /// Wait for the server task to finish without signalling shutdown.
    pub async fn wait(mut self) -> Result<()> {
        match self.task.take() {
            Some(task) => match task.await {
                Ok(res) => res,
                Err(e) if e.is_cancelled() => Ok(()),
                Err(e) => Err(anyhow::anyhow!("MCP server task panicked: {e}")),
            },
            None => Ok(()),
        }
    }
}

impl Drop for McpServerHandle {
    fn drop(&mut self) {
        // Safety net only: the normal path must `.shutdown().await`.
        self.shutdown_token.cancel();
    }
}

/// Spawn the local MCP HTTP listener.
///
/// Steps: security validation -> `TcpListener::bind()` -> router build ->
/// `tokio::spawn(axum::serve(...))`. Bind failures (port in use, invalid
/// address, permission denied) and non-loopback requests are returned
/// synchronously to the caller; a returned handle always means the socket is
/// already bound.
pub async fn spawn_mcp_server(
    address: &str,
    app_config: Arc<AppConfig>,
    repomap: Arc<RwLock<Option<RepoMap>>>,
) -> Result<McpServerHandle> {
    validate_local_bind_address(address)
        .with_context(|| format!("invalid local MCP bind address '{address}'"))?;

    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind MCP server to '{address}'"))?;
    let local_addr = listener
        .local_addr()
        .context("failed to read MCP listener address")?;

    let state = Arc::new(McpServiceState::new(app_config.clone(), repomap));

    let service = StreamableHttpService::new(
        {
            let state = state.clone();
            move || Ok(DogeMcpService::new(state.clone()))
        },
        LocalSessionManager::default().into(),
        Default::default(),
    );

    // Security layers: Host/Origin validation runs outermost, then the
    // request-body limit (covers Content-Length and chunked bodies), then
    // the MCP service. Layer order: last `layer()` call is outermost.
    let router = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            MAX_MCP_REQUEST_BODY_BYTES,
        ))
        .layer(axum::middleware::from_fn(mcp_security_middleware));

    let shutdown_token = CancellationToken::new();
    let shutdown = shutdown_token.clone();

    tracing::info!(
        address = %local_addr,
        project_root = %app_config.project_root.display(),
        "MCP server listening"
    );

    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .context("MCP server failed")?;
        Ok(())
    });

    Ok(McpServerHandle {
        local_addr,
        shutdown_token,
        task: Some(task),
    })
}

/// Resolve the effective bind address for `dgc mcp-server [address]`.
///
/// Precedence: CLI address -> `[mcp_server].address` -> `127.0.0.1:8000`.
pub fn resolve_mcp_listen_address(cli_address: Option<&str>, local_cfg_address: &str) -> String {
    cli_address.map(str::to_string).unwrap_or_else(|| {
        if local_cfg_address.is_empty() {
            "127.0.0.1:8000".to_string()
        } else {
            local_cfg_address.to_string()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn test_app_config(root: &std::path::Path) -> Arc<AppConfig> {
        Arc::new(AppConfig {
            project_root: root.to_path_buf(),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn test_spawn_mcp_server_binds_before_return() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = test_app_config(tmp.path());
        let repomap = Arc::new(RwLock::new(None));

        let handle = spawn_mcp_server("127.0.0.1:0", app, repomap)
            .await
            .expect("spawn should succeed");
        let addr = handle.local_addr();
        assert_ne!(addr.port(), 0, "OS should assign a real port");

        // The listener is already bound: a TCP connect must succeed.
        let conn =
            tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(addr))
                .await
                .expect("connect should not time out")
                .expect("connect should succeed");
        drop(conn);

        handle.shutdown().await.expect("shutdown should succeed");
    }

    #[tokio::test]
    async fn test_spawn_mcp_server_reports_bind_failure() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = test_app_config(tmp.path());

        // Occupy a port first; spawning the MCP server on the same port must
        // fail synchronously (no background-task-only failure).
        let blocker = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("blocker bind");
        let addr = blocker.local_addr().expect("blocker addr");
        let repomap = Arc::new(RwLock::new(None));

        let result = spawn_mcp_server(&addr.to_string(), app, repomap).await;
        assert!(result.is_err(), "bind conflict must surface as Err");
    }

    #[tokio::test]
    async fn test_mcp_server_shutdown_completes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = test_app_config(tmp.path());
        let repomap = Arc::new(RwLock::new(None));

        let handle = spawn_mcp_server("127.0.0.1:0", app, repomap)
            .await
            .expect("spawn");
        tokio::time::timeout(Duration::from_secs(5), handle.shutdown())
            .await
            .expect("shutdown should not hang")
            .expect("shutdown ok");
    }

    #[tokio::test]
    async fn test_mcp_http_rejects_untrusted_host() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = test_app_config(tmp.path());
        let repomap = Arc::new(RwLock::new(None));
        let handle = spawn_mcp_server("127.0.0.1:0", app, repomap)
            .await
            .expect("spawn");
        let addr = handle.local_addr();

        let client = reqwest::Client::new();
        let res = client
            .post(format!("http://{addr}/mcp"))
            .header("Host", "evil.example")
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .await
            .expect("request");
        assert_eq!(res.status(), reqwest::StatusCode::FORBIDDEN);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn test_mcp_http_accepts_loopback_host() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = test_app_config(tmp.path());
        let repomap = Arc::new(RwLock::new(None));
        let handle = spawn_mcp_server("127.0.0.1:0", app, repomap)
            .await
            .expect("spawn");
        let addr = handle.local_addr();

        let client = reqwest::Client::new();
        let res = client
            .post(format!("http://{addr}/mcp"))
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .await
            .expect("request");
        // Invalid MCP body may yield 400/406/415/422, but it must NOT be the
        // 403 host-security rejection.
        assert_ne!(
            res.status(),
            reqwest::StatusCode::FORBIDDEN,
            "loopback host must pass security middleware"
        );

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn test_mcp_http_rejects_untrusted_origin() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = test_app_config(tmp.path());
        let repomap = Arc::new(RwLock::new(None));
        let handle = spawn_mcp_server("127.0.0.1:0", app, repomap)
            .await
            .expect("spawn");
        let addr = handle.local_addr();

        let client = reqwest::Client::new();
        let res = client
            .post(format!("http://{addr}/mcp"))
            .header("Origin", "https://evil.example")
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .await
            .expect("request");
        assert_eq!(res.status(), reqwest::StatusCode::FORBIDDEN);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn test_mcp_http_accepts_no_origin() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = test_app_config(tmp.path());
        let repomap = Arc::new(RwLock::new(None));
        let handle = spawn_mcp_server("127.0.0.1:0", app, repomap)
            .await
            .expect("spawn");
        let addr = handle.local_addr();

        let client = reqwest::Client::new();
        // No Origin header (plain MCP client): must pass the middleware.
        let res = client
            .post(format!("http://{addr}/mcp"))
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .await
            .expect("request");
        assert_ne!(
            res.status(),
            reqwest::StatusCode::FORBIDDEN,
            "missing Origin must be allowed"
        );

        // Valid loopback Origin must also pass the middleware.
        let res = client
            .post(format!("http://{addr}/mcp"))
            .header("Origin", "http://localhost:12345")
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .await
            .expect("request");
        assert_ne!(
            res.status(),
            reqwest::StatusCode::FORBIDDEN,
            "loopback Origin must be allowed"
        );

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn test_mcp_http_rejects_oversized_body() {
        use crate::mcp::http_security::MAX_MCP_REQUEST_BODY_BYTES;
        let tmp = tempfile::tempdir().expect("tempdir");
        let app = test_app_config(tmp.path());
        let repomap = Arc::new(RwLock::new(None));
        let handle = spawn_mcp_server("127.0.0.1:0", app, repomap)
            .await
            .expect("spawn");
        let addr = handle.local_addr();

        // Body over the limit (covers Content-Length path; chunked bodies
        // are bounded by the same layer).
        let big = "x".repeat(MAX_MCP_REQUEST_BODY_BYTES + 1024);
        let client = reqwest::Client::new();
        let res = client
            .post(format!("http://{addr}/mcp"))
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .body(big)
            .send()
            .await
            .expect("request");
        assert_eq!(
            res.status(),
            reqwest::StatusCode::PAYLOAD_TOO_LARGE,
            "oversized body must be rejected"
        );

        handle.shutdown().await.expect("shutdown");
    }

    #[test]
    fn test_resolve_mcp_listen_address_precedence() {
        // CLI wins.
        assert_eq!(
            resolve_mcp_listen_address(Some("127.0.0.1:1111"), "127.0.0.1:2222"),
            "127.0.0.1:1111"
        );
        // Then config.
        assert_eq!(
            resolve_mcp_listen_address(None, "127.0.0.1:9000"),
            "127.0.0.1:9000"
        );
        // Then built-in default.
        assert_eq!(resolve_mcp_listen_address(None, ""), "127.0.0.1:8000");
    }
}
