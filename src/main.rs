//! Doge-Code CLI Application
//!
//! This module provides the main entry point for the Doge-Code application,
//! handling command-line arguments, configuration, and routing to appropriate
//! subcommands.

pub mod a2a;
pub mod analysis;
pub mod assets;
pub mod config;
pub mod error;

pub mod diff_review;
pub mod error_recovery;
pub mod exec;
pub mod features;
pub mod hooks;
pub mod llm;
pub mod logging;
pub mod mcp;
pub mod session;
pub mod tools;
mod tui;
pub mod utils;
pub mod watch;

use crate::config::AppConfig;
use crate::tui::commands::TuiExecutor;
use crate::tui::state::TuiApp;
use crate::watch::run_watch_mode;
use anyhow::Result;
use clap::{Parser, Subcommand};
use dotenvy::dotenv;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "doge-code",
    version,
    about = "Interactive AI coding agent (TUI)"
)]
pub struct Cli {
    /// OpenAI-compatible API base URL (no default; falls back to env OPENAI_BASE_URL or config file)
    #[arg(long, default_value = "")]
    pub base_url: String,

    /// Model name (no default; falls back to env OPENAI_MODEL or config file)
    #[arg(long, default_value = "")]
    pub model: String,

    /// API key (set via env OPENAI_API_KEY recommended)
    #[arg(long)]
    pub api_key: Option<String>,

    /// Disable repomap creation at startup
    #[arg(long, default_value_t = false)]
    pub no_repomap: bool,

    /// Path to the project instructions file
    #[arg(short, long)]
    pub instructions_file: Option<String>,

