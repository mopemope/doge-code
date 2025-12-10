use crate::analysis::RepoMap;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Manages the context for the agent, keeping track of relevant files
/// and generating prompt snippets to keep the agent aware of the broader codebase.
#[derive(Debug, Clone)]
pub struct ContextManager {
    /// Recently accessed files (working set), most recent last.
    recent_files: VecDeque<PathBuf>,
    /// Limit on the number of files to track.
    capacity: usize,
    /// Reference to the global RepoMap.
    repomap: Arc<RwLock<Option<RepoMap>>>,
}

impl ContextManager {
    pub fn new(repomap: Arc<RwLock<Option<RepoMap>>>) -> Self {
        Self {
            recent_files: VecDeque::new(),
            capacity: 5, // Keep context focused on top 5 related files
            repomap,
        }
    }

    /// Register a file access (read or edit).
    pub fn add_file(&mut self, path: &Path) {
        let path_buf = path.to_path_buf();
        // Remove if already exists to move it to the back (most recent)
        if let Some(pos) = self.recent_files.iter().position(|p| p == &path_buf) {
            self.recent_files.remove(pos);
        }
        self.recent_files.push_back(path_buf);
        if self.recent_files.len() > self.capacity {
            self.recent_files.pop_front();
        }
    }

    /// Retrieve the current context prompt, which includes summaries of recently accessed files.
    pub async fn get_context_prompt(&self) -> String {
        if self.recent_files.is_empty() {
            return String::new();
        }

        let mut output = String::new();
        output.push_str("## Active Context (Recently Accessed Files)\n\n");

        let map_guard = self.repomap.read().await;
        let map = map_guard.as_ref();

        for path in self.recent_files.iter().rev() {
            output.push_str(&format!("### {}\n", path.display()));

            if let Some(map) = map {
                // Find symbols for this file
                let mut symbols: Vec<_> = map.symbols.iter().filter(|s| s.file == *path).collect();

                // Sort by line number
                symbols.sort_by_key(|s| s.start_line);

                if symbols.is_empty() {
                    output.push_str("(No significant symbols found)\n");
                } else {
                    // List top-level symbols or important ones
                    let top_level: Vec<_> = symbols
                        .iter()
                        .filter(|s| s.parent.is_none())
                        .take(10) // Limit to 10 top-level symbols per file
                        .collect();

                    for sym in top_level {
                        output.push_str(&format!("- {}: {}\n", sym.kind.as_str(), sym.name));
                    }
                    if symbols.len() > 10 {
                        output.push_str("... (and more)\n");
                    }
                }
            } else {
                output.push_str("(RepoMap not available)\n");
            }
            output.push('\n');
        }

        output
    }
}
