use crate::analysis::RepoMap;
use crate::analysis::cache::ensure_repomap_ready;
use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use crate::mcp::resource_path::{ResourcePathError, resolve_project_resource_path};
use crate::tools::list::{FsListMode, FsListOptions};
use crate::tools::read::{FsReadMode, FsReadOptions};
use crate::tools::read_many::FsReadManyOptions;
use crate::tools::search_repomap::RepomapSearchTools;
use crate::tools::search_repomap::repomap::{ResultDensity, SearchRepomapArgs};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use schemars::schema_for;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

// Tool parameter structures
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct SearchRepomapParams {
    pub result_density: Option<String>,
    pub max_file_lines: Option<u32>,
    pub max_function_lines: Option<u32>,
    pub file_pattern: Option<String>,
    pub exclude_patterns: Option<Vec<String>>,
    pub language_filters: Option<Vec<String>>,
    pub symbol_kinds: Option<Vec<String>>,
    pub sort_by: Option<String>,
    pub sort_desc: Option<bool>,
    pub limit: Option<u32>,
    pub keyword_search: Option<Vec<String>>,

    pub name: Option<Vec<String>>,
    pub fields: Option<Vec<String>>,
    pub include_snippets: Option<bool>,
    pub context_lines: Option<u32>,
    pub snippet_max_chars: Option<u32>,
    pub max_symbols_per_file: Option<u32>,
    pub match_score_threshold: Option<f64>,
    pub response_budget_chars: Option<u32>,
    pub cursor: Option<u32>,
    pub page_size: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct FsReadParams {
    pub path: String,
    pub start_line: Option<usize>,
    pub limit: Option<usize>,
    pub mode: Option<String>,
    pub response_budget_chars: Option<u32>,
    pub cursor: Option<u32>,
    pub page_size: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct FsReadManyFilesParams {
    pub paths: Vec<String>,
    pub exclude: Option<Vec<String>>,
    pub recursive: Option<bool>,
    pub mode: Option<String>,
    pub response_budget_chars: Option<u32>,
    pub cursor: Option<u32>,
    pub page_size: Option<u32>,
    pub max_entries: Option<u32>,
    pub snippet_max_chars: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct SearchTextParams {
    pub search_pattern: String,
    pub file_glob: Option<String>,
    pub max_results: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct FsListParams {
    pub path: String,
    pub max_depth: Option<usize>,
    pub pattern: Option<String>,
    pub mode: Option<String>,
    pub response_budget_chars: Option<u32>,
    pub cursor: Option<u32>,
    pub page_size: Option<u32>,
    pub max_entries: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct FindFileParams {
    pub filename: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct HandshakeParams {
    pub agent_id: String,
    pub capabilities: Vec<String>,
}

/// Shared state for all `DogeMcpService` instances serving one listener.
///
/// The Streamable HTTP service factory may create many service instances
/// (one per session/request). They must share the same `AppConfig`,
/// the same `RepoMap` slot, and — critically — the same repomap build lock,
/// otherwise concurrent first-requests could build the repomap in parallel.
#[derive(Clone)]
pub struct McpServiceState {
    pub config: Arc<AppConfig>,
    pub repomap: Arc<RwLock<Option<RepoMap>>>,
    pub repomap_build_lock: Arc<Mutex<()>>,
}

impl McpServiceState {
    pub fn new(config: Arc<AppConfig>, repomap: Arc<RwLock<Option<RepoMap>>>) -> Self {
        Self {
            config,
            repomap,
            repomap_build_lock: Arc::new(Mutex::new(())),
        }
    }
}

#[derive(Clone)]
pub struct DogeMcpService {
    pub tool_router: ToolRouter<DogeMcpService>,
    state: Arc<McpServiceState>,
    search_repomap_tools: RepomapSearchTools,
}

impl DogeMcpService {
    pub fn new(state: Arc<McpServiceState>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            state,
            search_repomap_tools: RepomapSearchTools::new(),
        }
    }

    /// Shared service state (config, repomap, build lock).
    pub fn state(&self) -> &Arc<McpServiceState> {
        &self.state
    }

    async fn ensure_repomap_ready(&self) -> Result<RepoMap, McpError> {
        if let Some(map) = self.state.repomap.read().await.clone() {
            return Ok(map);
        }

        let _guard = self.state.repomap_build_lock.lock().await;
        if let Some(map) = self.state.repomap.read().await.clone() {
            return Ok(map);
        }

        ensure_repomap_ready(&self.state.repomap, &self.state.config.project_root)
            .await
            .map_err(|e| self.format_error("Repomap build failed", Some(json!(e.to_string()))))
    }

    pub async fn list_resources_impl(&self) -> Result<ListResourcesResult, McpError> {
        let ready = self.state.repomap.read().await.is_some();
        let status_description = format!(
            "Repomap status: {}",
            if ready { "ready" } else { "warming" }
        );
        Ok(ListResourcesResult {
            resources: vec![
                Resource::new(
                    RawResource {
                        uri: "doge://repomap/summary".to_string(),
                        name: "RepoMap Summary".to_string(),
                        description: Some(
                            "Summary of the repository map, including statistics and top symbols."
                                .to_string(),
                        ),
                        mime_type: Some("application/json".to_string()),
                        icons: None,
                        size: None,
                        title: None,
                        meta: None,
                    },
                    None,
                ),
                Resource::new(
                    RawResource {
                        uri: "doge://repomap/status".to_string(),
                        name: "RepoMap Status".to_string(),
                        description: Some(status_description),
                        mime_type: Some("application/json".to_string()),
                        icons: None,
                        size: None,
                        title: None,
                        meta: None,
                    },
                    None,
                ),
                Resource::new(
                    RawResource {
                        uri: "agent://card".to_string(),
                        name: "Agent Card".to_string(),
                        description: Some(
                            "The Agent Card describing identity and capabilities (A2A Protocol)"
                                .to_string(),
                        ),
                        mime_type: Some("application/json".to_string()),
                        icons: None,
                        size: None,
                        title: None,
                        meta: None,
                    },
                    None,
                ),
            ],
            next_cursor: None,
            meta: None,
        })
    }

    pub async fn list_resource_templates_impl(
        &self,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult {
            next_cursor: None,
            meta: None,
            resource_templates: vec![
                ResourceTemplate::new(
                    RawResourceTemplate {
                        uri_template: "doge://files/{path}".to_string(),
                        name: "File Content".to_string(),
                        description: Some("Read the content of a file in the project.".to_string()),
                        mime_type: Some("text/plain".to_string()),
                        title: None,
                        icons: None,
                    },
                    None,
                ),
                ResourceTemplate::new(
                    RawResourceTemplate {
                        uri_template: "doge://symbols/{path}".to_string(),
                        name: "File Symbols".to_string(),
                        description: Some("Get the symbol map for a specific file.".to_string()),
                        mime_type: Some("application/json".to_string()),
                        title: None,
                        icons: None,
                    },
                    None,
                ),
            ],
        })
    }

    pub async fn read_resource_impl(&self, uri: String) -> Result<ReadResourceResult, McpError> {
        if uri == "doge://repomap/status" {
            let ready = self.state.repomap.read().await.is_some();
            let summary = json!({
                "status": if ready { "ready" } else { "warming" }
            });
            let content = serde_json::to_string_pretty(&summary).unwrap();
            return Ok(ReadResourceResult {
                contents: vec![ResourceContents::text(content, uri)],
            });
        }

        if uri == "agent://card" {
            let tools = self.get_tool_defs();
            let card = crate::a2a::generate_agent_card(&self.state.config, tools);
            let content = serde_json::to_string_pretty(&card).map_err(|e| {
                McpError::internal_error(format!("Serialization error: {}", e), None)
            })?;
            return Ok(ReadResourceResult {
                contents: vec![ResourceContents::text(content, uri)],
            });
        }

        if uri == "doge://repomap/summary" {
            let map = self.ensure_repomap_ready().await?;
            let summary = json!({
                "total_symbols": map.symbols.len(),
                "files_count": map.symbols.iter().map(|s| &s.file).collect::<std::collections::HashSet<_>>().len(),
            });
            let content = serde_json::to_string_pretty(&summary).unwrap();
            return Ok(ReadResourceResult {
                contents: vec![ResourceContents::text(content, uri)],
            });
        }

        if let Some(path_str) = uri.strip_prefix("doge://files/") {
            let canonical =
                match resolve_project_resource_path(&self.state.config.project_root, path_str) {
                    Ok(p) => p,
                    Err(ResourcePathError::InvalidPath) => {
                        return Err(McpError::resource_not_found(
                            "invalid project resource path",
                            None,
                        ));
                    }
                    Err(ResourcePathError::NotFound) => {
                        return Err(McpError::resource_not_found("resource not found", None));
                    }
                };

            // Note: reads the full file with no response budget (pre-existing
            // behavior, unlike the `fs_read` tool). Local-only listener, so
            // blast radius is limited; a response budget is a follow-up.
            let content = tokio::fs::read_to_string(&canonical)
                .await
                .map_err(|_| McpError::resource_not_found("resource not found", None))?;
            return Ok(ReadResourceResult {
                contents: vec![ResourceContents::text(content, uri.clone())],
            });
        }

        if let Some(path_str) = uri.strip_prefix("doge://symbols/") {
            let canonical_target =
                match resolve_project_resource_path(&self.state.config.project_root, path_str) {
                    Ok(p) => p,
                    Err(ResourcePathError::InvalidPath) => {
                        return Err(McpError::resource_not_found(
                            "invalid project resource path",
                            None,
                        ));
                    }
                    Err(ResourcePathError::NotFound) => {
                        return Err(McpError::resource_not_found("resource not found", None));
                    }
                };

            let map = self.ensure_repomap_ready().await?;
            // RepoMap stores absolute (not necessarily canonical) paths.
            // Match against both the canonical target and the non-canonical
            // project-root-joined form to handle symlinked tmpdirs (e.g.
            // /tmp -> /private/tmp on macOS) without canonicalizing every
            // symbol on every request.
            let project_root = &self.state.config.project_root;
            let canonical_root = project_root
                .canonicalize()
                .unwrap_or_else(|_| project_root.clone());
            let alt_target = canonical_target
                .strip_prefix(&canonical_root)
                .ok()
                .map(|rel| project_root.join(rel));
            let symbols: Vec<_> = map
                .symbols
                .iter()
                .filter(|s| {
                    s.file == canonical_target
                        || alt_target.as_ref().is_some_and(|alt| s.file == *alt)
                })
                .collect();
            let content = serde_json::to_string_pretty(&symbols).map_err(|e| {
                McpError::internal_error(format!("Serialization error: {}", e), None)
            })?;
            return Ok(ReadResourceResult {
                contents: vec![ResourceContents::text(content, uri)],
            });
        }

        Err(McpError::resource_not_found("resource not found", None))
    }

    fn _create_resource_text(&self, uri: &str, name: &str) -> Resource {
        RawResource::new(uri, name.to_string()).no_annotation()
    }

    /// Format a result as JSON text content
    pub fn format_json_result<T: serde::Serialize>(
        &self,
        result: T,
    ) -> Result<CallToolResult, McpError> {
        let json_result = serde_json::to_value(result).map_err(|e| {
            McpError::internal_error(
                "Serialization error ",
                Some(serde_json::Value::String(e.to_string())),
            )
        })?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json_result).unwrap_or_else(|_| json_result.to_string()),
        )]))
    }

    /// Format an error response
    pub fn format_error(&self, message: &str, details: Option<serde_json::Value>) -> McpError {
        McpError::internal_error(message.to_string(), details)
    }
}

