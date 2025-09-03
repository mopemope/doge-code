//! Module for the `exec` subcommand.
//! This module provides functionality to execute a single instruction
//! provided via command-line arguments, interact with the LLM, use tools,
//! and output the final result to stdout.

use crate::analysis::{Analyzer, RepoMap};
use crate::config::AppConfig;
use crate::llm::{self, OpenAIClient};
use crate::tools::FsTools;
use anyhow::Result;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;
use tracing::{error, info};

/// Executor for the `exec` subcommand.
/// This struct holds the necessary components to interact with the LLM and tools.
pub struct Executor {
    cfg: AppConfig,
    tools: FsTools,
    #[allow(dead_code)] // Used internally by FsTools
    repomap: Arc<RwLock<Option<RepoMap>>>,
    client: Option<OpenAIClient>,
    conversation_history: Arc<Mutex<Vec<llm::types::ChatMessage>>>,
}

impl Executor {
    /// Creates a new `Executor`.
    /// Initializes the repomap, tools, LLM client, and other necessary components.
    pub fn new(cfg: AppConfig) -> Result<Self> {
        info!("Initializing Executor for exec subcommand");
        let repomap: Arc<RwLock<Option<RepoMap>>> = Arc::new(RwLock::new(None));
        let tools = FsTools::new(repomap.clone());

        // Only initialize repomap if not disabled
        if !cfg.no_repomap {
            let repomap_clone = repomap.clone();
            let project_root = cfg.project_root.clone();

            // Spawn an asynchronous task
            tokio::spawn(async move {
                info!(
                    "Starting background repomap generation for project at {:?}",
                    project_root
                );
                let start_time = std::time::Instant::now();
                let mut analyzer = match Analyzer::new(&project_root).await {
                    Ok(analyzer) => analyzer,
                    Err(e) => {
                        error!("Failed to create Analyzer: {:?}", e);
                        return;
                    }
                };

                match analyzer.build().await {
                    Ok(map) => {
                        let duration = start_time.elapsed();
                        let symbol_count = map.symbols.len();
                        *repomap_clone.write().await = Some(map);
                        tracing::debug!(
                            "Background repomap generation completed in {:?} with {} symbols",
                            duration,
                            symbol_count
                        );
                    }
                    Err(e) => {
                        error!("Failed to build RepoMap: {:?}", e);
                    }
                }
            });
        } else {
            info!("Repomap initialization skipped due to --no-repomap flag");
        }

        let client = match cfg.api_key.clone() {
            Some(key) => Some(OpenAIClient::new(cfg.base_url.clone(), key)?),
            None => None,
        };

        // Initialize conversation history
        let conversation_history = Arc::new(Mutex::new(Vec::new()));

        Ok(Self {
            cfg,
            tools,
            repomap,
            client,
            conversation_history,
        })
    }

    /// Runs the executor with the given instruction.
    /// Sends the instruction to the LLM, handles tool calls, and prints the final response to stdout.
    pub async fn run(&mut self, instruction: &str) -> Result<()> {
        if self.client.is_none() {
            eprintln!("OPENAI_API_KEY not set; cannot call LLM.");
            return Ok(());
        }

        let client = self.client.as_ref().unwrap();
        let model = self.cfg.model.clone();
        let fs_tools = self.tools.clone();
        let instruction = instruction.to_string();

        // Build initial messages with system prompt and user instruction
        let mut msgs = Vec::new();

        // Load system prompt
        let sys_prompt = crate::tui::commands::prompt::build_system_prompt(&self.cfg);
        msgs.push(llm::types::ChatMessage {
            role: "system".into(),
            content: Some(sys_prompt),
            tool_calls: vec![],
            tool_call_id: None,
        });

        // Add existing conversation history (should be empty for exec mode, but let's be safe)
        if let Ok(history) = self.conversation_history.lock() {
            msgs.extend(history.clone());
        }

        msgs.push(llm::types::ChatMessage {
            role: "user".into(),
            content: Some(instruction.clone()),
            tool_calls: vec![],
            tool_call_id: None,
        });

        // Create a channel to receive the final assistant message
        // Since we are not in a TUI, we will collect the output directly.
        let (tx, _rx) = std::sync::mpsc::channel::<String>(); // Buffer size is unbounded for std::sync::mpsc

        // Call run_agent_loop
        let res = llm::run_agent_loop(
            client,
            &model,
            &fs_tools,
            msgs,
            Some(tx), // Pass the sender
            None,     // No cancellation token for now
        )
        .await;

        // Get token usage after the agent loop completes
        let tokens_used = client.get_prompt_tokens_used();

        match res {
            Ok((_updated_messages, _final_msg)) => {
                // Print the final message content to stdout
                println!("{}", _final_msg.content);
                eprintln!("Total prompt tokens used: {}", tokens_used);
            }
            Err(e) => {
                eprintln!("LLM error: {}", e);
                eprintln!("Total prompt tokens used: {}", tokens_used);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_executor_new() {
        let temp_dir = TempDir::new().unwrap();
        let project_root = temp_dir.path().to_path_buf();

        // Create a minimal config without API key
        let cfg = AppConfig {
            base_url: "http://localhost:8080".to_string(),
            model: "test-model".to_string(),
            api_key: None, // No API key
            project_root: project_root.clone(),
            llm: crate::config::LlmConfig::default(), // Add default LlmConfig
            enable_stream_tools: false,               // Add enable_stream_tools
            theme: "default".to_string(),
            project_instructions_file: "PROJECT.md".to_string(), // Add project_instructions_file
            no_repomap: true,                                    // Disable repomap for simplicity
            auto_compact_prompt_token_threshold:
                crate::config::DEFAULT_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD,
        };

        let executor = Executor::new(cfg);
        assert!(executor.is_ok());
        let executor = executor.unwrap();
        assert!(executor.client.is_none()); // Client should not be initialized without API key
    }

    #[tokio::test]
    async fn test_executor_run_no_api_key() {
        let temp_dir = TempDir::new().unwrap();
        let project_root = temp_dir.path().to_path_buf();

        // Create a minimal config without API key
        let cfg = AppConfig {
            base_url: "http://localhost:8080".to_string(),
            model: "test-model".to_string(),
            api_key: None, // No API key
            project_root: project_root.clone(),
            llm: crate::config::LlmConfig::default(), // Add default LlmConfig
            enable_stream_tools: false,               // Add enable_stream_tools
            theme: "default".to_string(),
            project_instructions_file: "PROJECT.md".to_string(), // Add project_instructions_file
            no_repomap: true,                                    // Disable repomap for simplicity
            auto_compact_prompt_token_threshold:
                crate::config::DEFAULT_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD,
        };

        let mut executor = Executor::new(cfg).unwrap();

        // Capture stderr to check for the error message
        // Note: Directly capturing stderr in tests is complex and platform-dependent.
        // For now, we'll just ensure the function runs without panicking.
        // A more robust test would mock the LLM client or use a test harness.

        let result = executor.run("test instruction").await;
        assert!(result.is_ok()); // The function should return Ok(()) even if it can't call the LLM
        // Ideally, we would check the output (stderr) for "OPENAI_API_KEY not set"
        // but capturing stdout/stderr in tests is non-trivial.
        // This test at least ensures the code path is executed without panic.
    }

    // Additional tests could be added here, such as:
    // - Mocking the OpenAIClient to simulate successful LLM responses.
    // - Mocking the FsTools to simulate tool calls.
    // However, mocking these components would require more complex setup or dependency injection.
}
