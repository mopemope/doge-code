//! Hook system for executing custom processing after each instruction
use crate::analysis::RepoMap;
use crate::config::AppConfig;
use crate::llm::types::ChatMessage;
use crate::tools::FsTools;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Trait definition for instruction hooks
#[async_trait::async_trait]
pub trait InstructionHook: Send + Sync {
    /// Name of the hook for identification
    fn name(&self) -> &str;

    /// Execute the hook after an instruction is processed
    /// - `messages`: The complete conversation history
    /// - `final_msg`: The final assistant message
    /// - `config`: Application configuration
    /// - `fs_tools`: File system tools for accessing files
    /// - `repomap`: Repository map for code analysis
    async fn execute(
        &self,
        messages: &[ChatMessage],
        final_msg: &ChatMessage,
        config: &AppConfig,
        fs_tools: &FsTools,
        repomap: &Arc<RwLock<Option<RepoMap>>>,
    ) -> Result<()>;
}

/// Collection of hooks to execute after each instruction
pub struct HookManager {
    hooks: Vec<Box<dyn InstructionHook>>,
}

impl HookManager {
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    pub fn add_hook(&mut self, hook: Box<dyn InstructionHook>) {
        self.hooks.push(hook);
    }

    pub fn add_hooks(&mut self, mut hooks: Vec<Box<dyn InstructionHook>>) {
        self.hooks.append(&mut hooks);
    }

    pub async fn execute_hooks(
        &self,
        messages: &[ChatMessage],
        final_msg: &ChatMessage,
        config: &AppConfig,
        fs_tools: &FsTools,
        repomap: &Arc<RwLock<Option<RepoMap>>>,
    ) -> Result<()> {
        for hook in &self.hooks {
            if let Err(e) = hook
                .execute(messages, final_msg, config, fs_tools, repomap)
                .await
            {
                tracing::error!("Error executing hook '{}': {}", hook.name(), e);
            }
        }
        Ok(())
    }
}

impl Default for HookManager {
    fn default() -> Self {
        Self::new()
    }
}

pub mod examples;
pub mod repomap_update;
