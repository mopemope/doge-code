//! Repomap update hook for automatic repomap refresh after instructions

use crate::analysis::{Analyzer, RepoMap};
use crate::config::AppConfig;
use crate::hooks::InstructionHook;
use crate::llm::types::ChatMessage;
use crate::tools::FsTools;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::error;

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

    async fn rebuild_if_changed(
        &self,
        fs_tools: &FsTools,
        repomap: &Arc<RwLock<Option<RepoMap>>>,
        build: impl std::future::Future<Output = Result<RepoMap>> + Send,
    ) -> Result<()> {
        let Some(session_manager) = fs_tools
            .get_session_manager_wrapper()
            .get_session_manager()
            .as_ref()
        else {
            return Ok(());
        };
        let (session_id, pending) = {
            let mut manager = crate::utils::safe_std_lock(session_manager, "session_manager")?;
            let Some(session) = manager.current_session.as_mut() else {
                return Ok(());
            };
            (session.meta.id.clone(), session.repomap_dirty_snapshot())
        };
        if pending.is_empty() {
            return Ok(());
        }

        tracing::info!("RepomapUpdateHook: Files changed during session, rebuilding repomap");
        match build.await {
            Ok(new_map) => {
                let mut map = repomap.write().await;
                let mut manager = crate::utils::safe_std_lock(session_manager, "session_manager")?;
                if let Some(session) = manager.current_session.as_mut()
                    && session.meta.id == session_id
                    && session.acknowledge_repomap_changes(&pending)
                {
                    // Publish together with the acknowledgement. An older build
                    // must not overwrite a newer build that already consumed it.
                    *map = Some(new_map);
                    tracing::info!("Repomap successfully updated after instruction");
                }
            }
            Err(e) => error!("Failed to rebuild repomap: {:?}", e),
        }
        Ok(())
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
        self.rebuild_if_changed(fs_tools, repomap, async {
            let mut analyzer = Analyzer::new(&config.project_root).await?;
            analyzer.build().await
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionData, SessionManager, SessionStore};
    use std::sync::Mutex;

    struct Fixture {
        _dir: tempfile::TempDir,
        manager: Arc<Mutex<SessionManager>>,
        repomap: Arc<RwLock<Option<RepoMap>>>,
        fs_tools: FsTools,
    }

    fn fixture(paths: &[&str]) -> Result<Fixture> {
        let dir = tempfile::tempdir()?;
        let config = AppConfig {
            project_root: dir.path().to_path_buf(),
            ..Default::default()
        };
        let mut manager =
            SessionManager::with_store(SessionStore::new(dir.path().join("sessions"))?);
        manager.create_session(None)?;
        for path in paths {
            manager.update_current_session_with_changed_file(path.into())?;
        }
        let manager = Arc::new(Mutex::new(manager));
        let repomap = Arc::new(RwLock::new(None));
        let fs_tools =
            FsTools::new(repomap.clone(), Arc::new(config)).with_session_manager(manager.clone());
        Ok(Fixture {
            _dir: dir,
            manager,
            repomap,
            fs_tools,
        })
    }

    fn pending_paths(manager: &Arc<Mutex<SessionManager>>) -> Vec<String> {
        manager
            .lock()
            .unwrap()
            .current_session
            .as_mut()
            .unwrap()
            .repomap_dirty_snapshot()
            .into_keys()
            .collect()
    }

    #[tokio::test]
    async fn failed_rebuild_retains_changes_for_next_instruction() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let config = AppConfig {
            project_root: dir.path().to_path_buf(),
            ..Default::default()
        };
        let store = SessionStore::new(dir.path().join("sessions"))?;
        let mut manager = SessionManager::with_store(store);
        manager.create_session(None)?;
        manager.update_current_session_with_changed_file("src/lib.rs".into())?;
        let manager = Arc::new(Mutex::new(manager));
        let repomap = Arc::new(RwLock::new(None));
        let fs_tools = FsTools::new(repomap.clone(), Arc::new(config.clone()))
            .with_session_manager(manager.clone());
        // Prevent the real analyzer from creating its cache directory.
        std::fs::write(dir.path().join(".doge"), "cache creation blocked")?;
        let hook = RepomapUpdateHook::new();
        let final_msg: ChatMessage = serde_json::from_value(serde_json::json!({
            "role": "assistant", "content": "done"
        }))?;
        hook.execute(&[], &final_msg, &config, &fs_tools, &repomap)
            .await?;
        assert!(repomap.read().await.is_none());
        assert!(manager.lock().unwrap().current_session_has_changed_files());

        // No new edit: the next instruction must still retry the failed work.
        std::fs::remove_file(dir.path().join(".doge"))?;
        hook.execute(&[], &final_msg, &config, &fs_tools, &repomap)
            .await?;
        assert!(repomap.read().await.is_some());
        assert_eq!(
            manager
                .lock()
                .unwrap()
                .get_changed_files_from_current_session(),
            vec![std::path::PathBuf::from("src/lib.rs")],
            "successful analysis must preserve session change evidence"
        );
        Ok(())
    }

    #[tokio::test]
    async fn successful_build_acknowledges_only_unchanged_generations() -> Result<()> {
        let f = fixture(&["again.rs", "included.rs"])?;
        let hook = RepomapUpdateHook::new();
        hook.rebuild_if_changed(&f.fs_tools, &f.repomap, async {
            let mut manager = f.manager.lock().unwrap();
            // The same file changes again while analysis is in progress.
            manager.update_current_session_with_changed_file("again.rs".into())?;
            manager.update_current_session_with_changed_file("new.rs".into())?;
            Ok(RepoMap::default())
        })
        .await?;
        assert_eq!(pending_paths(&f.manager), ["again.rs", "new.rs"]);
        assert!(f.repomap.read().await.is_some());
        assert_eq!(
            f.manager
                .lock()
                .unwrap()
                .get_changed_files_from_current_session()
                .len(),
            3
        );

        hook.rebuild_if_changed(&f.fs_tools, &f.repomap, async { Ok(RepoMap::default()) })
            .await?;
        assert!(pending_paths(&f.manager).is_empty());
        let mut rebuilt = false;
        hook.rebuild_if_changed(&f.fs_tools, &f.repomap, async {
            rebuilt = true;
            Ok(RepoMap::default())
        })
        .await?;
        assert!(!rebuilt, "clean sessions must not rebuild");
        assert!(pending_paths(&f.manager).is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn successful_build_preserves_wire_evidence_and_resume_retries() -> Result<()> {
        let f = fixture(&["changed.rs"])?;
        let before =
            serde_json::to_value(f.manager.lock().unwrap().current_session.as_ref().unwrap())?;
        RepomapUpdateHook::new()
            .rebuild_if_changed(&f.fs_tools, &f.repomap, async { Ok(RepoMap::default()) })
            .await?;
        assert!(pending_paths(&f.manager).is_empty());
        let after =
            serde_json::to_value(f.manager.lock().unwrap().current_session.as_ref().unwrap())?;
        assert_eq!(
            before, after,
            "RepoMap acknowledgement must not alter session evidence"
        );
        let mut resumed: SessionData = serde_json::from_value(after)?;
        assert_eq!(
            resumed
                .repomap_dirty_snapshot()
                .into_keys()
                .collect::<Vec<_>>(),
            ["changed.rs"]
        );

        let mut manager = f.manager.lock().unwrap();
        let id = manager.get_current_session_id()?;
        manager.load_session(&id)?;
        drop(manager);
        assert_eq!(pending_paths(&f.manager), ["changed.rs"]);
        Ok(())
    }

    #[tokio::test]
    async fn completed_build_does_not_acknowledge_a_different_session() -> Result<()> {
        let f = fixture(&["same.rs"])?;
        // Exercise session switching with restored pending state as well.
        {
            let mut manager = f.manager.lock().unwrap();
            manager.current_session = Some(serde_json::from_value(serde_json::to_value(
                manager.current_session.as_ref().unwrap(),
            )?)?);
        }
        RepomapUpdateHook::new()
            .rebuild_if_changed(&f.fs_tools, &f.repomap, async {
                let mut manager = f.manager.lock().unwrap();
                manager.create_session(None)?;
                manager.update_current_session_with_changed_file("same.rs".into())?;
                manager.current_session = Some(serde_json::from_value(serde_json::to_value(
                    manager.current_session.as_ref().unwrap(),
                )?)?);
                Ok(RepoMap::default())
            })
            .await?;
        assert_eq!(pending_paths(&f.manager), ["same.rs"]);
        assert!(f.repomap.read().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn stale_build_does_not_acknowledge_changes_after_same_session_reload() -> Result<()> {
        let f = fixture(&["same.rs"])?;
        let id = {
            let mut manager = f.manager.lock().unwrap();
            let id = manager.get_current_session_id()?;
            manager.load_session(&id)?;
            id
        };
        RepomapUpdateHook::new()
            .rebuild_if_changed(&f.fs_tools, &f.repomap, async {
                let mut manager = f.manager.lock().unwrap();
                manager.update_current_session_with_changed_file("same.rs".into())?;
                manager.load_session(&id)?;
                Ok(RepoMap::default())
            })
            .await?;
        assert_eq!(pending_paths(&f.manager), ["same.rs"]);
        assert!(f.repomap.read().await.is_none());
        Ok(())
    }
}
