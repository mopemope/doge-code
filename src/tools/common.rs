use crate::analysis::RepoMap;
use crate::config::AppConfig;
use crate::mcp::client::McpClient;
use crate::session::{SessionData, SessionManager};
use crate::tools::execute;
use crate::tools::find_file;
use crate::tools::list;
use crate::tools::read;
use crate::tools::read_many;
use crate::tools::search_repomap;
use crate::tools::search_text;
use crate::tools::todo_read;
use crate::tools::todo_write;
use crate::tools::write;
use anyhow::{Result, anyhow};
use rmcp::model::CallToolRequestParam;
use serde_json::{Map as JsonMap, Value as JsonValue, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as AsyncMutex, RwLock};
use tracing::{debug, warn};

#[derive(Debug, Clone)]
pub struct FsTools {
    search_repomap_tools: search_repomap::RepomapSearchTools,
    repomap: Arc<RwLock<Option<RepoMap>>>,
    pub session_manager: Option<Arc<Mutex<SessionManager>>>,
    pub config: Arc<AppConfig>,
    remote_tools: Arc<RwLock<Option<RemoteToolRegistry>>>,
}

impl Default for FsTools {
    fn default() -> Self {
        Self::new(Arc::new(RwLock::new(None)), Arc::new(AppConfig::default()))
    }
}

impl FsTools {
    pub fn new(repomap: Arc<RwLock<Option<RepoMap>>>, config: Arc<AppConfig>) -> Self {
        Self {
            search_repomap_tools: search_repomap::RepomapSearchTools::new(),
            repomap,
            session_manager: None,
            config,
            remote_tools: Arc::new(RwLock::new(None)),
        }
    }

    pub fn with_session_manager(mut self, session_manager: Arc<Mutex<SessionManager>>) -> Self {
        self.session_manager = Some(session_manager);
        self
    }

    /// Update the current session with tool call count
    pub fn update_session_with_tool_call_count(&self) -> Result<()> {
        if let Some(session_manager) = &self.session_manager {
            let mut session_mgr = session_manager.lock().unwrap();
            session_mgr.update_current_session_with_tool_call_count()?;
        }
        Ok(())
    }

    /// Record a successful tool call in the current session
    pub fn record_tool_call_success(&self, tool_name: &str) -> Result<()> {
        if let Some(session_manager) = &self.session_manager {
            let mut session_mgr = session_manager.lock().unwrap();
            session_mgr.record_tool_call_success(tool_name)?;
        }
        Ok(())
    }

    /// Record a failed tool call in the current session
    pub fn record_tool_call_failure(&self, tool_name: &str) -> Result<()> {
        if let Some(session_manager) = &self.session_manager {
            let mut session_mgr = session_manager.lock().unwrap();
            session_mgr.record_tool_call_failure(tool_name)?;
        }
        Ok(())
    }

    /// Update the current session with lines edited count
    pub fn update_session_with_lines_edited(&self, lines_edited: u64) -> Result<()> {
        if let Some(session_manager) = &self.session_manager {
            // Clone the store outside the mutable borrow scope
            let store = {
                let session_mgr = session_manager.lock().unwrap();
                session_mgr.store.clone()
            };

            // Update the session with lines edited
            {
                let mut session_mgr = session_manager.lock().unwrap();
                if let Some(ref mut session) = session_mgr.current_session {
                    session.increment_lines_edited(lines_edited);
                }
            }

            // Save the session
            if let Some(session_manager) = &self.session_manager {
                let session_mgr = session_manager.lock().unwrap();
                if let Some(ref session) = session_mgr.current_session {
                    store.save(session)?;
                }
            }
        }
        Ok(())
    }

    /// Update the current session with a changed file path
    pub fn update_session_with_changed_file(&self, path: std::path::PathBuf) -> Result<()> {
        if let Some(session_manager) = &self.session_manager {
            let mut session_mgr = session_manager.lock().unwrap();
            session_mgr.update_current_session_with_changed_file(path)?;
        }
        Ok(())
    }

    /// Get current session data
    pub fn get_current_session(&self) -> Option<SessionData> {
        if let Some(session_manager) = &self.session_manager {
            let session_mgr = session_manager.lock().unwrap();
            session_mgr.current_session.clone()
        } else {
            None
        }
    }

