mod analysis;
mod llm;
mod tools;
mod tui;

use std::{
    io::{self, Write},
    path::PathBuf,
};

use anyhow::{Context, Result};
use clap::{ArgAction, Parser};
use dotenvy::dotenv;
use tracing::info;
use tracing_subscriber::prelude::*;

use crate::analysis::Analyzer;
use crate::llm::{ChatMessage, OpenAIClient};
use crate::tools::FsTools;
use crate::tui::{CommandHandler, TuiApp};
use tokio::sync::watch;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "doge-code",
    version,
    about = "Interactive AI coding agent (CLI/TUI)"
)]
struct Cli {
    /// Use plain CLI mode (disable TUI)
    #[arg(long, action = ArgAction::SetTrue)]
    no_tui: bool,

    /// OpenAI-compatible API base URL
    #[arg(long, default_value = "https://api.openai.com/v1")]
    base_url: String,

    /// Model name
    #[arg(long, default_value = "gpt-4o-mini")]
    model: String,

    /// API key (set via env OPENAI_API_KEY recommended)
    #[arg(long)]
    api_key: Option<String>,

    /// Log level (error,warn,info,debug,trace)
    #[arg(long, default_value = "info")]
    log_level: String,
}

#[derive(Debug, Clone)]
struct AppConfig {
    no_tui: bool,
    base_url: String,
    model: String,
    api_key: Option<String>,
    #[allow(dead_code)]
    log_level: String,
    project_root: PathBuf,
}

impl AppConfig {
    fn from_cli(cli: Cli) -> Result<Self> {
        let project_root = std::env::current_dir().context("resolve current dir")?;
        let api_key = cli.api_key.or_else(|| std::env::var("OPENAI_API_KEY").ok());
        let base_url = if cli.base_url.is_empty() {
            std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string())
        } else {
            cli.base_url
        };
        let model = if cli.model.is_empty() {
            std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string())
        } else {
            cli.model
        };
        let log_level = if cli.log_level.is_empty() {
            std::env::var("DOGE_LOG").unwrap_or_else(|_| "info".to_string())
        } else {
            cli.log_level
        };
        Ok(Self {
            no_tui: cli.no_tui,
            base_url,
            model,
            api_key,
            log_level,
            project_root,
        })
    }
}

fn init_logging(level: &str) -> Result<()> {
    let log_file = std::sync::Arc::new(std::fs::File::create("./debug.log")?);
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_file(true)
        .with_line_number(true)
        .with_writer(log_file)
        .init();
    info!("logging initialized");
    Ok(())
}

fn print_help() {
    println!(
        "/help  Show help\n/map   Show repo map (Rust fn only)\n/tools Show tools (fs_search, fs_read, fs_write)\n/clear Clear screen\n/quit  Quit\n/ask <text>  Send a single prompt to the LLM\n/read <path> [offset limit]\n/write <path> <text>\n/search <regex> [include_glob]"
    );
}

