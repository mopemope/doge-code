pub mod apply_patch;
pub mod budget;
mod common;
pub mod mutation;
pub mod requirements;
pub mod review;
pub mod scope;

#[derive(thiserror::Error, Debug)]
pub enum ToolError {
    #[error("Tool execution failed: {0}")]
    Execution(#[from] anyhow::Error),
    #[error("Invalid arguments: {0}")]
    InvalidArguments(String),
}
pub mod doc;
pub mod edit;
pub mod execute;
pub mod find_file;
pub mod observation;

pub mod list;
pub mod memory;
pub mod plan;
pub mod process;
pub mod provenance;
pub mod read;
pub mod read_many;
pub mod remote_tools;
pub mod search_repomap;
pub mod search_text;
pub mod security;
pub mod session_manager;
pub mod task;
pub mod tool_search;
pub mod undo;
pub mod workflow;
pub mod write;

pub mod shell;

pub use common::{FinalizeMutationOptions, FsTools, MutationFinalizeReport};

#[cfg(test)]
mod common_test;

#[cfg(test)]
mod test_utils;

#[cfg(test)]
mod read_pagination_tests;

#[cfg(test)]
mod mutation_target_tests;