    /// Get session info string
    pub fn get_session_info(&self) -> Option<String> {
        if let Some(session_manager) = &self.session_manager {
            let session_mgr = session_manager.lock().unwrap();
            (*session_mgr).current_session_info()
        } else {
            None
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
                    let client = Arc::new(AsyncMutex::new(client));

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

        self.update_session_with_tool_call_count()?;

        let arguments: Option<JsonMap<String, JsonValue>> = match args {
            JsonValue::Null => None,
            JsonValue::Object(map) => Some(map.clone()),
            other => {
                self.record_tool_call_failure(alias)?;
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
                self.record_tool_call_success(alias)?;
                let value = serde_json::to_value(&call_result)?;
                Ok(Some(json!({
                    "ok": true,
                    "server": tool.server_name,
                    "tool": tool.remote_name,
                    "result": value
                })))
            }
            Err(e) => {
                self.record_tool_call_failure(alias)?;
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

    pub fn fs_list(
        &self,
        path: &str,
        max_depth: Option<usize>,
        pattern: Option<&str>,
    ) -> Result<Vec<String>> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        match list::fs_list(path, max_depth, pattern, &self.config) {
            Ok(result) => {
                self.record_tool_call_success("fs_list")?;
                Ok(result)
            }
            Err(e) => {
                self.record_tool_call_failure("fs_list")?;
                Err(e)
            }
        }
    }

    pub fn fs_read(
        &self,
        path: &str,
        start_line: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        match read::fs_read(path, start_line, limit, &self.config) {
            Ok(result) => {
                self.record_tool_call_success("fs_read")?;
                Ok(result)
            }
            Err(e) => {
                self.record_tool_call_failure("fs_read")?;
                Err(e)
            }
        }
    }

    pub fn fs_read_many_files(
        &self,
        paths: Vec<String>,
        exclude: Option<Vec<String>>,
        recursive: Option<bool>,
    ) -> Result<String> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        match read_many::fs_read_many_files(paths, exclude, recursive, &self.config) {
            Ok(result) => {
                self.record_tool_call_success("fs_read_many_files")?;
                Ok(result)
            }
            Err(e) => {
                self.record_tool_call_failure("fs_read_many_files")?;
                Err(e)
            }
        }
    }

    pub fn search_text(
        &self,
        search_pattern: &str,
        file_glob: Option<&str>,
    ) -> Result<Vec<(PathBuf, usize, String)>> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        match search_text::search_text(search_pattern, file_glob, &self.config) {
            Ok(result) => {
                self.record_tool_call_success("search_text")?;
                Ok(result)
            }
            Err(e) => {
                self.record_tool_call_failure("search_text")?;
                Err(e)
            }
        }
    }

    pub fn fs_write(&self, path: &str, content: &str) -> Result<()> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        match write::fs_write(path, content, &self.config) {
            Ok(result) => {
                self.record_tool_call_success("fs_write")?;
                Ok(result)
            }
            Err(e) => {
                self.record_tool_call_failure("fs_write")?;
                Err(e)
            }
        }
    }

    pub async fn execute_bash(&self, command: &str) -> Result<String> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        // Check if the command is allowed
        if !self.is_command_allowed(command) {
            tracing::warn!("Command '{}' is not allowed", command);
            self.record_tool_call_failure("execute_bash")?;
            // Return a structured result indicating the command is not allowed
            let result = execute::ExecuteBashResult {
                stdout: String::new(),
                stderr: format!("Command '{}' is not allowed", command),
                exit_code: None,
                success: false,
            };
            return Ok(serde_json::to_string(&result)?);
        }

