//! dgc orchestrator daemon: supervises Claude Code and Codex agents over a
//! private Unix socket. Implemented in milestone M1.

pub mod agent;
pub mod config;
pub mod events_log;
pub mod handlers;
mod handoff;
pub mod lock;
pub mod paths;
pub mod process;
pub mod provider;
pub mod server;
pub mod store;
pub mod subscription;
pub mod time;
pub mod workspace;

pub use server::{ConnCtx, DaemonDispatcher, DaemonError, run_daemon};

/// Longest piece of untrusted input echoed back in an error message, in chars.
const ECHO_LIMIT: usize = 64;

fn clip_error_detail(text: &str) -> String {
    text.chars().take(ECHO_LIMIT).collect()
}
