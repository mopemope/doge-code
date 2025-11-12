//! Repomap update hook for automatic repomap refresh after instructions

use crate::analysis::{Analyzer, RepoMap};
use crate::config::AppConfig;
use crate::hooks::InstructionHook;
use crate::llm::types::ChatMessage;
use crate::tools::FsTools;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, warn};

/// Hook that automatically updates the repomap after each instruction if files were changed
#[derive(Clone)]
pub struct RepomapUpdateHook;

impl Default for RepomapUpdateHook {
    fn default() -> Self {
        Self::new()
    }
}

impl RepomapUpdateHook {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl InstructionHook for RepomapUpdateHook {
    fn name(&self) -> &str {
        "RepomapUpdateHook"
    }

    async fn execute(
        &self,
        _messages: &[ChatMessage],
        _final_msg: &ChatMessage,
        config: &AppConfig,
        fs_tools: &FsTools,
        repomap: &Arc<RwLock<Option<RepoMap>>>,
    ) -> Result<()> {
        // Check if there are changed files in the current session that would require a repomap rebuild
        if let Some(ref session_manager) = fs_tools.session_manager {
            // Check if session has changed files without holding the lock across await
            let has_changed_files = {
                let sm = session_manager.lock().unwrap();
                sm.current_session_has_changed_files()
            };

            if has_changed_files {
                tracing::info!(
                    "RepomapUpdateHook: Files changed during session, rebuilding repomap"
                );

                // Clear changed files from session without holding the lock across await
                {
                    let mut sm = session_manager.lock().unwrap();
                    if let Err(e) = sm.clear_changed_files_from_current_session() {
                        tracing::error!(?e, "Failed to clear changed files from session");
                    }
                }

                // Trigger repomap rebuild
                let repomap_clone = repomap.clone();
                let project_root = config.project_root.clone();

                // Create analyzer and rebuild repomap
                match Analyzer::new(&project_root).await {
                    Ok(mut analyzer) => {
                        if let Err(e) = analyzer.clear_cache().await {
                            warn!("Failed to clear cache before repomap rebuild: {}", e);
                        }

                        match analyzer.build_parallel().await {
                            Ok(new_map) => {
                                let mut repomap_guard = repomap_clone.write().await;
                                *repomap_guard = Some(new_map);
                                tracing::info!("Repomap successfully updated after instruction");
                            }
                            Err(e) => {
                                error!("Failed to rebuild repomap: {:?}", e);
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to create Analyzer for repomap rebuild: {:?}", e);
                    }
                }
            }
        }

        Ok(())
    }
}
