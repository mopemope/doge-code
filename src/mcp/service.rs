use crate::analysis::RepoMap;
use crate::config::AppConfig;
use crate::tools::search_repomap::RepomapSearchTools;
use crate::tools::search_repomap::repomap::{ResultDensity, SearchRepomapArgs};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

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
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct FsReadManyFilesParams {
    pub paths: Vec<String>,
    pub exclude: Option<Vec<String>>,
    pub recursive: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct SearchTextParams {
    pub search_pattern: String,
    pub file_glob: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct FsListParams {
    pub path: String,
    pub max_depth: Option<usize>,
    pub pattern: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct FindFileParams {
    pub filename: String,
}

#[derive(Clone)]
pub struct DogeMcpService {
    pub tool_router: ToolRouter<DogeMcpService>,
    repomap: Arc<RwLock<Option<RepoMap>>>,
    search_repomap_tools: RepomapSearchTools,
    config: Arc<AppConfig>,
}

impl Default for DogeMcpService {
    fn default() -> Self {
        Self::new(AppConfig::default())
    }
}

impl DogeMcpService {
    pub fn new(config: AppConfig) -> Self {
        Self {
            tool_router: Self::tool_router(),
            repomap: Arc::new(RwLock::new(None)),
            search_repomap_tools: RepomapSearchTools::new(),
            config: Arc::new(config),
        }
    }

    pub fn with_repomap(self, repomap: Arc<RwLock<Option<RepoMap>>>) -> Self {
        Self {
            tool_router: self.tool_router,
            repomap,
            search_repomap_tools: self.search_repomap_tools,
            config: self.config,
        }
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
        let repomap_guard = self.repomap.read().await;
        if let Some(map) = &*repomap_guard {
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
            };

            match self.search_repomap_tools.search_repomap(map, args) {
                Ok(results) => self.format_json_result(results),
                Err(e) => {
                    Err(self.format_error("Search repomap failed ", Some(json!(e.to_string()))))
                }
            }
        } else {
            Err(self.format_error(
                "Repomap is not available ",
                Some(json!("Repomap is still generating or not initialized ")),
            ))
        }
    }

    #[tool(description = "Read the content of a text file ")]
    pub fn fs_read(
        &self,
        Parameters(params): Parameters<FsReadParams>,
    ) -> Result<CallToolResult, McpError> {
        match crate::tools::read::fs_read(
            &params.path,
            params.start_line,
            params.limit,
            &self.config,
        ) {
            Ok(content) => Ok(CallToolResult::success(vec![Content::text(content)])),
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
            &self.config,
        ) {
            Ok(content) => Ok(CallToolResult::success(vec![Content::text(content)])),
            Err(e) => Err(self.format_error("Failed to read files ", Some(json!(e.to_string())))),
        }
    }

    #[tool(description = "Search for text within files using ripgrep ")]
    pub fn search_text(
        &self,
        Parameters(params): Parameters<SearchTextParams>,
    ) -> Result<CallToolResult, McpError> {
        match crate::tools::search_text::search_text(
            &params.search_pattern,
            params.file_glob.as_deref(),
            &self.config,
        ) {
            Ok(results) => {
                let formatted_results: Vec<String> = results
                    .into_iter()
                    .map(|(path, line, content)| {
                        format!("{}:{}: {}", path.display(), line, content)
                    })
                    .collect();
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
            &self.config,
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
            &self.config,
        )
        .await
        {
            Ok(result) => self.format_json_result(result),
            Err(e) => Err(self.format_error("Failed to find files ", Some(json!(e.to_string())))),
        }
    }
}

#[tool_handler]
impl ServerHandler for DogeMcpService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                "This server provides file system and code analysis tools.".to_string(),
            ),
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParam>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        // Not implemented yet
        Ok(ListResourcesResult {
            resources: vec![],
            next_cursor: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParam,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        // Not implemented yet
        Err(McpError::resource_not_found(
            "resource_not_found",
            Some(serde_json::Value::Object(serde_json::Map::from_iter(vec![
                ("uri".to_string(), serde_json::Value::String(request.uri)),
            ]))),
        ))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParam>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        // Not implemented yet
        Ok(ListResourceTemplatesResult {
            next_cursor: None,
            resource_templates: Vec::new(),
        })
    }

    async fn initialize(
        &self,
        _request: InitializeRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        Ok(self.get_info())
    }
}
