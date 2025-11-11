//! Example hook implementations for demonstration

use crate::analysis::RepoMap;
use crate::config::AppConfig;
use crate::hooks::InstructionHook;
use crate::llm::types::ChatMessage;
use crate::tools::FsTools;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Example hook that logs the completion of each instruction
#[derive(Clone)]
pub struct LoggingHook {
    name: String,
}

impl Default for LoggingHook {
    fn default() -> Self {
        Self::new()
    }
}

impl LoggingHook {
    pub fn new() -> Self {
        Self {
            name: "LoggingHook".to_string(),
        }
    }
}

#[async_trait::async_trait]
impl InstructionHook for LoggingHook {
    fn name(&self) -> &str {
        &self.name
    }

    async fn execute(
        &self,
        messages: &[ChatMessage],
        final_msg: &ChatMessage,
        _config: &AppConfig,
        _fs_tools: &FsTools,
        _repomap: &Arc<RwLock<Option<RepoMap>>>,
    ) -> Result<()> {
        println!(
            "[HOOK] Instruction completed with {} total messages",
            messages.len()
        );
        if let Some(content) = &final_msg.content {
            println!(
                "[HOOK] Final assistant response: {}",
                if content.len() > 100 {
                    format!("{}...", &content[..100])
                } else {
                    content.clone()
                }
            );
        }
        Ok(())
    }
}

/// Example hook that saves the conversation to a file
#[derive(Clone)]
pub struct SaveConversationHook {
    name: String,
    output_path: String,
}

impl SaveConversationHook {
    pub fn new(output_path: String) -> Self {
        Self {
            name: "SaveConversationHook".to_string(),
            output_path,
        }
    }
}

#[async_trait::async_trait]
impl InstructionHook for SaveConversationHook {
    fn name(&self) -> &str {
        &self.name
    }

    async fn execute(
        &self,
        messages: &[ChatMessage],
        final_msg: &ChatMessage,
        config: &AppConfig,
        _fs_tools: &FsTools,
        _repomap: &Arc<RwLock<Option<RepoMap>>>,
    ) -> Result<()> {
        let mut output_content = String::new();
        output_content.push_str(&format!("Conversation with {} messages\n", messages.len()));
        output_content.push_str(&format!("Project: {}\n", config.project_root.display()));
        output_content.push_str("=== MESSAGES ===\n");

        for (i, msg) in messages.iter().enumerate() {
            let role = &msg.role;
            let msg_content = msg.content.as_deref().unwrap_or("");
            output_content.push_str(&format!("{} {}: {}\n", i + 1, role, msg_content));
        }

        if let Some(final_content) = &final_msg.content {
            output_content.push_str("=== FINAL RESPONSE ===\n");
            output_content.push_str(final_content);
        }

        Ok(())
    }
}

/// Example hook that performs LLM-based analysis of the changes made
#[derive(Clone)]
pub struct AnalysisHook {
    name: String,
    model: String,
}

impl AnalysisHook {
    pub fn new(model: String) -> Self {
        Self {
            name: "AnalysisHook".to_string(),
            model,
        }
    }
}

#[async_trait::async_trait]
impl InstructionHook for AnalysisHook {
    fn name(&self) -> &str {
        &self.name
    }

    async fn execute(
        &self,
        _messages: &[ChatMessage],
        _final_msg: &ChatMessage,
        config: &AppConfig,
        _fs_tools: &FsTools,
        repomap: &Arc<RwLock<Option<RepoMap>>>,
    ) -> Result<()> {
        // Example: Check for git changes and analyze them
        let output = std::process::Command::new("git")
            .arg("diff")
            .current_dir(&config.project_root)
            .output()?;

        if !output.stdout.is_empty() {
            let diff = String::from_utf8_lossy(&output.stdout);
            println!(
                "[HOOK] Changes detected in the codebase ({} chars)",
                diff.len()
            );

            // Here you could call the LLM to analyze the changes
            // For now, just print a summary
            if diff.contains("fn ") || diff.contains("def ") || diff.contains("function") {
                println!("[HOOK] Function/Method changes detected");
            }
            if diff.contains("struct") || diff.contains("class") || diff.contains("interface") {
                println!("[HOOK] Type definition changes detected");
            }
        } else {
            println!("[HOOK] No git changes detected");
        }

        // You could also analyze the repomap for structural changes
        {
            let repomap_guard = repomap.read().await;
            if let Some(repomap) = repomap_guard.as_ref() {
                println!("[HOOK] Repomap contains {} symbols", repomap.symbols.len());
            }
        }

        Ok(())
    }
}
