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
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Daemon { command } => commands::daemon::run(command, cli.json).await,
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
