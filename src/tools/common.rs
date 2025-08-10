use anyhow::Result;
use std::path::PathBuf;

use crate::tools::execute;
use crate::tools::find_file;
use crate::tools::list;
use crate::tools::read;
use crate::tools::search_text;
use crate::tools::write;

#[derive(Debug, Clone)]
pub struct FsTools {
    pub root: PathBuf,
}

impl FsTools {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn fs_list(
        &self,
        path: &str,
        max_depth: Option<usize>,
        pattern: Option<&str>,
    ) -> Result<Vec<String>> {
        list::fs_list(path, max_depth, pattern)
    }

    pub fn fs_read(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String> {
        read::fs_read(path, offset, limit)
    }

    pub fn search_text(
        &self,
        search_pattern: &str,
        file_glob: Option<&str>,
    ) -> Result<Vec<(PathBuf, usize, String)>> {
        search_text::search_text(search_pattern, file_glob)
    }

    pub fn fs_write(&self, path: &str, content: &str) -> Result<()> {
        write::fs_write(path, content)
    }

    pub async fn execute_bash(&self, command: &str) -> Result<String> {
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
        find_file::find_file(find_file::FindFileArgs {
            filename: filename.to_string(),
        })
        .await
    }
}
