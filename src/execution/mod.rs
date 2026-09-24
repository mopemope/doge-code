//! Structured command execution core.
//!
//! The LLM's normal path for builds, tests, and git is
//! `program + args -> structured policy -> direct process execution`.
//! Shells are an explicitly-enabled escape hatch, never the default.

pub mod lifecycle;
pub mod output;
pub mod policy;
pub mod process;
pub mod runner;

#[cfg(unix)]
pub use lifecycle::process_group_exists;
pub use lifecycle::{
    ProcessGroupHandle, TERMINATE_GRACE_PERIOD, cleanup_process_group_after_exit,
    configure_process_group, is_process_alive, terminate_process_tree,
    terminate_process_tree_with_group,
};
pub use output::{BoundedCapture, budget_command_output};
pub use policy::{ExecutionPolicy, PolicyDenial, ProcessRequest, warn_if_dual_config};
pub use process::{ExecuteProcessParams, ProcessResult, ProcessStatus, run_process};
pub use runner::{
    ManagedProcessError, ManagedProcessOutput, ManagedProcessSpec, ManagedProcessTermination,
    ManagedRunOptions, run_managed_process,
};
