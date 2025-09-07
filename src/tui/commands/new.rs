use crate::analysis::{Analyzer, RepoMap};
use crate::llm::OpenAIClient;
use crate::tools::FsTools;
use crate::tui::commands::core::TuiExecutor;
use crate::tui::commands::prompt::build_system_prompt;
use crate::tui::commands_sessions::SessionManager;
use anyhow::Result;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;
use tracing::{error, info};

impl TuiExecutor {
    pub fn new(cfg: crate::config::AppConfig) -> Result<Self> {
        info!("Initializing TuiExecutor");
        let repomap: Arc<RwLock<Option<RepoMap>>> = Arc::new(RwLock::new(None));
        let tools = FsTools::new(repomap.clone());

        // Only initialize repomap if not disabled
        if !cfg.no_repomap {
            let repomap_clone = repomap.clone();
            let project_root = cfg.project_root.clone();

            // Spawn an asynchronous task
            tokio::spawn(async move {
                info!(
                    "Starting background repomap generation for project at {:?}",
                    project_root
                );
                let start_time = std::time::Instant::now();
                let mut analyzer = match Analyzer::new(&project_root).await {
                    Ok(analyzer) => analyzer,
                    Err(e) => {
                        error!("Failed to create Analyzer: {:?}", e);
                        return;
                    }
                };

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

        // Initialize plan manager
        let plan_manager = Arc::new(Mutex::new(crate::planning::PlanManager::new(
            cfg.project_root.clone(),
        )?));

        // Initialize task analyzer
        let task_analyzer = if let Some(client) = &client {
            crate::planning::TaskAnalyzer::new().with_llm_decomposer(
                client.clone(),
                cfg.model.clone(),
                tools.clone(),
                repomap.clone(),
            )
        } else {
            crate::planning::TaskAnalyzer::new()
        };

        Ok(Self {
            cfg: cfg.clone(),
            tools,
            repomap,
            client,
            history,
            ui_tx: None,
            cancel_tx: None,
            last_user_prompt: None,
            conversation_history: Arc::new(Mutex::new(Vec::new())), // Initialize conversation history
            session_manager,

            task_analyzer,
            plan_manager,
            custom_commands: crate::tui::commands::handlers::custom::load_custom_commands(
                &cfg.project_root,
            ),
        })
    }
}