        match execute::execute_bash(command, &self.config).await {
            Ok(result) => {
                self.record_tool_call_success("execute_bash")?;
                Ok(result)
            }
            Err(e) => {
                self.record_tool_call_failure("execute_bash")?;
                // Return a structured result with the error details
                let result = execute::ExecuteBashResult {
                    stdout: String::new(),
                    stderr: e.to_string(),
                    exit_code: None,
                    success: false,
                };
                Ok(serde_json::to_string(&result)?)
            }
        }
    }

    /// Finds files in the project based on a filename or pattern.
    ///
    /// This method allows the LLM agent to search for files within the project
    /// directory. It supports searching by full filename, partial name, or glob
    /// patterns.
    ///
    /// # Arguments
    ///
    /// * `filename` - The filename or pattern to search for.
    ///
    /// # Returns
    ///
    /// A `Result` containing:
    /// - `Ok(find_file::FindFileResult)`: A struct with a list of matching file paths.
    /// - `Err(anyhow::Error)`: An error if the search could not be completed.
    ///
    /// # Examples
    ///
    /// To find a file by its exact name:
    /// ```ignore
    /// let result = fs_tools.find_file("main.rs").await?;
    /// ```
    ///
    /// To find files matching a glob pattern:
    /// ```ignore
    /// let result = fs_tools.find_file("*.rs").await?;
    /// ```
    ///
    /// To find files with a partial name match:
    /// ```ignore
    /// let result = fs_tools.find_file("main").await?;
    /// ```
    pub async fn find_file(&self, filename: &str) -> Result<find_file::FindFileResult> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        match find_file::find_file(
            find_file::FindFileArgs {
                filename: filename.to_string(),
            },
            &self.config,
        )
        .await
        {
            Ok(result) => {
                self.record_tool_call_success("find_file")?;
                Ok(result)
            }
            Err(e) => {
                self.record_tool_call_failure("find_file")?;
                Err(e)
            }
        }
    }

    pub async fn search_repomap(
        &self,
        args: search_repomap::SearchRepomapArgs,
    ) -> Result<search_repomap::SearchRepomapResponse> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        // Use a more robust approach to handle potential RwLock poisoning
        let repomap_guard = self.repomap.read().await;
        let result = match if let Some(map) = &*repomap_guard {
            self.search_repomap_tools.search_repomap(map, args)
        } else {
            Err(anyhow::anyhow!("repomap is still generating"))
        } {
            Ok(search_result) => Ok(search_result),
            Err(e) => {
                self.record_tool_call_failure("search_repomap")?;
                return Err(e);
            }
        };

        match result {
            Ok(search_result) => {
                self.record_tool_call_success("search_repomap")?;
                Ok(search_result)
            }
            Err(e) => {
                self.record_tool_call_failure("search_repomap")?;
                Err(e)
            }
        }
    }

    pub fn todo_write(&self, todos: Vec<todo_write::TodoItem>) -> Result<todo_write::TodoList> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        // Get the current session ID
        let session_id = self
            .get_current_session()
            .map(|session| session.meta.id)
            .ok_or_else(|| anyhow::anyhow!("No current session"))?;

        match todo_write::todo_write(todos, &session_id, &self.config) {
            Ok(res) => {
                self.record_tool_call_success("todo_write")?;
                Ok(res)
            }
            Err(e) => {
                self.record_tool_call_failure("todo_write")?;
                Err(e)
            }
        }
    }

    pub fn todo_read(&self) -> Result<todo_read::TodoList> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        // Get the current session ID
        let session_id = self
            .get_current_session()
            .map(|session| session.meta.id)
            .ok_or_else(|| anyhow::anyhow!("No current session"))?;

        match todo_read::todo_read_from_base_path(&session_id, ".", &self.config) {
            Ok(todo_list) => {
                self.record_tool_call_success("todo_read")?;
                Ok(todo_list)
            }
            Err(e) => {
                self.record_tool_call_failure("todo_read")?;
                Err(e)
            }
        }
    }

    /// Check if a command is allowed based on the allowed_commands list
    pub fn is_command_allowed(&self, command: &str) -> bool {
        // If no allowed commands are specified, allow all commands (backward compatibility)
        if self.config.allowed_commands.is_empty() {
            return true;
        }

        // Check if the command matches any of the allowed commands (prefix match)
        self.config.allowed_commands.iter().any(|allowed| {
            // Exact match or prefix match (with space or end of string)
            command == allowed || command.starts_with(&format!("{} ", allowed))
        })
    }
}

#[derive(Debug, Default, Clone)]
struct RemoteToolRegistry {
    tools: Vec<RemoteToolInfo>,
    lookup: HashMap<String, RemoteToolInfo>,
}

#[derive(Clone)]
pub struct RemoteToolInfo {
    pub alias: String,
    pub remote_name: String,
    pub server_name: String,
    pub description: Option<String>,
    pub parameters: JsonValue,
    pub strict: Option<bool>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use anyhow::Result;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::RwLock;

    #[test]
    fn test_sanitize_identifier_basic() {
        assert_eq!(sanitize_identifier("Server-1"), "server-1");
        assert_eq!(sanitize_identifier("Server Name"), "server_name");
    }

    #[test]
    fn test_generate_unique_alias() {
        let mut counts = HashMap::new();
        assert_eq!(generate_unique_alias("alias".into(), &mut counts), "alias");
        assert_eq!(
            generate_unique_alias("alias".into(), &mut counts),
            "alias_2"
        );
        assert_eq!(
            generate_unique_alias("alias".into(), &mut counts),
            "alias_3"
        );
    }

