use crate::analysis::{ContextManager, RepoMap};
use crate::config::AppConfig;
use crate::session::{SessionData, SessionManager};
use crate::tools::execute;
use crate::tools::find_file;
use crate::tools::list;
use crate::tools::plan;
use crate::tools::read;
use crate::tools::read_many;
use crate::tools::remote_tools::RemoteToolManager;
use crate::tools::search_repomap;
use crate::tools::search_text;
use crate::tools::security::SecurityChecker;
use crate::tools::session_manager::SessionManagerWrapper;
use crate::tools::shell::{self, SharedShellSession};
use crate::tools::write;
use anyhow::Result;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

use crate::tools::memory::MemoryTools;
// ... imports ...

#[derive(Debug, Clone)]
pub struct FsTools {
    search_repomap_tools: search_repomap::RepomapSearchTools,
    memory_tools: MemoryTools,
    repomap: Arc<RwLock<Option<RepoMap>>>,
    // ...
    session_manager_wrapper: SessionManagerWrapper,
    pub config: Arc<AppConfig>,
    remote_tool_manager: RemoteToolManager,
    security_checker: SecurityChecker,
    pub context_manager: Arc<RwLock<ContextManager>>,
    pub undo_stack: Arc<RwLock<crate::tools::undo::UndoStack>>,
    pub shell_session: SharedShellSession,
    pub semantic_service: Option<crate::analysis::semantic::SemanticService>,
}

impl Default for FsTools {
    fn default() -> Self {
        Self::new(Arc::new(RwLock::new(None)), Arc::new(AppConfig::default()))
    }
}

impl FsTools {
    pub fn new(repomap: Arc<RwLock<Option<RepoMap>>>, config: Arc<AppConfig>) -> Self {
        Self {
            search_repomap_tools: search_repomap::RepomapSearchTools::new(None),
            memory_tools: MemoryTools::new(config.clone()),
            context_manager: Arc::new(RwLock::new(ContextManager::new(repomap.clone()))),
            repomap,
            session_manager_wrapper: SessionManagerWrapper::new(None),
            config: config.clone(),
            remote_tool_manager: RemoteToolManager::new(config.clone()),
            security_checker: SecurityChecker::new(config.clone()),
            undo_stack: Arc::new(RwLock::new(crate::tools::undo::UndoStack::new())),
            shell_session: SharedShellSession::new(
                config.project_root.clone(),
                config.command_timeout_ms,
            ),
            semantic_service: None,
        }
    }

    pub fn with_session_manager(mut self, session_manager: Arc<Mutex<SessionManager>>) -> Self {
        self.session_manager_wrapper = SessionManagerWrapper::new(Some(session_manager));
        self
    }

    pub fn with_semantic_service(
        mut self,
        service: Option<crate::analysis::semantic::SemanticService>,
    ) -> Self {
        self.semantic_service = service.clone();
        self.search_repomap_tools = search_repomap::RepomapSearchTools::new(service);
        self
    }

    pub async fn search_history(&self, query: &str, limit: usize) -> Result<String> {
        self.update_session_with_tool_call_count()?;
        match crate::tools::history::search_history(&self.semantic_service, query, limit).await {
            Ok(result) => {
                self.record_tool_call_success("search_history")?;
                Ok(result)
            }
            Err(e) => {
                self.record_tool_call_failure("search_history")?;
                Err(e)
            }
        }
    }

    pub async fn log_action(
        &self,
        action_type: &str,
        content: &str,
        metadata: Option<serde_json::Value>,
    ) -> Result<()> {
        if let Some(semantic) = &self.semantic_service
            && let Some(session) = self.session_manager_wrapper.get_current_session()
        {
            semantic
                .log_action(
                    &session.meta.id,
                    action_type,
                    content,
                    metadata.unwrap_or(serde_json::json!({})),
                )
                .await?;
        }
        Ok(())
    }

    /// Update the current session with tool call count
    pub fn update_session_with_tool_call_count(&self) -> Result<()> {
        self.session_manager_wrapper
            .update_session_with_tool_call_count()
    }

    /// Record a successful tool call in the current session
    pub fn record_tool_call_success(&self, tool_name: &str) -> Result<()> {
        self.session_manager_wrapper
            .record_tool_call_success(tool_name)
    }

    /// Record a failed tool call in the current session
    pub fn record_tool_call_failure(&self, tool_name: &str) -> Result<()> {
        self.session_manager_wrapper
            .record_tool_call_failure(tool_name)
    }

