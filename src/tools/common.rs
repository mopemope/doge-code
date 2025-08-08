use anyhow::Result;
use std::path::PathBuf;

use crate::tools::execute;
use crate::tools::list;
use crate::tools::read;
use crate::tools::search;
use crate::tools::write;

#[derive(Debug, Clone)]
pub struct FsTools {
    pub(crate) root: PathBuf,
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
        list::fs_list(&self.root, path, max_depth, pattern)
    }

    pub fn fs_read(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String> {
        read::fs_read(&self.root, path, offset, limit)
    }

    pub fn fs_search(
        &self,
        search_pattern: &str,
        file_glob: Option<&str>,
    ) -> Result<Vec<(PathBuf, usize, String)>> {
        search::fs_search(&self.root, search_pattern, file_glob)
    }

    pub fn fs_write(&self, path: &str, content: &str) -> Result<()> {
        write::fs_write(&self.root, path, content)
    }

    pub async fn execute_bash(&self, command: &str) -> Result<String> {
        execute::execute_bash(&self.root, command).await
    }
}
