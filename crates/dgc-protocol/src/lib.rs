//! Socket protocol shared by the dgc orchestrator daemon and its clients.
//!
//! This crate holds types only and performs no I/O. See
//! `docs/superpowers/specs/2026-11-25-orchestrator-design.md` §7.

/// Major version of the daemon socket protocol. Clients and daemon must agree.
pub const PROTOCOL_VERSION: u32 = 1;
