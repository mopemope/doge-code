use anyhow::Result;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

use crate::analysis::RepoMap;
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

#[derive(Debug, Clone)]
pub struct FsTools {
    search_repomap_tools: search_repomap::RepomapSearchTools,
    repomap: Arc<RwLock<Option<RepoMap>>>,
    pub session_manager: Option<Arc<Mutex<SessionManager>>>,
}

impl Default for FsTools {
    fn default() -> Self {
        Self::new(Arc::new(RwLock::new(None)))
    }
}

impl FsTools {
    pub fn new(repomap: Arc<RwLock<Option<RepoMap>>>) -> Self {
        Self {
            search_repomap_tools: search_repomap::RepomapSearchTools::new(),
            repomap,
            session_manager: None,
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
            session_mgr.current_session_info()
        } else {
            None
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

        match list::fs_list(path, max_depth, pattern) {
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
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        match read::fs_read(path, offset, limit) {
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

        match read_many::fs_read_many_files(paths, exclude, recursive) {
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

        match search_text::search_text(search_pattern, file_glob) {
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

        match write::fs_write(path, content) {
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

        match execute::execute_bash(command).await {
            Ok(result) => {
                self.record_tool_call_success("execute_bash")?;
                Ok(result)
            }
            Err(e) => {
                self.record_tool_call_failure("execute_bash")?;
                Err(e)
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

        match find_file::find_file(find_file::FindFileArgs {
            filename: filename.to_string(),
        })
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
    ) -> Result<Vec<search_repomap::RepomapSearchResult>> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        let repomap_guard = self.repomap.read().await;
        match if let Some(map) = &*repomap_guard {
            self.search_repomap_tools.search_repomap(map, args)
        } else {
            Err(anyhow::anyhow!("repomap is still generating"))
        } {
            Ok(result) => {
                self.record_tool_call_success("search_repomap")?;
                Ok(result)
            }
            Err(e) => {
                self.record_tool_call_failure("search_repomap")?;
                Err(e)
            }
        }
    }

    pub fn todo_write(&self, todos: Vec<todo_write::TodoItem>) -> Result<()> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        // Get the current session ID
        let session_id = self
            .get_current_session()
            .map(|session| session.meta.id)
            .ok_or_else(|| anyhow::anyhow!("No current session"))?;

        match todo_write::todo_write(todos, &session_id) {
            Ok(result) => {
                self.record_tool_call_success("todo_write")?;
                Ok(result)
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

        match todo_read::todo_read_from_base_path(&session_id, ".") {
            Ok(result) => {
                self.record_tool_call_success("todo_read")?;
                Ok(result)
            }
            Err(e) => {
                self.record_tool_call_failure("todo_read")?;
                Err(e)
            }
        }
    }
}