    #[tokio::test]
    async fn test_execute_bash_with_permissions_allowed() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with allowed commands
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec!["echo".to_string(), "ls".to_string()],
            ..Default::default()
        };

        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        // This should succeed because "echo" is in the allowed list
        let result = fs_tools.execute_bash("echo 'hello world'").await;
        assert!(result.is_ok());

        Ok(())
    }

    #[tokio::test]
    async fn test_execute_bash_with_permissions_not_allowed() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with allowed commands
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec!["echo".to_string(), "ls".to_string()],
            ..Default::default()
        };

        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        // This should return a JSON string with success = false because "rm" is not in the allowed list
        let result_str = fs_tools.execute_bash("rm -rf /").await.unwrap();
        let result: execute::ExecuteBashResult = serde_json::from_str(&result_str).unwrap();
        assert!(!result.success);
        assert!(result.stderr.contains("not allowed"));

        Ok(())
    }

    #[tokio::test]
    async fn test_execute_bash_with_permissions_no_config() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config without allowed commands
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec![], // Empty list means all commands are allowed
            ..Default::default()
        };

        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        // This should be allowed because the allowed_commands list is empty
        let result = fs_tools.execute_bash("echo 'hello world'").await;
        assert!(result.is_ok());

        Ok(())
    }

    #[tokio::test]
    async fn test_is_command_allowed_exact_match() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with allowed commands
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec!["cargo".to_string(), "ls".to_string()],
            ..Default::default()
        };

        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        // Exact match should be allowed
        assert!(fs_tools.is_command_allowed("cargo"));

        Ok(())
    }

    #[tokio::test]
    async fn test_is_command_allowed_prefix_match() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with allowed commands
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec!["cargo".to_string(), "ls".to_string()],
            ..Default::default()
        };

        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        // Prefix match should be allowed
        assert!(fs_tools.is_command_allowed("cargo build"));

        Ok(())
    }

    #[tokio::test]
    async fn test_is_command_allowed_not_allowed() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with allowed commands
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec!["cargo".to_string(), "ls".to_string()],
            ..Default::default()
        };

        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        // Command not in the allowed list should not be allowed
        assert!(!fs_tools.is_command_allowed("rm"));

        Ok(())
    }

    // Additional tests for edge cases in allowed_commands functionality
    #[tokio::test]
    async fn test_is_command_allowed_partial_match_edge_case() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with allowed commands
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec!["cargo".to_string(), "ls".to_string()],
            ..Default::default()
        };

        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        // "carg" should not match "cargo" (partial match without space should not be allowed)
        assert!(!fs_tools.is_command_allowed("carg"));

        // "cargox" should not match "cargo" (extra characters without space should not be allowed)
        assert!(!fs_tools.is_command_allowed("cargox"));

        Ok(())
    }

    #[tokio::test]
    async fn test_is_command_allowed_space_separation() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with allowed commands
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec!["git".to_string(), "ls".to_string()],
            ..Default::default()
        };

        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        // Valid commands with proper space separation should be allowed
        assert!(fs_tools.is_command_allowed("git status"));
        assert!(fs_tools.is_command_allowed("ls -la"));

        // Commands with no space after should not be allowed
        assert!(!fs_tools.is_command_allowed("gitstatus"));

        Ok(())
    }

    #[tokio::test]
    async fn test_is_command_allowed_complex_commands() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with complex allowed commands
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec!["cargo build".to_string(), "git status".to_string()],
            ..Default::default()
        };

        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        // Commands matching the specific allowed commands should be allowed
        assert!(fs_tools.is_command_allowed("cargo build"));
        assert!(fs_tools.is_command_allowed("git status"));

        // Different commands should not be allowed
        assert!(!fs_tools.is_command_allowed("cargo test"));
        assert!(!fs_tools.is_command_allowed("git commit"));

        Ok(())
    }

    #[tokio::test]
    async fn test_execute_bash_complex_allowed_command() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with a complex allowed command
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec!["echo 'hello world'".to_string()],
            ..Default::default()
        };

        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        // This should succeed because the exact command is allowed
        let result = fs_tools.execute_bash("echo 'hello world'").await;
        assert!(result.is_ok());

        Ok(())
    }

    #[tokio::test]
    async fn test_execute_bash_with_empty_allowed_commands() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let project_root = temp_dir.path().to_path_buf();

        // Create a config with no allowed commands (should allow all)
        let cfg = AppConfig {
            project_root: project_root.clone(),
            allowed_commands: vec![],
            ..Default::default()
        };

        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));

        // All commands should be allowed when allowed_commands list is empty
        let result = fs_tools.execute_bash("echo 'test'").await;
        assert!(result.is_ok());

        let result = fs_tools.execute_bash("ls -la").await;
        assert!(result.is_ok());

        Ok(())
    }
}