    /// Update the current session with lines edited count
    pub fn update_session_with_lines_edited(&self, lines_edited: u64) -> Result<()> {
        self.session_manager_wrapper
            .update_session_with_lines_edited(lines_edited)
    }

    /// Update the current session with a changed file path
    pub fn update_session_with_changed_file(&self, path: std::path::PathBuf) -> Result<()> {
        self.session_manager_wrapper
            .update_session_with_changed_file(path)
    }

    /// Get current session data
    pub fn get_current_session(&self) -> Option<SessionData> {
        self.session_manager_wrapper.get_current_session()
    }

    /// Get session info string
    pub fn get_session_info(&self) -> Option<String> {
        self.session_manager_wrapper.get_session_info()
    }

    pub fn ensure_current_session_id(&self) -> Result<String> {
        if let Some(session) = self.get_current_session() {
            return Ok(session.meta.id);
        }

        let session_manager = self
            .session_manager_wrapper
            .get_session_manager()
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("No session manager"))?;

        let mut mgr = session_manager.lock().unwrap();
        if mgr.current_session.is_none() {
            mgr.create_session(None)?;
        }

        mgr.current_session
            .as_ref()
            .map(|s| s.meta.id.clone())
            .ok_or_else(|| anyhow::anyhow!("No current session"))
    }

    /// Get reference to remote tool manager
    pub fn get_remote_tool_manager(&self) -> &RemoteToolManager {
        &self.remote_tool_manager
    }

    /// Get reference to session manager wrapper
    pub fn get_session_manager_wrapper(&self) -> &SessionManagerWrapper {
        &self.session_manager_wrapper
    }

    pub fn is_command_allowed(&self, command: &str) -> bool {
        self.security_checker.is_command_allowed(command)
    }

    /// Update context with accessed file
    pub fn update_context(&self, path: std::path::PathBuf) {
        let cm = self.context_manager.clone();
        tokio::spawn(async move {
            cm.write().await.add_file(&path);
        });
    }

    /// Backup file content to undo stack
    pub async fn backup_file(&self, path: &std::path::Path) -> Result<()> {
        if path.exists() {
            let content = tokio::fs::read_to_string(path).await.unwrap_or_default();
            self.undo_stack
                .write()
                .await
                .push(path.to_path_buf(), content);
        } else {
            // If file doesn't exist, we push an empty content entry for it,
            // or handle "creation" undo separately. For now, empty string implies "was empty/new".
            // But actually, if it didn't exist, we might want to delete it on undo.
            // Currently undo() just writes content. Writing empty string is close enough for text files.
            self.undo_stack
                .write()
                .await
                .push(path.to_path_buf(), String::new());
        }
        Ok(())
    }

    pub fn fs_list(
        &self,
        path: &str,
        max_depth: Option<usize>,
        pattern: Option<&str>,
        options: list::FsListOptions,
    ) -> Result<list::FsListResponse> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        match list::fs_list(path, max_depth, pattern, &self.config, options) {
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

    pub fn fs_read(&self, path: &str, opts: read::FsReadOptions) -> Result<read::FsReadResult> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        match read::fs_read(path, opts, &self.config) {
            Ok(result) => {
                self.record_tool_call_success("fs_read")?;
                self.update_context(PathBuf::from(path));
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
        options: read_many::FsReadManyOptions,
    ) -> Result<read_many::FsReadManyResponse> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        match read_many::fs_read_many_files(paths, exclude, recursive, &self.config, options) {
            Ok(result) => {
                self.record_tool_call_success("fs_read_many_files")?;
                for file in &result.files {
                    self.update_context(PathBuf::from(&file.path));
                }
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

    pub async fn fs_write(&self, path: &str, content: &str) -> Result<()> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        // Backup existing file before overwriting
        if let Err(e) = self.backup_file(std::path::Path::new(path)).await {
            tracing::warn!("Failed to backup file {}: {}", path, e);
        }

        match write::fs_write(path, content, &self.config) {
            Ok(result) => {
                self.record_tool_call_success("fs_write")?;
                self.update_context(PathBuf::from(path));
                let _ = self
                    .log_action(
                        "fs_write",
                        &format!("Modified file: {}", path),
                        Some(serde_json::json!({
                            "path": path,
                            "content_snippet": content.chars().take(200).collect::<String>(),
                        })),
                    )
                    .await;
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
                let _ = self
                    .log_action(
                        "execute_bash",
                        command,
                        Some(serde_json::json!({
                            "command": command,
                            "result_snippet": result.chars().take(200).collect::<String>(),
                        })),
                    )
                    .await;
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

    pub async fn execute_shell(&self, command: &str) -> Result<String> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        // Check if the command is allowed
        if !self.is_command_allowed(command) {
            tracing::warn!("Command '{}' is not allowed", command);
            self.record_tool_call_failure("execute_shell")?;
            let result = shell::ExecuteShellResult {
                stdout: String::new(),
                stderr: format!("Command '{}' is not allowed", command),
                exit_code: None,
                success: false,
            };
            return Ok(serde_json::to_string(&result)?);
        }

        match self.shell_session.exec(command).await {
            Ok(result) => {
                self.record_tool_call_success("execute_shell")?;
                Ok(result)
            }
            Err(e) => {
                self.record_tool_call_failure("execute_shell")?;
                let result = shell::ExecuteShellResult {
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
            self.search_repomap_tools
                .search_repomap(map, args, &self.config.project_root)
                .await
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

    pub fn plan_write(
        &self,
        items: Vec<plan::PlanItem>,
        mode: plan::PlanWriteMode,
    ) -> Result<plan::PlanList> {
        let session_id = self.ensure_current_session_id()?;
        plan::plan_write(items, mode, &session_id, &self.config)
    }

    pub fn plan_read(&self) -> Result<plan::PlanList> {
        let session_id = self.ensure_current_session_id()?;
        plan::plan_read(&session_id, &self.config)
    }

    pub async fn call_remote_tool(
        &self,
        alias: &str,
        args: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>> {
        // Update session with tool call count before making the call
        self.update_session_with_tool_call_count()?;

        match self.remote_tool_manager.call_remote_tool(alias, args).await {
            Ok(Some(result)) => {
                self.record_tool_call_success(alias)?;
                Ok(Some(result))
            }
            Ok(None) => Ok(None), // No tool found with this alias
            Err(e) => {
                self.record_tool_call_failure(alias)?;
                Err(e)
            }
        }
    }
    pub async fn read_memory(&self, key: &str) -> Result<String> {
        self.update_session_with_tool_call_count()?;
        match self.memory_tools.read_memory(key).await {
            Ok(content) => {
                self.record_tool_call_success("read_memory")?;
                Ok(content)
            }
            Err(e) => {
                self.record_tool_call_failure("read_memory")?;
                Err(e)
            }
        }
    }

    pub async fn write_memory(&self, key: &str, content: &str) -> Result<String> {
        self.update_session_with_tool_call_count()?;
        match self.memory_tools.write_memory(key, content).await {
            Ok(msg) => {
                self.record_tool_call_success("write_memory")?;
                Ok(msg)
            }
            Err(e) => {
                self.record_tool_call_failure("write_memory")?;
                Err(e)
            }
        }
    }

    pub async fn list_memories(&self) -> Result<String> {
        self.update_session_with_tool_call_count()?;
        match self.memory_tools.list_memories().await {
            Ok(msg) => {
                self.record_tool_call_success("list_memories")?;
                Ok(msg)
            }
            Err(e) => {
                self.record_tool_call_failure("list_memories")?;
                Err(e)
            }
        }
    }
    pub async fn doc_generate(&self, path: &str, symbol: Option<&str>) -> Result<String> {
        self.update_session_with_tool_call_count()?;
        match crate::tools::doc::doc_generate(path, symbol, &self.config, self.repomap.clone())
            .await
        {
            Ok(msg) => {
                self.record_tool_call_success("doc_generate")?;
                Ok(msg)
            }
            Err(e) => {
                self.record_tool_call_failure("doc_generate")?;
                Err(e)
            }
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

        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
        let session_manager = Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            store,
            current_session: None,
        }));
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg))
            .with_session_manager(session_manager);

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

        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
        let session_manager = Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            store,
            current_session: None,
        }));
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg))
            .with_session_manager(session_manager);

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

        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
        let session_manager = Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            store,
            current_session: None,
        }));
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg))
            .with_session_manager(session_manager);

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

        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
        let session_manager = Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            store,
            current_session: None,
        }));
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg))
            .with_session_manager(session_manager);

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

        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
        let session_manager = Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            store,
            current_session: None,
        }));
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg))
            .with_session_manager(session_manager);

        // All commands should be allowed when allowed_commands list is empty
        let result = fs_tools.execute_bash("echo 'test'").await;
        assert!(result.is_ok());

        let result = fs_tools.execute_bash("ls -la").await;
        assert!(result.is_ok());

        Ok(())
    }
}
