//! Subcommand implementations.

pub mod agent;
pub mod daemon;
pub mod handoff;
pub mod permit;
pub mod profile;
pub mod project;
pub mod schedule;
pub mod script;
pub mod workspace;

use std::error::Error;

use dgc_client::Client;
use dgc_daemon::paths::DaemonPaths;

pub type CliResult<T> = Result<T, Box<dyn Error>>;

/// Where the daemon lives: `DGC_STATE_ROOT` (tests and development) puts
/// everything under that directory, otherwise the XDG locations apply.
pub fn resolve_paths() -> CliResult<DaemonPaths> {
    match std::env::var_os("DGC_STATE_ROOT") {
        Some(root) if !root.is_empty() => Ok(DaemonPaths::under(std::path::Path::new(&root))),
        _ => Ok(DaemonPaths::from_env()?),
    }
}

/// Connect to the daemon at `paths`, starting it if it is not running.
pub async fn connect_or_spawn(paths: &DaemonPaths) -> CliResult<Client> {
    let exe = std::env::current_exe()?;
    Ok(Client::connect_or_spawn(paths, &exe).await?)
}

/// Connect to the daemon at the resolved paths, starting it if needed.
pub async fn connect() -> CliResult<Client> {
    connect_or_spawn(&resolve_paths()?).await
}