    /// Resume a session: `--resume` or `--resume=latest` for the most recently
    /// updated session, `--resume=<id>` (prefix allowed) for a specific session
    #[arg(
        short,
        long,
        value_name = "SESSION_ID",
        require_equals = true,
        num_args = 0..=1,
        default_missing_value = "latest"
    )
    ]
    pub resume: Option<String>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    /// Run in TUI mode (default if no subcommand is provided)
    #[command()]
    Tui,

    /// Watch for file changes and execute tasks
    #[command()]
    Watch,

    /// Execute a command from arguments and exit
    #[command()]
    Exec {
        /// The instruction to execute
        instruction: String,
        /// Output in JSON format for machine parsing
        #[arg(long, default_value_t = false)]
        json: bool,
        /// Resume a session: `--resume` or `--resume=latest` for the most
        /// recently updated session, `--resume=<id>` (prefix allowed) for a specific session
        #[arg(
            long,
            value_name = "SESSION_ID",
            require_equals = true,
            num_args = 0..=1,
            default_missing_value = "latest"
        )
        ]
        resume: Option<String>,
    },

    /// Run MCP server for tool access
    #[command()]
    McpServer {
        /// MCP server address (e.g., 127.0.0.1:8000)
        address: Option<String>,
    },

    /// Run a predefined workflow
    #[command()]
    Run {
        /// The name of the workflow to run (without extension)
        workflow: String,
    },

    /// Manage sessions (list, show, delete)
    #[command()]
    Session {
        #[command(subcommand)]
        command: session::cli::SessionCommands,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    let cli = Cli::parse();
    logging::init_logging()?;

    let cfg = AppConfig::from_cli(cli.clone())?;
    // info!(?cfg, "app config");

    // Handle `dgc session` early: no repomap, no MCP server, no LLM setup.
    if let Some(Commands::Session { command }) = &cli.command {
        return session::cli::run(cfg, command.clone());
    }

    // Initialize repomap
    let (repomap, status_rx) = if !cfg.no_repomap {
        let repomap = std::sync::Arc::new(tokio::sync::RwLock::new(None));
        let repomap_clone = repomap.clone();
        let project_root = cfg.project_root.clone();

        // Create a channel for sending status messages
        let (status_tx, status_rx) = std::sync::mpsc::channel::<String>();

        // Initialize persistent store and semantic service
        let store_result = crate::analysis::cache::RepomapStore::new(project_root.clone()).await;

        let store = match store_result {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::error!("Failed to initialize RepomapStore: {:?}", e);
                if let Err(send_err) = status_tx.send("::status:repomap_error".to_string()) {
                    tracing::error!("Failed to send repomap error message: {:?}", send_err);
                }
                None
            }
        };

        if let Some(store) = store {
            let root_clone = project_root.clone();

            // Spawn an asynchronous task to build the repomap
            tokio::spawn(async move {
                match crate::analysis::Analyzer::new_with_store(root_clone, store).await {
                    Ok(mut analyzer) => match analyzer.build().await {
                        Ok(map) => {
                            let start_time = std::time::Instant::now();
                            let symbol_count = map.symbols.len();
                            *repomap_clone.write().await = Some(map);
                            tracing::debug!(
                                "Background repomap generation completed in {:?} with {} symbols",
                                start_time.elapsed(),
                                symbol_count
                            );
                            // Send a message to notify that the repomap is ready
                            if let Err(e) = status_tx.send("::status:repomap_ready".to_string()) {
                                tracing::error!("Failed to send repomap ready message: {:?}", e);
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to build RepoMap: {:?}", e);
                            // Send an error message
                            if let Err(send_err) =
                                status_tx.send("::status:repomap_error".to_string())
                            {
                                tracing::error!(
                                    "Failed to send repomap error message: {:?}",
                                    send_err
                                );
                            }
                        }
                    },
                    Err(e) => {
                        tracing::error!("Failed to create Analyzer: {:?}", e);
                        // Send an error message
                        if let Err(send_err) = status_tx.send("::status:repomap_error".to_string())
                        {
                            tracing::error!("Failed to send repomap error message: {:?}", send_err);
                        }
                    }
                }
            });
        }
        (repomap, Some(status_rx))
    } else {
        (std::sync::Arc::new(tokio::sync::RwLock::new(None)), None)
    };

    // Start the MCP server if enabled
    let _mcp_server_handle = if let Some(mcp_server) = cfg.mcp_servers.first() {
        mcp::server::start_mcp_server(mcp_server, repomap.clone())
    } else {
        None
    };

    match &cli.command {
        Some(Commands::Watch) => run_watch_mode(cfg).await,
        Some(Commands::Exec {
            instruction,
            json,
            resume,
        }) => {
            let mut cfg = cfg;
            if resume.is_some() {
                cfg.resume = resume.clone();
            }
            run_exec(cfg, instruction, *json).await
        }
        Some(Commands::Tui) | None => run_tui(cfg, repomap, status_rx).await,
        Some(Commands::McpServer { address }) => {
            let addr = address
                .clone()
                .unwrap_or_else(|| "127.0.0.1:8000".to_string());
            let config = crate::config::McpServerConfig {
                name: "doge-mcp".to_string(),
                enabled: true,
                address: addr,
                transport: "http".to_string(),
            };
            mcp::server::start_mcp_server(&config, repomap.clone());
            Ok(())
        }
        Some(Commands::Run { workflow }) => features::workflow::run_workflow(cfg, workflow).await,
        // Handled early, before repomap/MCP initialization.
        Some(Commands::Session { .. }) => unreachable!("session subcommand handled earlier"),
    }
}

