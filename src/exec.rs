//! Module for the `exec` subcommand.
//! This module provides functionality to execute a single instruction
//! provided via command-line arguments, interact with the LLM, use tools,
//! and output the final result to stdout.

use crate::analysis::RepoMap;
use crate::config::AppConfig;
use crate::hooks::{HookManager, repomap_update::RepomapUpdateHook};
use crate::llm::ChatHistory;
use crate::llm::{self, OpenAIClient};
use crate::session::SessionManager;
use crate::tools::FsTools;
use anyhow::{Context, Result};
use notify_rust::Notification;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::fs;
use tokio::sync::RwLock;
use tracing::info;

pub mod fix;

/// Executor for the `exec` subcommand.
/// This struct holds the necessary components to interact with the LLM and tools.
pub struct Executor {
    cfg: AppConfig,
    tools: FsTools,
    #[allow(dead_code)] // Used internally by FsTools
    repomap: Arc<RwLock<Option<RepoMap>>>,
    client: Option<OpenAIClient>,
    conversation_history: Arc<tokio::sync::Mutex<ChatHistory>>,
    hook_manager: HookManager,
}

impl Executor {
    /// Creates a new `Executor`.
    /// Initializes the repomap, tools, LLM client, and other necessary components.
    pub fn new(cfg: AppConfig) -> Result<Self> {
        info!("Initializing Executor for exec subcommand");
        let repomap: Arc<RwLock<Option<RepoMap>>> = Arc::new(RwLock::new(None));
        // Initialize session manager for exec mode
        let session_manager = Arc::new(Mutex::new(SessionManager::new()?));

        // Create a default session if none exists
        {
            let mut session_mgr = session_manager.lock().unwrap();
            if session_mgr.current_session.is_none() {
                session_mgr.create_session(None)?;
            }
        }
        let tools = FsTools::new(repomap.clone(), Arc::new(cfg.clone()))
            .with_session_manager(session_manager);

        // Only initialize repomap if not disabled
        // For the exec command, we rely on the main initialization to handle repomap building
        // to prevent duplicate analyzer work and duplicate logging
        if cfg.no_repomap {
            info!("Repomap initialization skipped due to --no-repomap flag");
        }

        let client = match cfg.api_key.clone() {
            Some(key) => Some(OpenAIClient::new(cfg.base_url.clone(), key)?),
            None => None,
        };

        // Initialize conversation history with model-aware context sizing.
        // Fall back to a large default if no model context size is known.
        let max_tokens = cfg.get_context_window_size().unwrap_or(100_000) as usize;
        let conversation_history =
            Arc::new(tokio::sync::Mutex::new(ChatHistory::new(max_tokens, None)));

        Ok(Self {
            cfg,
            tools,
            repomap,
            client,
            conversation_history,
            hook_manager: {
                let mut hook_manager = HookManager::default();
                hook_manager.add_hook(Box::new(RepomapUpdateHook::new()));
                hook_manager
            },
        })
    }

