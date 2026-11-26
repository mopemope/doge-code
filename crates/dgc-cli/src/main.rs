//! `dgc`: command-line client for the dgc orchestrator daemon.

mod commands;
mod output;
mod tui;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "dgc", version, about = "dgc orchestrator client")]
struct Cli {
    /// Print machine-readable JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    /// Without a subcommand the TUI opens.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Open the terminal UI (the default without a subcommand).
    Tui,
    /// Manage the background daemon.
    Daemon {
        #[command(subcommand)]
        command: commands::daemon::DaemonCommand,
    },
    /// Register and remove projects.
    Project {
        #[command(subcommand)]
        command: commands::project::ProjectCommand,
    },
    /// Create, list and archive workspaces.
    Workspace {
        #[command(subcommand)]
        command: commands::workspace::WorkspaceCommand,
    },
    #[command(flatten)]
    Agent(commands::agent::AgentCommand),
    /// List and answer agents' permission requests.
    Permit {
        #[command(subcommand)]
        command: commands::permit::PermitCommand,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        None | Some(Command::Tui) => run_tui(cli.json).await,
        Some(Command::Daemon { command }) => commands::daemon::run(command, cli.json).await,
        Some(Command::Project { command }) => commands::project::run(command, cli.json).await,
        Some(Command::Workspace { command }) => commands::workspace::run(command, cli.json).await,
        Some(Command::Agent(command)) => commands::agent::run(command, cli.json).await,
        Some(Command::Permit { command }) => commands::permit::run(command, cli.json).await,
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {}", output::escape_text(&err.to_string()));
            ExitCode::FAILURE
        }
    }
}

/// `dgc tui`: the terminal is restored before an error is printed.
async fn run_tui(json: bool) -> commands::CliResult<ExitCode> {
    if json {
        return Err("the TUI has no JSON mode".into());
    }
    let paths = commands::resolve_paths()?;
    let exe = std::env::current_exe()?;
    tui::run(paths, exe).await?;
    Ok(ExitCode::SUCCESS)
}
