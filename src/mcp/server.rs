use crate::analysis::RepoMap;
use crate::analysis::semantic::SemanticService;
use crate::config::McpServerConfig;
use crate::mcp::service::DogeMcpService;
use anyhow::Result;
use rmcp::transport::streamable_http_server::{
    StreamableHttpService, session::local::LocalSessionManager,
};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::info;

pub fn start_mcp_server(
    config: &McpServerConfig,
    repomap: Arc<RwLock<Option<RepoMap>>>,
    semantic_service: Option<SemanticService>,
) -> Option<JoinHandle<Result<()>>> {
    if !config.enabled {
        return None;
    }

    let config = config.clone();
    let semantic_service = semantic_service.clone();
    let handle = tokio::spawn(async move {
        info!("Starting MCP server at {}", &config.address);

        let service = StreamableHttpService::new(
            move || {
                let service = DogeMcpService::default()
                    .with_repomap(repomap.clone())
                    .with_semantic_service(semantic_service.clone());
                Ok(service)
            },
            LocalSessionManager::default().into(),
            Default::default(),
        );

        let router = axum::Router::new().nest_service("/mcp", service);
        let tcp_listener = tokio::net::TcpListener::bind(&config.address).await?;
        axum::serve(tcp_listener, router)
            .with_graceful_shutdown(async { tokio::signal::ctrl_c().await.unwrap() })
            .await?;
        Ok(())
    });

    Some(handle)
}
