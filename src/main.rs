pub mod analysis;
pub mod config;
pub mod llm;
pub mod logging;
pub mod session;
pub mod tools;
mod tui;

use anyhow::Result;
use clap::Parser;
use dotenvy::dotenv;
use tracing::info;

use crate::config::AppConfig;
use crate::tui::{TuiApp, TuiExecutor};

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

    /// Log level (error,warn,info,debug,trace)
    #[arg(long, default_value = "info")]
    pub log_level: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    let cli = Cli::parse();
    logging::init_logging(&cli.log_level)?;

    let cfg = AppConfig::from_cli(cli)?;
    info!(?cfg, "app config");

    run_tui(cfg).await
}

async fn run_tui(cfg: AppConfig) -> Result<()> {
    let exec = TuiExecutor::new(cfg.clone())?;
    let mut app = TuiApp::new(
        "🦮 doge-code - /help, Esc or /quit to exit",
        Some(cfg.model.clone()),
    )
    .with_handler(Box::new(exec));
    app.push_log("Welcome to doge-code TUI");
    app.push_log("Type plain prompts (no leading slash) or commands like /clear, /quit");
    app.run()?;
    Ok(())
}