fn handle_command(line: &str) -> Option<bool> {
    match line.trim() {
        "/help" => {
            print_help();
            None
        }
        "/clear" => {
            print!("\x1B[2J\x1B[H");
            let _ = io::stdout().flush();
            None
        }
        "/quit" | "/exit" => Some(true),
        "/tools" => {
            println!("Available tools: fs_search, fs_read, fs_write");
            None
        }
        _ => None,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    let cli = Cli::parse();
    init_logging(&cli.log_level)?;

    let cfg = AppConfig::from_cli(cli)?;
    info!(?cfg, "app config");

    if cfg.no_tui {
        run_cli_loop(cfg).await
    } else {
        run_tui(cfg).await
    }
}

struct TuiExecutor {
    cfg: AppConfig,
    tools: FsTools,
    analyzer: Analyzer,
    client: Option<OpenAIClient>,
    ui_tx: Option<std::sync::mpsc::Sender<String>>, // to send results back to UI
    cancel_tx: Option<watch::Sender<bool>>,         // broadcast cancel signal
}

impl TuiExecutor {
    fn new(cfg: AppConfig) -> Result<Self> {
        let tools = FsTools::new(&cfg.project_root);
        let analyzer = Analyzer::new(&cfg.project_root)?;
        let client = match cfg.api_key.clone() {
            Some(key) => Some(OpenAIClient::new(cfg.base_url.clone(), key)?),
            None => None,
        };
        Ok(Self {
            cfg,
            tools,
            analyzer,
            client,
            ui_tx: None,
            cancel_tx: None,
        })
    }
}

impl CommandHandler for TuiExecutor {
    fn handle(&mut self, line: &str, ui: &mut TuiApp) {
        if self.ui_tx.is_none() {
            self.ui_tx = ui.sender();
        }
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        match line {
            "/help" => {
                ui.push_log("/help, /map, /tools, /clear, /quit");
                ui.push_log("/ask <text>");
                ui.push_log("/read <path> [offset limit]");
                ui.push_log("/write <path> <text>");
                ui.push_log("/search <regex> [include_glob]");
            }
            "/tools" => ui.push_log("Available tools: fs_search, fs_read, fs_write"),
            "/clear" => {
                ui.log.clear();
            }
            "/cancel" => {
                if let Some(tx) = &self.cancel_tx {
                    let _ = tx.send(true);
                    ui.push_log("[Cancelled]");
                    if let Some(tx) = &self.ui_tx {
                        let _ = tx.send("::status:cancelled".into());
                    }
                    self.cancel_tx = None;
                } else {
                    ui.push_log("[no running task]");
                }
            }
            "/map" => match self.analyzer.build() {
                Ok(map) => {
                    ui.push_log(format!(
                        "RepoMap (Rust functions): {} symbols",
                        map.symbols.len()
                    ));
                    for s in map.symbols.iter().take(50) {
                        ui.push_log(format!("fn {}  @{}:{}", s.name, s.file.display(), s.line));
                    }
                }
                Err(e) => ui.push_log(format!("map error: {e}")),
            },
            _ => {
                if let Some(rest) = line.strip_prefix("/ask ") {
                    ui.push_log(format!("> {rest}"));
                    if let Some(tx) = &self.ui_tx {
                        let _ = tx.send("::status:streaming".into());
                    }
                    match self.client.as_ref() {
                        Some(c) => {
                            let rt = tokio::runtime::Handle::current();
                            let model = self.cfg.model.clone();
                            let content = rest.to_string();
                            let c = c.clone();
                            let tx = self.ui_tx.clone();
                            ui.push_log(String::new());
                            let (cancel_tx, cancel_rx) = watch::channel(false);
                            self.cancel_tx = Some(cancel_tx);
                            rt.spawn(async move {
                                // Try streaming; fall back to once if stream fails
                                let stream_res = c
                                    .chat_stream(
                                        &model,
                                        vec![ChatMessage {
                                            role: "user".into(),
                                            content: content.clone(),
                                        }],
                                    )
                                    .await;
                                match stream_res {
                                    Ok(mut s) => {
                                        use futures::StreamExt;
                                        while let Some(item) = s.next().await {
                                            if *cancel_rx.borrow() {
                                                if let Some(tx) = tx.as_ref() {
                                                    let _ = tx.send("\n[Cancelled]\n".into());
                                                }
                                                break;
                                            }
                                            let token = match item {
                                                Ok(t) => t,
                                                Err(e) => {
                                                    if let Some(tx) = tx.as_ref() {
                                                        let _ =
                                                            tx.send("[Error] stream error".into());
                                                        let _ = tx.send("::status:error".into());
                                                    }
                                                    String::new()
                                                }
                                            };
                                            if let Some(tx) = tx.as_ref() {
                                                let _ = tx.send(token);
                                            }
                                        }
                                        if let Some(tx) = tx {
                                            let _ = tx.send("\n[Done]\n".into());
                                            let _ = tx.send("::status:done".into());
                                        }
                                    }
                                    Err(_) => {
                                        if *cancel_rx.borrow() {
                                            if let Some(tx) = tx {
                                                let _ = tx.send("[Cancelled]".into());
                                            }
                                            return;
                                        }
                                        let res = c
                                            .chat_once(
                                                &model,
                                                vec![ChatMessage {
                                                    role: "user".into(),
                                                    content,
                                                }],
                                            )
                                            .await;
                                        let out = match res {
                                            Ok(m) => m.content,
                                            Err(e) => format!("LLM error: {e}"),
                                        };
                                        if let Some(tx) = tx {
                                            let _ = tx.send(out);
                                            let _ = tx.send("\n[Done]\n".into());
                                            let _ = tx.send("::status:done".into());
                                        }
                                    }
                                }
                            });
                        }
                        None => ui.push_log("OPENAI_API_KEY not set; cannot call LLM."),
                    }
                } else if let Some(rest) = line.strip_prefix("/read ") {
                    let mut parts = rest.split_whitespace();
                    let path = match parts.next() {
                        Some(p) => p,
                        None => {
                            ui.push_log("usage: /read <path> [offset limit]");
                            return;
                        }
                    };
                    let off = parts.next().and_then(|s| s.parse::<usize>().ok());
                    let lim = parts.next().and_then(|s| s.parse::<usize>().ok());
                    match self.tools.fs_read(path, off, lim) {
                        Ok(s) => ui.push_log(s),
                        Err(e) => ui.push_log(format!("read error: {e}")),
                    }
                } else if let Some(rest) = line.strip_prefix("/write ") {
                    let mut parts = rest.splitn(2, ' ');
                    let path = match parts.next() {
                        Some(p) => p,
                        None => {
                            ui.push_log("usage: /write <path> <text>");
                            return;
                        }
                    };
                    let text = match parts.next() {
                        Some(t) => t,
                        None => {
                            ui.push_log("usage: /write <path> <text>");
                            return;
                        }
                    };
                    match self.tools.fs_write(path, text) {
                        Ok(()) => ui.push_log(format!("wrote {} bytes", text.len())),
                        Err(e) => ui.push_log(format!("write error: {e}")),
                    }
                } else if let Some(rest) = line.strip_prefix("/search ") {
                    let mut parts = rest.split_whitespace();
                    let regex = match parts.next() {
                        Some(p) => p,
                        None => {
                            ui.push_log("usage: /search <regex> [include_glob]");
                            return;
                        }
                    };
                    let include = parts.next();
                    match self.tools.fs_search(regex, include) {
                        Ok(rows) => {
                            for (p, ln, text) in rows.into_iter().take(50) {
                                ui.push_log(format!("{}:{}: {}", p.display(), ln, text));
                            }
                        }
                        Err(e) => ui.push_log(format!("search error: {e}")),
                    }
                } else {
                    ui.push_log(format!("> {line}"));
                }
            }
        }
    }
}

async fn run_tui(cfg: AppConfig) -> Result<()> {
    let exec = TuiExecutor::new(cfg)?;
    let mut app =
        TuiApp::new("doge-code - /help, Esc or /quit to exit").with_handler(Box::new(exec));
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
