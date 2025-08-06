mod analysis;
mod llm;
mod tools;
mod tui;

use std::{
    io::{self, Write},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context, Result};
use clap::{ArgAction, Parser};
use dotenvy::dotenv;
use tokio::sync::Mutex;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::analysis::Analyzer;
use crate::llm::{ChatMessage, OpenAIClient};
use crate::tools::FsTools;
use crate::tui::TuiApp;

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
    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(io::stderr))
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

async fn run_tui(cfg: AppConfig) -> Result<()> {
    let mut app = TuiApp::new("doge-code - /help, Esc or /quit to exit");
    app.push_log("Welcome to doge-code TUI");
    app.push_log("Type commands like /ask, /read, /search, /map");

    // Wire streaming: on Enter with /ask, spawn a task to stream tokens and update UI
    let client = if let Some(key) = cfg.api_key.clone() {
        Some(OpenAIClient::new(cfg.base_url.clone(), key)?)
    } else {
        None
    };
    let model = cfg.model.clone();

    // Shared state for a simplistic callback style
    let app_ref = Arc::new(Mutex::new(app));
    // Small input loop inside TUI module; we hack a minimal broker here by replacing TuiApp::run with inline loop
    // Reuse the TUI drawing/event loop from TuiApp but intercept /ask commands via callback

    // Inline reimplementation using the existing TuiApp instance for simplicity
    let app_ref_clone = app_ref.clone();
    {
        use crossterm::event::{Event, KeyCode};
        use crossterm::{cursor, event, execute, terminal};
        use std::io::Write;
        let mut stdout = std::io::stdout();
        terminal::enable_raw_mode()?;
        execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide)?;
        let res: Result<()> = loop {
            {
                let app = app_ref_clone.lock().await;
                // draw
                let (w, h) = terminal::size()?;
                use crossterm::{
                    cursor,
                    terminal::{self, ClearType},
                };
                use std::io::Write as _;
                execute!(
                    stdout,
                    terminal::Clear(ClearType::All),
                    cursor::MoveTo(0, 0)
                )?;
                writeln!(stdout, "{}", app.title)?;
                writeln!(stdout, "{}", "─".repeat(w as usize))?;
                let max_log_rows = h.saturating_sub(3) as usize;
                let start = app.log.len().saturating_sub(max_log_rows);
                for line in &app.log[start..] {
                    let mut s = line.clone();
                    if s.len() > (w as usize) {
                        s.truncate(w as usize);
                    }
                    writeln!(stdout, "{s}")?;
                }
                execute!(stdout, cursor::MoveTo(0, h.saturating_sub(1)))?;
                write!(stdout, "> {}", app.input)?;
                stdout.flush()?;
            }

            if event::poll(std::time::Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(k) => match k.code {
                        KeyCode::Esc => {
                            break Ok(());
                        }
                        KeyCode::Enter => {
                            let mut to_send = String::new();
                            {
                                let mut app = app_ref_clone.lock().await;
                                let line = std::mem::take(&mut app.input);
                                if line.trim() == "/quit" {
                                    break Ok(());
                                }
                                if let Some(rest) = line.strip_prefix("/ask ") {
                                    to_send = rest.to_string();
                                    app.push_log(format!("> {rest}"));
                                    app.push_log(String::new()); // allocate output line
                                } else {
                                    app.push_log(format!("> {line}"));
                                }
                            }
                            if !to_send.is_empty() {
                                if let Some(ref client) = client {
                                    let app_ref2 = app_ref_clone.clone();
                                    let model2 = model.clone();
                                    let client2 = client.clone();
                                    tokio::spawn(async move {
                                        match client2
                                            .chat_stream(
                                                &model2,
                                                vec![ChatMessage {
                                                    role: "user".into(),
                                                    content: to_send,
                                                }],
                                            )
                                            .await
                                        {
                                            Ok(mut stream) => {
                                                use futures::StreamExt;
                                                while let Some(tok) = stream.next().await {
                                                    match tok {
                                                        Ok(s) => {
                                                            let mut app = app_ref2.lock().await;
                                                            app.append_stream_token(&s);
                                                        }
                                                        Err(e) => {
                                                            let mut app = app_ref2.lock().await;
                                                            app.push_log(format!(
                                                                "[stream error] {e}"
                                                            ));
                                                            break;
                                                        }
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                let mut app = app_ref2.lock().await;
                                                app.push_log(format!("[stream start error] {e}"));
                                            }
                                        }
                                    });
                                } else {
                                    let mut app = app_ref_clone.lock().await;
                                    app.push_log("OPENAI_API_KEY not set; cannot call LLM.");
                                }
                            }
                        }
                        KeyCode::Backspace => {
                            let mut app = app_ref_clone.lock().await;
                            app.input.pop();
                        }
                        KeyCode::Char(c) => {
                            let mut app = app_ref_clone.lock().await;
                            app.input.push(c);
                        }
                        _ => {}
                    },
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }
        };
        execute!(stdout, terminal::LeaveAlternateScreen, cursor::Show)?;
        terminal::disable_raw_mode()?;
        res?;
    }
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
