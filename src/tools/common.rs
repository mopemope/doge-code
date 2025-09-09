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

        list::fs_list(path, max_depth, pattern)
    }

    pub fn fs_read(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        read::fs_read(path, offset, limit)
    }

    pub fn fs_read_many_files(
        &self,
        paths: Vec<String>,
        exclude: Option<Vec<String>>,
        recursive: Option<bool>,
    ) -> Result<String> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        read_many::fs_read_many_files(paths, exclude, recursive)
    }

    pub fn search_text(
        &self,
        search_pattern: &str,
        file_glob: Option<&str>,
    ) -> Result<Vec<(PathBuf, usize, String)>> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        search_text::search_text(search_pattern, file_glob)
    }

    pub fn fs_write(&self, path: &str, content: &str) -> Result<()> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        write::fs_write(path, content)
    }

    pub async fn execute_bash(&self, command: &str) -> Result<String> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        execute::execute_bash(command).await
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

        find_file::find_file(find_file::FindFileArgs {
            filename: filename.to_string(),
        })
        .await
    }

    pub async fn search_repomap(
        &self,
        args: search_repomap::SearchRepomapArgs,
    ) -> Result<Vec<search_repomap::RepomapSearchResult>> {
        // Update session with tool call count
        self.update_session_with_tool_call_count()?;

        let repomap_guard = self.repomap.read().await;
        if let Some(map) = &*repomap_guard {
            self.search_repomap_tools.search_repomap(map, args)
        } else {
            Err(anyhow::anyhow!("repomap is still generating"))
        }
    }
}
