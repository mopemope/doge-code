use crate::analysis::{Analyzer, RepoMap};
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
        })
    }

    pub fn new_with_repomap(
        cfg: crate::config::AppConfig,
        repomap: Arc<RwLock<Option<RepoMap>>>,
    ) -> Result<Self> {
        info!("Initializing TuiExecutor with existing repomap");
        let tools = FsTools::new(repomap.clone(), Arc::new(cfg.clone()));

        // Only initialize repomap if not disabled and it's not already initialized
        if !cfg.no_repomap {
            let repomap_clone = repomap.clone();
            let project_root = cfg.project_root.clone();

            // Spawn a task to check if repomap is initialized and generate if needed
            tokio::spawn(async move {
                // Check if repomap is already initialized
                let is_initialized = {
                    let repomap_guard = repomap_clone.read().await;
                    repomap_guard.is_some()
                };

                if !is_initialized {
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
                } else {
                    info!("Repomap already initialized, skipping generation");
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
        })
    }
}