#[tool_router]
impl DogeMcpService {
    #[tool(description = "Say hello to the client ")]
    pub fn say_hello(&self) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![Content::text("hello")]))
    }

    #[tool(description = "Search the repository map for symbols and code structures ")]
    pub async fn search_repomap(
        &self,
        Parameters(params): Parameters<SearchRepomapParams>,
    ) -> Result<CallToolResult, McpError> {
        let map = self.ensure_repomap_ready().await?;

        let result_density = params
            .result_density
            .as_deref()
            .and_then(|raw| ResultDensity::from_str(raw).ok());
        let args = SearchRepomapArgs {
            result_density,
            min_file_lines: None,
            max_file_lines: params.max_file_lines.map(|v| v as usize),
            min_function_lines: None,
            max_function_lines: params.max_function_lines.map(|v| v as usize),
            symbol_kinds: params.symbol_kinds,
            file_pattern: params.file_pattern,
            exclude_patterns: params.exclude_patterns,
            language_filters: params.language_filters,
            min_symbols_per_file: None,
            max_symbols_per_file: params.max_symbols_per_file.map(|v| v as usize),
            sort_by: params.sort_by,
            sort_desc: params.sort_desc,
            limit: params.limit.map(|v| v as usize),
            response_budget_chars: params.response_budget_chars.map(|v| v as usize),
            keyword_search: params.keyword_search,

            name: params.name,
            fields: params.fields,
            include_snippets: params.include_snippets,
            context_lines: params.context_lines.map(|v| v as usize),
            snippet_max_chars: params.snippet_max_chars.map(|v| v as usize),
            ranking_strategy: None,
            match_score_threshold: params.match_score_threshold,
            cursor: params.cursor.map(|v| v as usize),
            page_size: params.page_size.map(|v| v as usize),
            include_relations: None,
        };

        match self
            .search_repomap_tools
            .search_repomap(&map, args, &self.state.config.project_root)
            .await
        {
            Ok(results) => self.format_json_result(results),
            Err(e) => Err(self.format_error("Search repomap failed ", Some(json!(e.to_string())))),
        }
    }

    #[tool(description = "Read the content of a text file ")]
    pub fn fs_read(
        &self,
        Parameters(params): Parameters<FsReadParams>,
    ) -> Result<CallToolResult, McpError> {
        match crate::tools::read::fs_read(
            &params.path,
            FsReadOptions {
                start_line: params.start_line,
                limit: params.limit,
                cursor: params.cursor.map(|v| v as usize),
                page_size: params.page_size.map(|v| v as usize),
                response_budget_chars: params.response_budget_chars.map(|v| v as usize),
                mode: FsReadMode::from_optional_str(params.mode.as_deref()),
            },
            &self.state.config,
        ) {
            Ok(result) => self.format_json_result(result),
            Err(e) => Err(self.format_error("Failed to read file ", Some(json!(e.to_string())))),
        }
    }

    #[tool(description = "Read the content of multiple files ")]
    pub fn fs_read_many_files(
        &self,
        Parameters(params): Parameters<FsReadManyFilesParams>,
    ) -> Result<CallToolResult, McpError> {
        match crate::tools::read_many::fs_read_many_files(
            params.paths,
            params.exclude,
            params.recursive,
            &self.state.config,
            FsReadManyOptions {
                mode: FsReadMode::from_optional_str(params.mode.as_deref()),
                cursor: params.cursor.map(|v| v as usize),
                page_size: params.page_size.map(|v| v as usize),
                max_entries: params.max_entries.map(|v| v as usize),
                response_budget_chars: params.response_budget_chars.map(|v| v as usize),
                snippet_max_chars: params.snippet_max_chars.map(|v| v as usize),
            },
        ) {
            Ok(result) => self.format_json_result(result),
            Err(e) => Err(self.format_error("Failed to read files ", Some(json!(e.to_string())))),
        }
    }

    #[tool(description = "Search for text within files using ripgrep ")]
    pub fn search_text(
        &self,
        Parameters(params): Parameters<SearchTextParams>,
    ) -> Result<CallToolResult, McpError> {
        match crate::tools::search_text::search_text_with_options(
            &params.search_pattern,
            params.file_glob.as_deref(),
            crate::tools::search_text::SearchTextOptions {
                max_results: params.max_results.map(|v| v as usize),
                offset: params.offset.map(|v| v as usize),
                response_budget_chars: None,
            },
            &self.state.config,
        ) {
            Ok(results) => {
                let mut formatted_results: Vec<String> = results
                    .rows
                    .into_iter()
                    .map(|(path, line, content)| {
                        format!("{}:{}: {}", path.display(), line, content)
                    })
                    .collect();
                if results.truncated {
                    formatted_results
                        .push(format!("[truncated] next_offset={:?}", results.next_offset));
                }
                Ok(CallToolResult::success(vec![Content::text(
                    formatted_results.join("\n"),
                )]))
            }
            Err(e) => Err(self.format_error("Failed to search text ", Some(json!(e.to_string())))),
        }
    }

    #[tool(description = "List files and directories within a path ")]
    pub fn fs_list(
        &self,
        Parameters(params): Parameters<FsListParams>,
    ) -> Result<CallToolResult, McpError> {
        match crate::tools::list::fs_list(
            &params.path,
            params.max_depth,
            params.pattern.as_deref(),
            &self.state.config,
            FsListOptions {
                mode: FsListMode::from_optional_str(params.mode.as_deref()),
                cursor: params.cursor.map(|v| v as usize),
                page_size: params.page_size.map(|v| v as usize),
                max_entries: params.max_entries.map(|v| v as usize),
                response_budget_chars: params.response_budget_chars.map(|v| v as usize),
            },
        ) {
            Ok(files) => self.format_json_result(files),
            Err(e) => Err(self.format_error("Failed to list files ", Some(json!(e.to_string())))),
        }
    }

    #[tool(description = "Find files by name or pattern ")]
    pub async fn find_file(
        &self,
        Parameters(params): Parameters<FindFileParams>,
    ) -> Result<CallToolResult, McpError> {
        match crate::tools::find_file::find_file(
            crate::tools::find_file::FindFileArgs {
                filename: params.filename,
            },
            &self.state.config,
        )
        .await
        {
            Ok(result) => self.format_json_result(result),
            Err(e) => Err(self.format_error("Failed to find files ", Some(json!(e.to_string())))),
        }
    }

    #[tool(description = "Exchange agent information for collaboration (A2A Protocol) ")]
    pub fn handshake(
        &self,
        Parameters(params): Parameters<HandshakeParams>,
    ) -> Result<CallToolResult, McpError> {
        tracing::info!("A2A Handshake received from agent: {}", params.agent_id);

        let tools = self.get_tool_defs();
        let card = crate::a2a::generate_agent_card(&self.state.config, tools);

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&card).unwrap_or_default(),
        )]))
    }

    fn get_tool_defs(&self) -> Vec<ToolDef> {
        vec![
            ToolDef {
                kind: "function".to_string(),
                function: ToolFunctionDef {
                    name: "search_repomap".to_string(),
                    description: "Search the repository map for symbols and code structures"
                        .to_string(),
                    parameters: serde_json::to_value(schema_for!(SearchRepomapParams))
                        .unwrap_or_default(),
                    strict: None,
                },
            },
            ToolDef {
                kind: "function".to_string(),
                function: ToolFunctionDef {
                    name: "fs_read".to_string(),
                    description: "Read the content of a text file".to_string(),
                    parameters: serde_json::to_value(schema_for!(FsReadParams)).unwrap_or_default(),
                    strict: None,
                },
            },
            ToolDef {
                kind: "function".to_string(),
                function: ToolFunctionDef {
                    name: "fs_read_many_files".to_string(),
                    description: "Read the content of multiple files".to_string(),
                    parameters: serde_json::to_value(schema_for!(FsReadManyFilesParams))
                        .unwrap_or_default(),
                    strict: None,
                },
            },
            ToolDef {
                kind: "function".to_string(),
                function: ToolFunctionDef {
                    name: "search_text".to_string(),
                    description: "Search for text within files using ripgrep".to_string(),
                    parameters: serde_json::to_value(schema_for!(SearchTextParams))
                        .unwrap_or_default(),
                    strict: None,
                },
            },
            ToolDef {
                kind: "function".to_string(),
                function: ToolFunctionDef {
                    name: "fs_list".to_string(),
                    description: "List files and directories within a path".to_string(),
                    parameters: serde_json::to_value(schema_for!(FsListParams)).unwrap_or_default(),
                    strict: None,
                },
            },
            ToolDef {
                kind: "function".to_string(),
                function: ToolFunctionDef {
                    name: "find_file".to_string(),
                    description: "Find files by name or pattern".to_string(),
                    parameters: serde_json::to_value(schema_for!(FindFileParams))
                        .unwrap_or_default(),
                    strict: None,
                },
            },
            ToolDef {
                kind: "function".to_string(),
                function: ToolFunctionDef {
                    name: "handshake".to_string(),
                    description: "Exchange agent information for collaboration".to_string(),
                    parameters: serde_json::to_value(schema_for!(HandshakeParams))
                        .unwrap_or_default(),
                    strict: None,
                },
            },
        ]
    }
}

#[tool_handler]
impl ServerHandler for DogeMcpService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                "This server provides file system and code analysis tools. \
                 Resources available: \
                 - doge://repomap/summary: Overview of the codebase. \
                 - doge://files/{path}: Read file content. \
                 - doge://symbols/{path}: Get symbols for a file. \
                 - agent://card: Agent identity and capabilities (A2A Protocol)."
                    .to_string(),
            ),
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        self.list_resources_impl().await
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        self.read_resource_impl(request.uri).await
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        self.list_resource_templates_impl().await
    }

    async fn initialize(
        &self,
        _request: InitializeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        Ok(self.get_info())
    }
}
