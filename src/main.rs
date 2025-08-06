mod analysis;
pub mod cli;
pub mod config;
mod llm;
pub mod logging;
mod tools;
mod tui;

use std::io::{self};

use anyhow::Result;
use clap::{ArgAction, Parser};
use dotenvy::dotenv;
use tracing::info;

use crate::analysis::Analyzer;
use crate::cli::handle_command;
use crate::config::AppConfig;
use crate::llm::{ChatMessage, OpenAIClient};
use crate::tools::FsTools;
use crate::tui::{TuiApp, TuiExecutor};

#[derive(Parser, Debug, Clone)]
#[command(
    name = "doge-code",
    version,
    about = "Interactive AI coding agent (CLI/TUI)"
)]
pub struct Cli {
    /// Use plain CLI mode (disable TUI)
    #[arg(long, action = ArgAction::SetTrue)]
    pub no_tui: bool,

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

    if cfg.no_tui {
        run_cli_loop(cfg).await
    } else {
        run_tui(cfg).await
    }
}

async fn run_tui(cfg: AppConfig) -> Result<()> {
    let exec = TuiExecutor::new(cfg.clone())?;
    let mut app = TuiApp::new(
        "doge-code - /help, Esc or /quit to exit",
        Some(cfg.model.clone()),
    )
    .with_handler(Box::new(exec));
    app.push_log("Welcome to doge-code TUI");
    app.push_log("Type commands like /ask, /read, /search, /map");
    app.run()?;
    Ok(())
}

async fn run_cli_loop(cfg: AppConfig) -> Result<()> {
    use std::io::{BufRead, BufReader};

    println!("doge-code (CLI) - type /help for commands");
    let stdin = io::stdin();
    let reader = BufReader::new(stdin).lines();

    let tools = FsTools::new(&cfg.project_root);
    let mut analyzer = Analyzer::new(&cfg.project_root)?;

    let client = match cfg.api_key.clone() {
        Some(key) => Some(OpenAIClient::new(cfg.base_url.clone(), key)?),
        None => None,
    };

    for line in reader {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(quit) = handle_command(&line) {
            if quit {
                break;
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("/ask ") {
            if let Some(ref client) = client {
                let reply = client
                    .chat_once(
                        &cfg.model,
                        vec![ChatMessage {
                            role: "user".into(),
                            content: rest.to_string(),
                        }],
                    )
                    .await;
                match reply {
                    Ok(msg) => println!("{}", msg.content),
                    Err(e) => eprintln!("LLM error: {e}"),
                }
            } else {
                eprintln!("OPENAI_API_KEY not set; cannot call LLM.");
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("/read ") {
            let mut parts = rest.split_whitespace();
            let path = match parts.next() {
                Some(p) => p,
                None => {
                    eprintln!("usage: /read <path> [offset limit]");
                    continue;
                }
            };
            let off = parts.next().and_then(|s| s.parse::<usize>().ok());
            let lim = parts.next().and_then(|s| s.parse::<usize>().ok());
            match tools.fs_read(path, off, lim) {
                Ok(s) => println!("{s}"),
                Err(e) => eprintln!("read error: {e}"),
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("/write ") {
            let mut parts = rest.splitn(2, ' ');
            let path = match parts.next() {
                Some(p) => p,
                None => {
                    eprintln!("usage: /write <path> <text>");
                    continue;
                }
            };
            let text = match parts.next() {
                Some(t) => t,
                None => {
                    eprintln!("usage: /write <path> <text>");
                    continue;
                }
            };
            match tools.fs_write(path, text) {
                Ok(()) => println!("wrote {} bytes", text.len()),
                Err(e) => eprintln!("write error: {e}"),
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("/search ") {
            let mut parts = rest.split_whitespace();
            let regex = match parts.next() {
                Some(p) => p,
                None => {
                    eprintln!("usage: /search <regex> [include_glob]");
                    continue;
                }
            };
            let include = parts.next();
            match tools.fs_search(regex, include) {
                Ok(rows) => {
                    for (p, ln, text) in rows.into_iter().take(50) {
                        println!("{}:{}: {}", p.display(), ln, text);
                    }
                }
                Err(e) => eprintln!("search error: {e}"),
            }
            continue;
        }
        if line.trim() == "/map" {
            match analyzer.build() {
                Ok(map) => {
                    println!("RepoMap (Rust functions): {} symbols", map.symbols.len());
                    for s in map.symbols.iter().take(50) {
                        println!("fn {}  @{}:{}", s.name, s.file.display(), s.line);
                    }
                }
                Err(e) => eprintln!("map error: {e}"),
            }
            continue;
        }

        println!("You said: {line}");
    }

    Ok(())
}