    /// Runs the executor with the given instruction.
    /// Sends the instruction to the LLM, handles tool calls, and prints the final response to stdout.
    pub async fn run(&mut self, instruction: &str, json: bool) -> Result<()> {
        // ... existing code ...
        // Build initial messages with system prompt and user instruction
        let mut msgs = Vec::new();

        // Load system prompt
        let sys_prompt = crate::tui::commands::prompt::build_system_prompt(&self.cfg);
        // Note: With SmartChatHistory, we usually inject system prompt into it.
        // But run_agent_loop expects raw messages?
        // Actually, run_agent_loop takes `messages: Vec<ChatMessage>`.
        // So we build logic here.

        let mut history_guard: tokio::sync::MutexGuard<'_, ChatHistory> =
            self.conversation_history.lock().await;
        // Build messages explicitly to help inference
        let history_msgs = history_guard.build_messages();
        msgs.extend(history_msgs);
        // Inject system prompt into history if not present?
        // history_guard.system_prompt is private? No, we set it in new.
        // But we didn't set it in new().
        // Let's set it now.
        // Actually, cleaner to just use append_system_once but we can't change the field.
        // Let's just create a temporary vector for this run since exec is stateless for history mostly?
        // "Add existing conversation history (should be empty for exec mode...)"

        // Wait, if it's purely one-shot, we can just push to history.
        // But ChatHistory handles system prompt specially.
        // For now, let's just use the history messages + system prompt.

        // Actually, I should probably configure ChatHistory with system prompt in `run` if possible?
        // Or just prepend system prompt manually to the list passed to run_agent_loop.

        msgs.push(llm::types::ChatMessage {
            role: "system".into(),
            content: Some(sys_prompt),
            tool_calls: vec![],
            tool_call_id: None,
        });

        // Extended above

        msgs.push(llm::types::ChatMessage {
            role: "user".into(),
            content: Some(instruction.to_string()),
            tool_calls: vec![],
            tool_call_id: None,
        });

        // We update history with user message for consistency, though exec is one-shot.
        history_guard.append_user(instruction);
        drop(history_guard); // Free lock before loop

        // Call run_agent_loop
        let res = llm::run_agent_loop(
            self.client
                .as_ref()
                .context("OpenAI client not initialized")?,
            &self.cfg.model,
            &self.tools,
            msgs,
            None,
            None, // No cancellation token for now
            &self.cfg,
            None, // No TuiExecutor for exec mode
        )
        .await;

        // Get token usage after the agent loop completes
        let tokens_used = self
            .client
            .as_ref()
            .map(|c| c.get_prompt_tokens_used())
            .unwrap_or(0);

        match res {
            Ok((updated_messages, final_msg)) => {
                // Execute hooks after the agent loop completes
                let final_assistant_msg = crate::llm::types::ChatMessage {
                    role: "assistant".into(),
                    content: Some(final_msg.content.clone()),
                    tool_calls: vec![],
                    tool_call_id: None,
                };

                if let Err(e) = self
                    .hook_manager
                    .execute_hooks(
                        &updated_messages,
                        &final_assistant_msg,
                        &self.cfg,
                        &self.tools,
                        &self.repomap,
                    )
                    .await
                {
                    tracing::error!("Error executing hooks: {}", e);
                }

                if json {
                    let tools_called = collect_tools_called(&updated_messages);
                    let response = &final_msg.content;
                    let output = serde_json::json!({
                        "success": true,
                        "response": response,
                        "tokens_used": tokens_used,
                        "tools_called": tools_called,
                        "conversation_length": updated_messages.len()
                    });
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&output).unwrap_or_else(|_| {
                            r#"{"error": "JSON serialization failed"}"#.to_string()
                        })
                    );
                } else {
                    println!("{}", final_msg.content);
                    eprintln!("Total prompt tokens used: {}", tokens_used);
                }

                if !json {
                    // Send desktop notification on success
                    let summary = format!(
                        "Execution Completed Successfully\nTokens: {}\nSteps: {}",
                        tokens_used,
                        updated_messages.len()
                    );
                    if let Err(e) = Notification::new()
                        .summary("Doge-Code Agent Finished")
                        .body(&summary)
                        .show()
                    {
                        tracing::warn!("Failed to send desktop notification: {}", e);
                    }
                }
            }
            Err(e) => {
                tracing::error!("LLM execution failed: {}", e);
                if json {
                    let output = serde_json::json!({
                        "success": false,
                        "error": e.to_string(),
                        "tokens_used": tokens_used
                    });
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&output).unwrap_or_else(|_| {
                            r#"{"error": "JSON serialization failed"}"#.to_string()
                        })
                    );
                } else {
                    eprintln!("LLM error: {}", e);
                    eprintln!("Total prompt tokens used: {}", tokens_used);

                    // Send desktop notification on failure
                    let summary =
                        format!("Execution Failed\nError: {}\nTokens: {}", e, tokens_used);
                    if let Err(e) = Notification::new()
                        .summary("Doge-Code Agent Failed")
                        .body(&summary)
                        .show()
                    {
                        tracing::warn!("Failed to send desktop notification: {}", e);
                    }
                }
            }
        }

        Ok(())
    }

    /// Runs the executor in rewrite mode, returning the rewritten snippet.
    pub async fn run_rewrite(
        &mut self,
        prompt: &str,
        snippet: &str,
        file_path: Option<&str>,
        json: bool,
    ) -> Result<()> {
        if self.client.is_none() {
            if json {
                let output = serde_json::json!({
                    "success": false,
                    "error": "OPENAI_API_KEY not set; cannot call LLM.",
                    "tokens_used": 0
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&output).unwrap_or_else(|_| {
                        r#"{"error": "JSON serialization failed"}"#.to_string()
                    })
                );
            } else {
                eprintln!("OPENAI_API_KEY not set; cannot call LLM.");
            }
            return Ok(());
        }

        let client = self.client.as_ref().unwrap();
        let model = self.cfg.model.clone();
        let fs_tools = self.tools.clone();
        let original_file_path = file_path.map(|path| path.to_string());
        let display_path = original_file_path
            .as_deref()
            .map(|path| format_location_hint(path, &self.cfg.project_root));
        let request = build_rewrite_prompt(prompt, snippet, display_path.as_deref());

        let mut msgs = Vec::new();
        let sys_prompt = crate::tui::commands::prompt::build_system_prompt(&self.cfg);
        msgs.push(llm::types::ChatMessage {
            role: "system".into(),
            content: Some(sys_prompt),
            tool_calls: vec![],
            tool_call_id: None,
        });

        msgs.push(llm::types::ChatMessage {
            role: "user".into(),
            content: Some(request.clone()),
            tool_calls: vec![],
            tool_call_id: None,
        });

        let res = tokio::time::timeout(
            std::time::Duration::from_secs(self.cfg.rewrite_timeout_sec),
            llm::run_agent_loop(client, &model, &fs_tools, msgs, None, None, &self.cfg, None),
        )
        .await;

        let tokens_used = client.get_prompt_tokens_used();

        match res {
            Ok(Ok((updated_messages, final_msg))) => {
                let tools_called = collect_tools_called(&updated_messages);
                // Execute hooks after the agent loop completes
                let final_assistant_msg = crate::llm::types::ChatMessage {
                    role: "assistant".into(),
                    content: Some(final_msg.content.clone()),
                    tool_calls: vec![],
                    tool_call_id: None,
                };

                if let Err(e) = self
                    .hook_manager
                    .execute_hooks(
                        &updated_messages,
                        &final_assistant_msg,
                        &self.cfg,
                        &fs_tools,
                        &self.repomap,
                    )
                    .await
                {
                    tracing::error!("Error executing hooks: {}", e);
                }

                let raw_response = final_msg.content.clone();
                if let Some(rewritten) = extract_rewritten_code(&raw_response, snippet) {
                    // Security check: Ensure the rewritten code does not contain malicious patterns
                    if rewritten.contains("rm -rf")
                        || rewritten.contains("drop table")
                        || rewritten.contains("exec(")
                    {
                        tracing::error!(
                            "Security check failed: Rewritten code contains potentially malicious patterns"
                        );
                        if json {
                            let output = serde_json::json!({
                                "success": false,
                                "error": "Security check failed: Rewritten code contains potentially malicious patterns",
                                "tokens_used": tokens_used,
                                "file_path": original_file_path,
                                "display_path": display_path,
                            });
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&output).unwrap_or_else(|_| {
                                    r#"{"error": "JSON serialization failed"}"#.to_string()
                                })
                            );
                        } else {
                            eprintln!(
                                "Security check failed: Rewritten code contains potentially malicious patterns"
                            );
                            eprintln!("Total prompt tokens used: {}", tokens_used);
                        }
                        return Ok(());
                    }
                    if json {
                        let output = serde_json::json!({
                            "success": true,
                            "mode": "rewrite",
                            "rewritten_code": rewritten,
                            "tokens_used": tokens_used,
                            "tools_called": tools_called,
                            "raw_response": raw_response,
                            "file_path": original_file_path,
                            "display_path": display_path,
                        });
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&output).unwrap_or_else(|_| {
                                r#"{"error": "JSON serialization failed"}"#.to_string()
                            })
                        );
                    } else {
                        println!("{}", rewritten);
                        eprintln!("Total prompt tokens used: {}", tokens_used);
                    }

                    // Send desktop notification on successful rewrite
                    let file_description = display_path.as_deref().unwrap_or("the file");
                    if let Err(e) = Notification::new()
                        .summary("Doge-Code Rewrite Completed")
                        .body(&format!(
                            "Successfully rewrote code in {}",
                            file_description
                        ))
                        .show()
                    {
                        tracing::warn!("Failed to send desktop notification: {}", e);
                    }
                } else {
                    let parse_error = "Failed to parse rewritten code from model response";
                    if json {
                        let output = serde_json::json!({
                            "success": false,
                            "error": parse_error,
                            "raw_response": raw_response,
                            "tokens_used": tokens_used,
                            "file_path": original_file_path,
                            "display_path": display_path,
                        });
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&output).unwrap_or_else(|_| {
                                r#"{"error": "JSON serialization failed"}"#.to_string()
                            })
                        );
                    } else {
                        eprintln!("{}", parse_error);
                        eprintln!("{}", raw_response);
                        eprintln!("Total prompt tokens used: {}", tokens_used);
                    }
                }
            }
            Ok(Err(e)) => {
                // Inner error from the agent loop
                if json {
                    let output = serde_json::json!({
                        "success": false,
                        "error": e.to_string(),
                        "tokens_used": tokens_used,
                        "file_path": original_file_path,
                        "display_path": display_path,
                    });
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&output).unwrap_or_else(|_| {
                            r#"{"error": "JSON serialization failed"}"#.to_string()
                        })
                    );
                } else {
                    eprintln!("LLM error: {}", e);
                    eprintln!("Total prompt tokens used: {}", tokens_used);
                }
            }
            Err(e) => {
                // Timeout error
                if json {
                    let output = serde_json::json!({
                        "success": false,
                        "error": format!("Timeout error: {}", e),
                        "tokens_used": tokens_used,
                        "file_path": original_file_path,
                        "display_path": display_path,
                    });
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&output).unwrap_or_else(|_| {
                            r#"{"error": "JSON serialization failed"}"#.to_string()
                        })
                    );
                } else {
                    eprintln!("Timeout error: {}", e);
                    eprintln!("Total prompt tokens used: {}", tokens_used);
                }
            }
        }

        Ok(())
    }

    /// Add a hook to be executed after each instruction
    pub fn add_hook(&mut self, hook: Box<dyn crate::hooks::InstructionHook>) {
        self.hook_manager.add_hook(hook);
    }

    /// Get access to the hook manager
    pub fn hook_manager(&mut self) -> &mut crate::hooks::HookManager {
        &mut self.hook_manager
    }
}

