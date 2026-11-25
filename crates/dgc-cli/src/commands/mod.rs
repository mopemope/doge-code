//! Subcommand implementations.

pub mod daemon;

use std::error::Error;

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
