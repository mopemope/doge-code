//! `dgc-next`: command-line client for the dgc orchestrator daemon.
//!
//! Renamed to `dgc` when the legacy runtime is removed (milestone M3).

use clap::Parser;

#[derive(Parser)]
#[command(name = "dgc-next", version, about = "dgc orchestrator client")]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
