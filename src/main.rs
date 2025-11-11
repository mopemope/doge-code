pub mod analysis;
pub mod assets;
pub mod config;
pub mod diff_review;
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

    /// Resume the latest session
    #[arg(short, long, default_value_t = false)]
    pub resume: bool,

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
    },

    /// Run MCP server for tool access
    #[command()]
    McpServer {
        /// MCP server address (e.g., 127.0.0.1:8000)
        address: Option<String>,
    },

    /// Rewrite a snippet of code using the LLM based on a user prompt
    #[command()]
    Rewrite {
        /// Prompt describing the desired transformation
        #[arg(long)]
        prompt: String,
        /// Path to a temporary file containing the snippet to rewrite
        #[arg(long, value_name = "FILE")]
        code_file: std::path::PathBuf,
        /// Optional original file path hint for additional context
        #[arg(long)]
        file_path: Option<String>,
        /// Output structured JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    let cli = Cli::parse();
    logging::init_logging()?;

    let cfg = AppConfig::from_cli(cli.clone())?;
    // info!(?cfg, "app config");

    // Initialize repomap
    let (repomap, status_rx) = if !cfg.no_repomap {
        let repomap = std::sync::Arc::new(tokio::sync::RwLock::new(None));
        let repomap_clone = repomap.clone();
        let project_root = cfg.project_root.clone();

        // Create a channel for sending status messages
        let (status_tx, status_rx) = std::sync::mpsc::channel::<String>();

        // Spawn an asynchronous task to build the repomap
        tokio::spawn(async move {
            match crate::analysis::Analyzer::new(&project_root).await {
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
                        if let Err(send_err) = status_tx.send("::status:repomap_error".to_string())
                        {
                            tracing::error!("Failed to send repomap error message: {:?}", send_err);
                        }
                    }
                },
                Err(e) => {
                    tracing::error!("Failed to create Analyzer: {:?}", e);
                    // Send an error message
                    if let Err(send_err) = status_tx.send("::status:repomap_error".to_string()) {
                        tracing::error!("Failed to send repomap error message: {:?}", send_err);
                    }
                }
            }
        });
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
        Some(Commands::Exec { instruction, json }) => run_exec(cfg, instruction, *json).await,
        Some(Commands::Rewrite {
            prompt,
            code_file,
            file_path,
            json,
        }) => run_rewrite(cfg, prompt, code_file, file_path.as_deref(), *json).await,
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

    // app.push_log("Welcome to doge-code TUI");
    // app.push_log("Initializing repomap...");

    let exec = match TuiExecutor::new_with_repomap(cfg.clone(), repomap) {
        Ok(exec) => {
            // If resume flag is set, load the latest session
            if cfg.resume {
                let mut session_manager = exec.session_manager.lock().unwrap();
                if let Err(e) = session_manager.load_latest_session() {
                    eprintln!("Failed to load latest session: {}", e);
                } else if session_manager.current_session.is_some() {
                    println!("Resumed latest session");
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
        let session_manager = executor.session_manager.lock().unwrap();
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
    let mut executor = crate::exec::Executor::new(cfg)?;
    executor.run(instruction, json).await
}

async fn run_rewrite(
    cfg: crate::config::AppConfig,
    prompt: &str,
    code_file: &std::path::Path,
    file_path: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    crate::exec::run_rewrite(cfg, prompt, code_file, file_path, json).await
}
