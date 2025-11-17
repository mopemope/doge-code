use crate::config::AppConfig;
use crate::mcp::client::McpClient;
use anyhow::{Result, anyhow};
use rmcp::model::CallToolRequestParam;
use serde_json::{Map as JsonMap, Value as JsonValue, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex as AsyncMutex, RwLock};
use tracing::{debug, warn};

#[derive(Clone)]
pub struct RemoteToolInfo {
    pub alias: String,
    pub remote_name: String,
    pub server_name: String,
    pub description: Option<String>,
    pub parameters: JsonValue,
    pub strict: Option<bool>,
    client: std::sync::Arc<AsyncMutex<McpClient>>,
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

#[derive(Debug, Default, Clone)]
pub struct RemoteToolRegistry {
    pub tools: Vec<RemoteToolInfo>,
    pub lookup: HashMap<String, RemoteToolInfo>,
}

#[derive(Debug, Clone)]
pub struct RemoteToolManager {
    remote_tools: std::sync::Arc<RwLock<Option<RemoteToolRegistry>>>,
    config: std::sync::Arc<AppConfig>,
}

impl RemoteToolManager {
    pub fn new(config: std::sync::Arc<AppConfig>) -> Self {
        Self {
            remote_tools: std::sync::Arc::new(RwLock::new(None)),
            config,
        }
    }

    pub async fn ensure_remote_tools(&self) -> Result<()> {
        if self.remote_tools.read().await.is_some() {
            return Ok(());
        }

        let registry = self.build_remote_registry().await;
        let mut guard = self.remote_tools.write().await;
        *guard = Some(registry);
        Ok(())
    }

    async fn build_remote_registry(&self) -> RemoteToolRegistry {
        let mut registry = RemoteToolRegistry::default();
        let mut alias_counts: HashMap<String, usize> = HashMap::new();

        for server_cfg in &self.config.mcp_servers {
            if !server_cfg.enabled {
                continue;
            }

            let server_name = server_cfg.name.clone();
            match McpClient::from_config(server_cfg).await {
                Ok(client) => {
                    let client = std::sync::Arc::new(AsyncMutex::new(client));

                    let list_result = {
                        let client_guard = client.lock().await;
                        client_guard.list_tools().await
                    };

                    let list_result = match list_result {
                        Ok(res) => res,
                        Err(e) => {
                            warn!(
                                server = %server_name,
                                error = %e,
                                "Failed to list tools from remote MCP server"
                            );
                            continue;
                        }
                    };

                    for tool in list_result.tools {
                        let alias_base = format!(
                            "mcp_{}_{}",
                            sanitize_identifier(&server_name),
                            sanitize_identifier(tool.name.as_ref())
                        );
                        let alias = generate_unique_alias(alias_base, &mut alias_counts);

                        let params_value =
                            JsonValue::Object(Arc::as_ref(&tool.input_schema).clone());

                        let mut description_parts = Vec::new();
                        if let Some(desc) = &tool.description {
                            description_parts.push(desc.to_string());
                        }
                        if let Some(annotations) = &tool.annotations
                            && let Some(title) = &annotations.title
                            && description_parts.is_empty()
                        {
                            description_parts.push(title.clone());
                        }

                        let description = if description_parts.is_empty() {
                            Some(format!(
                                "Remote MCP tool '{}' provided by server '{}'",
                                tool.name, server_name
                            ))
                        } else {
                            Some(format!(
                                "Remote MCP tool '{}' on server '{}': {}",
                                tool.name,
                                server_name,
                                description_parts.join(" ")
                            ))
                        };

                        let info = RemoteToolInfo {
                            alias: alias.clone(),
                            remote_name: tool.name.to_string(),
                            server_name: server_name.clone(),
                            description,
                            parameters: params_value,
                            strict: None,
                            client: client.clone(),
                        };

                        registry.lookup.insert(alias.clone(), info.clone());
                        registry.tools.push(info);
                    }
                }
                Err(e) => {
                    warn!(
                        server = %server_name,
                        error = %e,
                        "Failed to initialize MCP client for remote server"
                    );
                }
            }
        }

        debug!(count = registry.tools.len(), "Registered remote MCP tools");

        registry
    }

    pub async fn remote_tools_snapshot(&self) -> Vec<RemoteToolInfo> {
        self.remote_tools
            .read()
            .await
            .as_ref()
            .map(|registry| registry.tools.clone())
            .unwrap_or_default()
    }

    pub async fn call_remote_tool(
        &self,
        alias: &str,
        args: &JsonValue,
    ) -> Result<Option<JsonValue>> {
        self.ensure_remote_tools().await?;

        let tool_opt = {
            let guard = self.remote_tools.read().await;
            guard
                .as_ref()
                .and_then(|registry| registry.lookup.get(alias).cloned())
        };

        let Some(tool) = tool_opt else {
            return Ok(None);
        };

        let arguments: Option<JsonMap<String, JsonValue>> = match args {
            JsonValue::Null => None,
            JsonValue::Object(map) => Some(map.clone()),
            other => {
                return Err(anyhow!(
                    "Remote MCP tool '{}' expects object arguments, received {}",
                    tool.alias,
                    other
                ));
            }
        };

        let params = CallToolRequestParam {
            name: tool.remote_name.clone().into(),
            arguments,
        };

        let result = {
            let client = tool.client.clone();
            let client_guard = client.lock().await;
            client_guard.call_tool(params).await
        };

        match result {
            Ok(call_result) => {
                let value = serde_json::to_value(&call_result)?;
                Ok(Some(json!({
                    "ok": true,
                    "server": tool.server_name,
                    "tool": tool.remote_name,
                    "result": value
                })))
            }
            Err(e) => {
                warn!(
                    server = %tool.server_name,
                    tool = %tool.remote_name,
                    error = %e,
                    "Remote MCP tool call failed"
                );
                Err(anyhow!(
                    "Remote MCP tool '{}' on server '{}' failed: {}",
                    tool.remote_name,
                    tool.server_name,
                    e
                ))
            }
        }
    }
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

fn generate_unique_alias(base: String, counts: &mut HashMap<String, usize>) -> String {
    match counts.get_mut(&base) {
        Some(counter) => {
            *counter += 1;
            format!("{}_{}", base, *counter)
        }
        None => {
            counts.insert(base.clone(), 1);
            base
        }
    }
}
