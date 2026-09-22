//! Structured command execution core.
//!
//! The LLM's normal path for builds, tests, and git is
//! `program + args -> structured policy -> direct process execution`.
//! Shells are an explicitly-enabled escape hatch, never the default.

pub mod lifecycle;
pub mod output;
pub mod policy;
pub mod process;

pub use lifecycle::{TERMINATE_GRACE_PERIOD, configure_process_group, terminate_process_tree};
pub use output::{BoundedCapture, budget_command_output};
pub use policy::{ExecutionPolicy, PolicyDenial, ProcessRequest, warn_if_dual_config};
pub use process::{ExecuteProcessParams, ProcessResult, ProcessStatus, run_process};
