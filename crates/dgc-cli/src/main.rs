//! `dgc-next`: command-line client for the dgc orchestrator daemon.
//!
//! Renamed to `dgc` when the legacy runtime is removed (milestone M3).

mod commands;
mod output;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "dgc-next", version, about = "dgc orchestrator client")]
struct Cli {
    /// Print machine-readable JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
        Command::Daemon { command } => commands::daemon::run(command, cli.json).await,
        Command::Project { command } => commands::project::run(command, cli.json).await,
        Command::Workspace { command } => commands::workspace::run(command, cli.json).await,
        Command::Agent(command) => commands::agent::run(command, cli.json).await,
        Command::Permit { command } => commands::permit::run(command, cli.json).await,
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
