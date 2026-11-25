//! dgc orchestrator daemon: supervises Claude Code and Codex agents over a
//! private Unix socket. Implemented in milestone M1.

pub mod config;
pub mod lock;
pub mod paths;
pub mod store;
