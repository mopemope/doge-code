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
pub mod execution;
pub mod features;
pub mod hooks;
pub mod jobs;
pub mod llm;
pub mod logging;
pub mod mcp;
pub mod provenance;
pub mod session;
pub mod tools;
mod tui;
pub mod utils;
pub mod watch;

use crate::config::AppConfig;
use crate::tui::commands::TuiExecutor;
use crate::tui::state::TuiApp;
use crate::watch::run_watch_mode;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dotenvy::dotenv;

#[derive(Parser, Clone)]
#[command(
    name = "doge-code",
    version,
    about = "Interactive AI coding agent (TUI)"
)]
pub struct Cli {
    /// Inference billing provider (default: existing API-key provider)
    #[arg(long, value_enum)]
    pub provider: Option<features::openai_subscription::ProviderKind>,
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
    /// Manage Sign in with ChatGPT credentials
    Auth {
        #[command(subcommand)]
        command: features::openai_subscription::cli::AuthCommand,
    },
    /// List account-specific ChatGPT models
    Models {
        #[arg(long, value_enum, default_value = "openai-chatgpt")]
        provider: features::openai_subscription::ProviderKind,
    },
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

    /// Manage sessions (list, show, delete, evidence)
    #[command()]
    Session {
        #[command(subcommand)]
        command: session::cli::SessionCommands,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // Evidence export uses current-directory storage without loading user
    // credentials, creating default config/debug.log, or starting services.
    if let Some(Commands::Session {
        command: command @ session::cli::SessionCommands::Evidence { .. },
    }) = &cli.command
    {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init()
            .ok();
        let cfg = AppConfig {
            project_root: std::env::current_dir()?,
            ..AppConfig::default()
        };
        return session::cli::run(cfg, command.clone()).await;
    }
    if let Some(Commands::Auth { command }) = &cli.command {
        let store = features::openai_subscription::credentials::CredentialStore::default_path()?;
        let cancel = tokio_util::sync::CancellationToken::new();
        let operation =
            features::openai_subscription::cli::run(command.clone(), store, cancel.clone());
        tokio::pin!(operation);
        return tokio::select! {
            result = &mut operation => result,
            _ = tokio::signal::ctrl_c() => { cancel.cancel(); operation.await }
        };
    }
    if let Some(Commands::Models { provider }) = &cli.command {
        anyhow::ensure!(
            *provider == features::openai_subscription::ProviderKind::OpenaiChatgpt,
            "models currently supports openai-chatgpt only"
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        let operation = features::openai_subscription::cli::models(
            features::openai_subscription::credentials::CredentialStore::default_path()?,
            cancel.clone(),
        );
        tokio::pin!(operation);
        return tokio::select! { result = &mut operation => result, _ = tokio::signal::ctrl_c() => { cancel.cancel(); operation.await } };
    }
    dotenv().ok();
    logging::init_logging()?;

    let cfg = AppConfig::from_cli(cli.clone())?;
    // info!(?cfg, "app config");

    // Handle `dgc session` early: no repomap, no MCP server, no LLM setup.
    if let Some(Commands::Session { command }) = &cli.command {
        return session::cli::run(cfg, command.clone()).await;
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

    // Local MCP listener lifecycle is owned per-command below. Remote
    // `[[mcp_servers]]` endpoints are never used to start a local listener.
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
        Some(Commands::Tui) | None => {
            // Background local MCP listener only when explicitly enabled via
            // `[mcp_server] enabled = true`. Short-lived automation commands
            // (exec/run/session/watch) never auto-start a network listener.
            // A bind failure must not take the TUI down: warn and continue
            // without the listener (fail-open for availability; the listener
            // is an auxiliary integration point, not the primary UI).
            let mcp_handle = if cfg.local_mcp_server.enabled {
                let addr = cfg.local_mcp_server.address.clone();
                match mcp::server::spawn_mcp_server(
                    &addr,
                    std::sync::Arc::new(cfg.clone()),
                    repomap.clone(),
                )
                .await
                {
                    Ok(handle) => Some(handle),
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            address = %addr,
                            "failed to start background MCP server; continuing without it"
                        );
                        None
                    }
                }
            } else {
                None
            };

            let result = run_tui(cfg, repomap, status_rx).await;

            if let Some(handle) = mcp_handle
                && let Err(e) = handle.shutdown().await
            {
                tracing::warn!(error = %e, "failed to shut down background MCP server");
            }

            result
        }
        Some(Commands::McpServer { address }) => {
            // Dedicated foreground service. The subcommand itself is an
            // explicit opt-in, so `[mcp_server] enabled = false` does not
            // block it. Precedence: CLI address -> config -> default.
            let addr = mcp::server::resolve_mcp_listen_address(
                address.as_deref(),
                &cfg.local_mcp_server.address,
            );
            let handle =
                mcp::server::spawn_mcp_server(&addr, std::sync::Arc::new(cfg), repomap.clone())
                    .await?;
            tracing::info!(address = %handle.local_addr(), "MCP server ready");
            tokio::signal::ctrl_c().await.context("wait for Ctrl-C")?;
            handle.shutdown().await?;
            Ok(())
        }
        Some(Commands::Run { workflow }) => features::workflow::run_workflow(cfg, workflow).await,
        // Handled early, before repomap/MCP initialization.
        Some(Commands::Auth { .. } | Commands::Models { .. } | Commands::Session { .. }) => {
            unreachable!("session subcommand handled earlier")
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
            // If resume is requested, hydrate both the SessionManager and the
            // runtime conversation from the same saved session.
            if let Some(resume_id) = cfg.resume.as_deref() {
                match exec.resume_session(resume_id) {
                    Ok(crate::tui::commands::session_state::ResumeOutcome::Resumed {
                        session_id,
                    }) => {
                        println!("Resumed session {}", session_id);
                    }
                    Ok(crate::tui::commands::session_state::ResumeOutcome::NoPreviousSession) => {
                        println!("No sessions to resume; starting a new session");
                    }
                    Err(e) => {
                        eprintln!("Failed to resume session '{}': {}", resume_id, e);
                        eprintln!("Hint: run `dgc session list` to see available sessions.");
                        std::process::exit(1);
                    }
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

    if let Some(account) = exec
        .client
        .as_ref()
        .and_then(|client| client.account_label())
    {
        app.inference_label = Some(format!("openai-chatgpt | {account} | {}", cfg.model));
        app.push_log(format!(
            "Provider: openai-chatgpt | Account: {account} | Model: {}",
            cfg.model
        ));
        app.push_log("ChatGPT plan usage: review limits in ChatGPT Settings > Usage. Token counts do not indicate remaining allowance.");
    }

    let mut exec = exec;
    exec.set_ui_tx(app.sender());
    exec.publish_plan_list();
    // Keep a clone for graceful shutdown after the UI exits. The manager
    // owns all foreground/background job tasks via TaskTracker.
    let jobs = exec.jobs.clone();
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
    let run_result = app.run();

    // Graceful shutdown: cancel all jobs, drain the tracker, and force-abort
    // only tasks still stuck past the grace period.
    jobs.shutdown(crate::jobs::JOB_SHUTDOWN_GRACE).await;

    // Final error-path flush happens only after all job checkpoints stopped.
    let flush_result = (|| -> Result<()> {
        if let Some(handler) = &app.handler
            && let Some(executor) = handler.as_any().downcast_ref::<TuiExecutor>()
        {
            let mut manager = utils::safe_std_lock(&executor.session_manager, "session_manager")?;
            match manager
                .flush_before_transition()
                .map_err(|error| manager.recover_capacity_exit(error))?
            {
                crate::session::store::SessionSaveOutcome::Durable => {}
                crate::session::store::SessionSaveOutcome::DurabilityUnconfirmed { message } => {
                    eprintln!("Warning: {message}")
                }
            }
            if let Some(message) = manager.checkpoint_warning() {
                eprintln!("Warning: {message}");
            }
        }
        Ok(())
    })();
    if let Err(error) = flush_result {
        return Err(error.context(format!(
            "final TUI checkpoint failed (UI result: {run_result:?})"
        )));
    }
    run_result?;

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
    let cancel = tokio_util::sync::CancellationToken::new();
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let signal = async {
        #[cfg(unix)]
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await
    };
    let result = {
        let run = executor.run_with_cancel(instruction, json, Some(cancel.clone()));
        tokio::pin!(run);
        tokio::select! {
            biased;
            signal_result = signal => {
                signal_result?;
                cancel.cancel();
                // Keep polling the agent so managed children are reaped and
                // canonical interruption history is saved before CLI exit.
                run.await
            }
            result = &mut run => result,
        }
    };
    executor
        .flush_session()
        .context("final exec checkpoint failed")?;
    result
}

impl std::fmt::Debug for Cli {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cli")
            .field("model", &self.model)
            .field("api_key", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}
