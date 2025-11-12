use crate::analysis::{Analyzer, RepoMap};
use crate::hooks::{HookManager, repomap_update::RepomapUpdateHook};
use crate::llm::OpenAIClient;
use crate::session::SessionManager;
use crate::tools::FsTools;
use crate::tui::commands::core::TuiExecutor;
use crate::tui::commands::prompt::build_system_prompt;
use anyhow::Result;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;
use tracing::{error, info};

impl TuiExecutor {
    pub fn new(cfg: crate::config::AppConfig) -> Result<Self> {
        info!("Initializing TuiExecutor");
        let repomap: Arc<RwLock<Option<RepoMap>>> = Arc::new(RwLock::new(None));
        let tools = FsTools::new(repomap.clone(), Arc::new(cfg.clone()));

        // Only initialize repomap if not disabled
        if !cfg.no_repomap {
            let repomap_clone = repomap.clone();
            let project_root = cfg.project_root.clone();

            // Spawn an asynchronous task
            tokio::spawn(async move {
                match Analyzer::new(&project_root).await {
                    Ok(mut analyzer) => {
                        info!(
                            "Starting background repomap generation for project at {:?}",
                            project_root
                        );
                        let start_time = std::time::Instant::now();

                        match analyzer.build().await {
                            Ok(map) => {
                                let duration = start_time.elapsed();
                                let symbol_count = map.symbols.len();
                                *repomap_clone.write().await = Some(map);
                                tracing::debug!(
                                    "Background repomap generation completed in {:?} with {} symbols",
                                    duration,
                                    symbol_count
                                );
                            }
                            Err(e) => {
                                error!("Failed to build RepoMap: {:?}", e);
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to create Analyzer: {:?}", e);
                    }
                }
            });
        } else {
            info!("Repomap initialization skipped due to --no-repomap flag");
        }

        let client = match cfg.api_key.clone() {
            Some(key) => Some(OpenAIClient::new(cfg.base_url.clone(), key)?),
            None => None,
        };
        // Load system prompt
        let sys_prompt = build_system_prompt(&cfg);
        let mut history = crate::llm::ChatHistory::new(12_000, Some(sys_prompt));
        history.append_system_once();

        // Initialize session manager
        let session_manager = Arc::new(Mutex::new(SessionManager::new()?));

        // Create a default session if none exists
        {
            let mut session_mgr = session_manager.lock().unwrap();
            if session_mgr.current_session.is_none() {
                session_mgr.create_session(None)?;
            }
        }

        // Pass session manager to tools
        let tools = tools.with_session_manager(session_manager.clone());

        Ok(Self {
            cfg: cfg.clone(),
            tools,
            repomap,
            client,
            history,
            ui_tx: None, // This will be set by TuiApp later
            cancel_tx: None,
            last_user_prompt: None,
            conversation_history: Arc::new(Mutex::new(Vec::new())), // Initialize conversation history
            session_manager,

            custom_commands: crate::tui::commands::handlers::custom::load_custom_commands(
                &cfg.project_root,
            ),
            hook_manager: {
                let mut hook_manager = HookManager::default();
                hook_manager.add_hook(Box::new(RepomapUpdateHook::new()));
                hook_manager
            },
        })
    }

    pub fn new_with_repomap(
        cfg: crate::config::AppConfig,
        repomap: Arc<RwLock<Option<RepoMap>>>,
    ) -> Result<Self> {
        info!("Initializing TuiExecutor with existing repomap");
        let tools = FsTools::new(repomap.clone(), Arc::new(cfg.clone()));

        // Only initialize repomap if not disabled and it's not already initialized
        // In the new_with_repomap flow, we rely on the repomap being initialized elsewhere
        // (e.g., by the main thread), so we don't spawn our own initialization task here.
        if !cfg.no_repomap {
            info!("Repomap initialization deferred to external initialization in new_with_repomap");
        } else {
            info!("Repomap initialization skipped due to --no-repomap flag");
        }

        let client = match cfg.api_key.clone() {
            Some(key) => Some(OpenAIClient::new(cfg.base_url.clone(), key)?),
            None => None,
        };
        // Load system prompt
        let sys_prompt = build_system_prompt(&cfg);
        let mut history = crate::llm::ChatHistory::new(12_000, Some(sys_prompt));
        history.append_system_once();

        // Initialize session manager
        let session_manager = Arc::new(Mutex::new(SessionManager::new()?));

        // Create a default session if none exists
        {
            let mut session_mgr = session_manager.lock().unwrap();
            if session_mgr.current_session.is_none() {
                session_mgr.create_session(None)?;
            }
        }

        // Pass session manager to tools
        let tools = tools.with_session_manager(session_manager.clone());

        Ok(Self {
            cfg: cfg.clone(),
            tools,
            repomap,
            client,
            history,
            ui_tx: None, // This will be set by TuiApp later
            cancel_tx: None,
            last_user_prompt: None,
            conversation_history: Arc::new(Mutex::new(Vec::new())), // Initialize conversation history
            session_manager,

            custom_commands: crate::tui::commands::handlers::custom::load_custom_commands(
                &cfg.project_root,
            ),
            hook_manager: {
                let mut hook_manager = HookManager::default();
                hook_manager.add_hook(Box::new(RepomapUpdateHook::new()));
                hook_manager
            },
        })
    }
}