fn collect_tools_called(messages: &[crate::llm::types::ChatMessage]) -> Vec<String> {
    messages
        .iter()
        .flat_map(|m| m.tool_calls.iter())
        .map(|tc| tc.function.name.clone())
        .collect()
}

const REWRITE_MARKER_START: &str = "<REWRITTEN_CODE>";
const REWRITE_MARKER_END: &str = "</REWRITTEN_CODE>";
const SNIPPET_MARKER_START: &str = "<ORIGINAL_SNIPPET>";
const SNIPPET_MARKER_END: &str = "</ORIGINAL_SNIPPET>";

fn format_location_hint(file_path: &str, project_root: &Path) -> String {
    let file_path = Path::new(file_path);

    if let Ok(relative) = file_path.strip_prefix(project_root)
        && relative.components().count() > 0
    {
        return relative.display().to_string();
    }

    file_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_path.display().to_string())
}

fn build_rewrite_prompt(prompt: &str, snippet: &str, file_path: Option<&str>) -> String {
    let location_hint = file_path.unwrap_or("the current buffer");
    format!(
        "You are an expert software engineer helping to rewrite a code snippet for a user editing a file inside Emacs.\n\
Only work with the provided snippet; do not assume or modify code outside it.\n\
User request (natural language):\n{}\n\
Original snippet (from {}), wrapped between {} and {} markers:\n{}\n\
Rewrite the snippet so it satisfies the request.\n\
Return only the rewritten snippet enclosed between {} and {} markers.\n\
Do not include explanations, commentary, code fences, or additional text outside the markers.",
        prompt.trim(),
        location_hint,
        SNIPPET_MARKER_START,
        SNIPPET_MARKER_END,
        wrap_with_snippet_markers(snippet),
        REWRITE_MARKER_START,
        REWRITE_MARKER_END
    )
}