async fn run_tui(
    cfg: AppConfig,
    repomap: std::sync::Arc<tokio::sync::RwLock<Option<crate::analysis::RepoMap>>>,
    status_rx: Option<std::sync::mpsc::Receiver<String>>,
) -> Result<()> {
    let mut app = TuiApp::new(
        "🦮 doge-code 🐕‍🦺 /help",
        Some(cfg.model.clone()),
        &cfg.theme, // pass theme name
    )?;
    // Set auto-compact threshold in the UI from configuration
    app.auto_compact_prompt_token_threshold =
        cfg.auto_compact_prompt_token_threshold_for_current_model();

    // Initialize remaining context tokens
    let context_size = cfg.get_context_window_size();
    app.update_remaining_context_tokens(context_size);

    // Set configuration in the UI app before creating and attaching executor so that
    // context size is available when processing token update messages
    app.cfg = Some(cfg.clone());

    // app.push_log("Welcome to doge-code TUI");
    // app.push_log("Initializing repomap...");

    let exec = match TuiExecutor::new_with_repomap(cfg.clone(), repomap) {
        Ok(exec) => {
            // If resume is requested, load the specified (or latest) session.
            if let Some(resume_id) = cfg.resume.as_deref() {
                let mut session_manager =
                    utils::safe_std_lock(&exec.session_manager, "session_manager")?;
                // The executor eagerly created an empty session for this run;
                // drop it once we successfully resume a different session.
                let fresh_id = session_manager.current_session_id();
                let result = match resume_id {
                    "latest" => {
                        let loaded =
                            session_manager.load_latest_session_excluding(fresh_id.as_deref())?;
                        if loaded {
                            println!(
                                "Resumed session {}",
                                session_manager.get_current_session_id()?
                            );
                            if let Some(fid) = fresh_id {
                                let _ = session_manager.delete_session(&fid);
                            }
                        } else {
                            println!("No sessions to resume; starting a new session");
                        }
                        Ok(())
                    }
                    id => session_manager.resolve_and_load_session(id).map(|session| {
                        println!("Resumed session {}", session.meta.id);
                        if Some(session.meta.id.as_str()) != fresh_id.as_deref()
                            && let Some(fid) = fresh_id
                        {
                            let _ = session_manager.delete_session(&fid);
                        }
                    }),
                };
                if let Err(e) = result {
                    eprintln!("Failed to resume session '{}': {}", resume_id, e);
                    eprintln!("Hint: run `dgc session list` to see available sessions.");
                    std::process::exit(1);
                }
            }
            //            app.push_log("Repomap initialization completed.");
            exec
        }
        Err(e) => {
            //            app.push_log(format!("Failed to initialize repomap: {:?}", e));
            return Err(e);
        }
    };

    let mut exec = exec;
    exec.set_ui_tx(app.sender());
    exec.publish_plan_list();
    // Show which project instructions file (if any) was used at startup
    if let Some(path) = crate::tui::commands::prompt::get_project_instructions_file_path(&exec.cfg)
    {
        app.push_log(format!("Project instructions file: {}", path.display()));
    } else {
        app.push_log("Project instructions file: (none)");
    }
    app = app.with_handler(Box::new(exec));

    // Handle repomap status messages if available
    if let Some(status_rx) = status_rx {
        // Spawn a thread to handle status messages
        let ui_tx = app.sender();
        std::thread::spawn(move || {
            while let Ok(status) = status_rx.recv() {
                if let Some(ref tx) = ui_tx
                    && let Err(e) = tx.send(status)
                {
                    tracing::error!("Failed to send status message to UI: {:?}", e);
                }
            }
        });
    }

    //    app.push_log("Type plain prompts (no leading slash) or commands like /clear, /quit");
    app.run()?;

    // Display session statistics on shutdown
    if let Some(handler) = &app.handler
        && let Some(executor) = handler.as_any().downcast_ref::<TuiExecutor>()
    {
        let session_manager = utils::safe_std_lock(&executor.session_manager, "session_manager")?;
        if let Some(stats) = session_manager.get_session_statistics() {
            println!("{}", stats);
        }
    }

    Ok(())
}

/// Runs the `exec` subcommand.
/// Initializes the executor and runs the provided instruction.
async fn run_exec(
    cfg: crate::config::AppConfig,
    instruction: &str,
    json: bool,
) -> anyhow::Result<()> {
    let mut executor = crate::exec::Executor::new(cfg).await?;
    executor.run(instruction, json).await
}