fn wrap_with_snippet_markers(snippet: &str) -> String {
    format!(
        "{}\n{}\n{}",
        SNIPPET_MARKER_START, snippet, SNIPPET_MARKER_END
    )
}

fn extract_rewritten_code(response: &str, original_snippet: &str) -> Option<String> {
    let start = response.find(REWRITE_MARKER_START)? + REWRITE_MARKER_START.len();
    let rest = &response[start..];
    let end = rest.find(REWRITE_MARKER_END)?;
    let snippet = &rest[..end];
    Some(adjust_rewrite_payload(snippet, original_snippet))
}

fn adjust_rewrite_payload(payload: &str, original_snippet: &str) -> String {
    let trimmed_start = payload.trim_start_matches(['\r', '\n']);
    let mut trimmed = trimmed_start.trim_end_matches(['\r', '\n']).to_string();
    if original_snippet.ends_with('\n') && !trimmed.ends_with('\n') {
        trimmed.push('\n');
    }
    trimmed
}

pub async fn run_rewrite(
    cfg: AppConfig,
    prompt: &str,
    code_file: &Path,
    file_path: Option<&str>,
    json: bool,
) -> Result<()> {
    let snippet = fs::read_to_string(code_file)
        .await
        .with_context(|| format!("Failed to read snippet from {}", code_file.display()))?;
    let mut executor = Executor::new(cfg)?;
    executor
        .run_rewrite(prompt, &snippet, file_path, json)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::{ChatMessage, ToolCall, ToolCallFunction};
    use std::collections::HashMap;
    use std::path::PathBuf;
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
            git_root: Some(project_root.clone()),
            llm: crate::config::LlmConfig::default(), // Add default LlmConfig
            watch_config: crate::config::WatchConfig::default(), // Add default WatchConfig
            enable_stream_tools: false,               // Add enable_stream_tools
            theme: "default".to_string(),
            project_instructions_file: Some("PROJECT.md".to_string()), // Add project_instructions_file
            no_repomap: true, // Disable repomap for simplicity
            resume: false,    // Add resume field
            auto_compact_prompt_token_threshold:
                crate::config::DEFAULT_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD,
            auto_compact_prompt_token_threshold_overrides: HashMap::new(),
            show_diff: true,
            allowed_paths: vec![],
            allowed_commands: vec![], // Add allowed_commands
            mcp_servers: vec![crate::config::McpServerConfig::default()], // Add mcp_servers field
            rewrite_timeout_sec: 30,
            rag: crate::config::RagConfig::default(),
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
            git_root: Some(project_root.clone()),
            llm: crate::config::LlmConfig::default(), // Add default LlmConfig
            watch_config: crate::config::WatchConfig::default(), // Add default WatchConfig
            enable_stream_tools: false,               // Add enable_stream_tools
            theme: "default".to_string(),
            project_instructions_file: Some("PROJECT.md".to_string()), // Add project_instructions_file
            no_repomap: true, // Disable repomap for simplicity
            resume: false,    // Add resume field
            auto_compact_prompt_token_threshold:
                crate::config::DEFAULT_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD,
            auto_compact_prompt_token_threshold_overrides: HashMap::new(),
            show_diff: true,
            allowed_paths: vec![],
            allowed_commands: vec![], // Add allowed_commands
            mcp_servers: vec![crate::config::McpServerConfig::default()], // Add mcp_servers field
            rewrite_timeout_sec: 30,
            rag: crate::config::RagConfig::default(),
        };

        let mut executor = Executor::new(cfg).unwrap();

        // Capture stderr to check for the error message
        // Note: Directly capturing stderr in tests is complex and platform-dependent.
        // For now, we'll just ensure the function runs without panicking.
        // A more robust test would mock the LLM client or use a test harness.

        let result = executor.run("test instruction", false).await;
        assert!(result.is_err()); // Should error because API key is missing
        // Ideally, we would check the output (stderr) for "OPENAI_API_KEY not set"
        // but capturing stdout/stderr in tests is non-trivial.
        // This test at least ensures the code path is executed without panic.
    }

    #[test]
    fn test_extract_rewritten_code_preserves_trailing_newline() {
        let snippet = "fn main() { println(\"hi\"); }";
        let response = format!(
            "{}\n{}\n{}",
            super::REWRITE_MARKER_START,
            snippet,
            super::REWRITE_MARKER_END
        );
        let original = format!("{}\n", snippet);
        let rewritten = super::extract_rewritten_code(&response, &original).unwrap();
        assert!(rewritten.ends_with('\n'));
        assert!(rewritten.starts_with("fn main()"));
    }

    #[test]
    fn test_extract_rewritten_code_trims_padding() {
        let snippet = "fn add(a: i32, b: i32) -> i32 { a + b }";
        let response = format!(
            "{}\n\n{}\n\n{}",
            super::REWRITE_MARKER_START,
            snippet,
            super::REWRITE_MARKER_END
        );
        let original = "fn add(a: i32, b: i32) -> i32 { a + b }";
        let rewritten = super::extract_rewritten_code(&response, original).unwrap();
        assert_eq!(rewritten, snippet);
    }

    #[test]
    fn test_format_location_hint_relative_path() {
        let root = PathBuf::from("/tmp/doge_project");
        let file_path = root.join("src").join("lib.rs");
        let hint = super::format_location_hint(file_path.to_str().unwrap(), &root);
        let expected = format!("src{}lib.rs", std::path::MAIN_SEPARATOR);
        assert_eq!(hint, expected);
    }

    #[test]
    fn test_format_location_hint_outside_project() {
        let root = PathBuf::from("/tmp/doge_project");
        let file_path = PathBuf::from("/var/tmp/other.rs");
        let hint = super::format_location_hint(file_path.to_str().unwrap(), &root);
        assert_eq!(hint, "other.rs");
    }

    #[test]
    fn test_collect_tools_called_preserves_order_and_duplicates() {
        let messages = vec![
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: vec![ToolCall {
                    id: Some("call-1".to_string()),
                    r#type: "function".to_string(),
                    function: ToolCallFunction {
                        name: "fs_read".to_string(),
                        arguments: "{}".to_string(),
                    },
                }],
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: vec![
                    ToolCall {
                        id: Some("call-2".to_string()),
                        r#type: "function".to_string(),
                        function: ToolCallFunction {
                            name: "fs_write".to_string(),
                            arguments: "{}".to_string(),
                        },
                    },
                    ToolCall {
                        id: Some("call-3".to_string()),
                        r#type: "function".to_string(),
                        function: ToolCallFunction {
                            name: "fs_read".to_string(),
                            arguments: "{}".to_string(),
                        },
                    },
                ],
                tool_call_id: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some("ok".to_string()),
                tool_calls: vec![],
                tool_call_id: Some("call-1".to_string()),
            },
        ];

        assert_eq!(
            collect_tools_called(&messages),
            vec![
                "fs_read".to_string(),
                "fs_write".to_string(),
                "fs_read".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn test_executor_new_without_api_key() {
        let cfg = AppConfig {
            api_key: None,
            ..Default::default()
        };
        let executor = Executor::new(cfg).unwrap();
        assert!(executor.client.is_none());
    }

    #[tokio::test]
    async fn test_executor_new_with_api_key() {
        let cfg = AppConfig {
            api_key: Some("test_key".to_string()),
            ..Default::default()
        };
        let executor = Executor::new(cfg).unwrap();
        assert!(executor.client.is_some());
    }

    // Additional tests could be added here, such as:
    // - Mocking the OpenAIClient to simulate successful LLM responses.
    // - Mocking the FsTools to simulate tool calls.
    // However, mocking these components would require more complex setup or dependency injection.
}
