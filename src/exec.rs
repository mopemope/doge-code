//! Module for the `exec` subcommand.
//! This module provides functionality to execute a single instruction
//! provided via command-line arguments, interact with the LLM, use tools,
//! and output the final result to stdout.

use crate::analysis::RepoMap;
use crate::config::AppConfig;
use crate::hooks::{HookManager, repomap_update::RepomapUpdateHook};
use crate::llm::ChatHistory;
use crate::llm::{self, OpenAIClient};
use crate::tools::FsTools;
use anyhow::{Context, Result};
use notify_rust::Notification;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::fs;
use tokio::sync::RwLock;
use tracing::info;

#[derive(thiserror::Error, Debug)]
pub enum ExecError {
    #[error("Execution failed: {0}")]
    Failed(#[from] anyhow::Error),
    #[error("Timeout")]
    Timeout,
}

/// Executor for the `exec` subcommand.
/// This struct holds the necessary components to interact with the LLM and tools.
pub struct Executor {
    cfg: AppConfig,
    tools: FsTools,
    #[allow(dead_code)] // Used internally by FsTools
    repomap: Arc<RwLock<Option<RepoMap>>>,
    client: Option<OpenAIClient>,
    pub conversation_history: Arc<tokio::sync::Mutex<ChatHistory>>,
    hook_manager: HookManager,
}

impl Executor {
    /// Creates a new `Executor`.
    /// Initializes the repomap, tools, LLM client, and other necessary components.
    pub async fn new(cfg: AppConfig) -> Result<Self> {
        info!("Initializing Executor for exec subcommand");
        let repomap: Arc<RwLock<Option<RepoMap>>> = Arc::new(RwLock::new(None));
        // Initialize session manager for exec mode using project root
        let session_store =
            crate::session::SessionStore::new(cfg.project_root.join(".doge/sessions"))?;
        let session_manager = Arc::new(Mutex::new(crate::session::SessionManager::with_store(
            session_store,
        )));

        // With --resume, skip the eager fresh session so that "latest"
        // resolves to the most recently updated pre-existing session.
        if cfg.resume.is_none() {
            let mut session_mgr = session_manager
                .lock()
                .map_err(|e| anyhow::anyhow!("Failed to lock session manager: {}", e))?;
            if session_mgr.current_session.is_none() {
                session_mgr.create_session(None)?;
            }
        }
        let tools = FsTools::new(repomap.clone(), Arc::new(cfg.clone()))
            .with_session_manager(session_manager.clone());

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

        // If resume is requested, load the specified (or latest) session and
        // populate history. Prefixes are resolved via resolve_and_load_session.
        if let Some(resume_id) = cfg.resume.as_deref() {
            let (conversation_to_resume, session_id) = {
                let mut session_mgr = session_manager
                    .lock()
                    .map_err(|e| anyhow::anyhow!("Failed to lock session manager: {}", e))?;
                match resume_id {
                    "latest" => {
                        session_mgr.load_latest_session()?;
                        match &session_mgr.current_session {
                            Some(session) => (
                                Some(session.conversation.clone()),
                                Some(session.meta.id.clone()),
                            ),
                            None => (None, None),
                        }
                    }
                    id => {
                        let session = session_mgr.resolve_and_load_session(id)?;
                        (
                            Some(session.conversation.clone()),
                            Some(session.meta.id.clone()),
                        )
                    }
                }
            };

            match (conversation_to_resume, session_id) {
                (Some(conversation), Some(id)) => {
                    info!("Resuming session: {}", id);
                    let mut history = conversation_history.lock().await;
                    for entry in conversation {
                        if let Ok(value) = serde_json::to_value(entry)
                            && let Ok(msg) =
                                serde_json::from_value::<crate::llm::types::ChatMessage>(value)
                        {
                            history.append_message(msg);
                        }
                    }
                }
                (None, None) => {
                    // "latest" with no pre-existing sessions: start fresh.
                    info!("No sessions to resume; starting a new session");
                    let mut session_mgr = session_manager
                        .lock()
                        .map_err(|e| anyhow::anyhow!("Failed to lock session manager: {}", e))?;
                    session_mgr.create_session(None)?;
                }
                _ => unreachable!("conversation and session id are always set together"),
            }
        }

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
                // Update conversation history with new interactions
                {
                    let mut history_guard = self.conversation_history.lock().await;

                    // We sent `msgs` in. `updated_messages` contains `msgs` + new messages.
                    // We need to find where the new messages start.
                    // The initial len was `msgs.len()`.
                    // But wait, `run_agent_loop` might compact history?
                    // If compaction happened, `updated_messages` might look totally different.
                    // Ideally, we should just trust `updated_messages` as the new truth?
                    // But `ChatHistory` optimizes storage.
                    // For now, let's append the *delta*.
                    // The safest way given compaction possibilities is to check if the history was compacted.
                    // But for `exec` mode, let's assume standard appending for now as compaction handles its own history replacement if implemented deep.
                    // Actually, `run_agent_loop` logic handles compaction internally on the `messages` vec.
                    // So `updated_messages` IS the current valid state of the conversation.

                    // Ideally we would replace `ChatHistory`'s content, but it might be easier to just append the difference
                    // if we assume no compaction for short workflows, OR we leverage `set_messages` if it exists.
                    // `ChatHistory` usually doesn't expose internal vec replacement easily to avoid invalid states.
                    // Let's iterate and append new messages.
                    // Original count:
                    // Note: `msgs` was consumed/cloned. We don't have the original `msgs` count variable easily available after await unless we saved it.
                    // But wait, we pushed System + User.
                    // Let's assume we want to capture the Assistant steps + Result.

                    // Check if we can identify new messages.
                    // A simple heuristic: append messages that are NOT in the original set?
                    // Or, simpler: we know we added 2 messages (System + User).
                    // So anything after index `initial_msg_count` are new.

                    // Note: `msgs` is moved into `run_agent_loop`. We can't query it.
                    // But we know how many we added?
                    // We took `history_msgs` + System + User.
                    // Let's just blindly append the *last* few messages? No.

                    // Correct approach:
                    // 1. We know `history_guard` has the *old* history + user prompt (we appended it).
                    // 2. `updated_messages` has *old* history + user prompt + system prompt(maybe) + new steps.
                    // We generally just want to append the *assistant* responses and *tool* outputs.

                    let existing_count = history_guard.build_messages().len();
                    // Verify if `updated_messages` contains the pre-existing ones.
                    // If `updated_messages` is shorter, compaction likely happened.

                    if updated_messages.len() > existing_count {
                        // Append the delta
                        for msg in updated_messages.iter().skip(existing_count) {
                            // Skip system prompt if it was injected internally and duplicates?
                            // Just appending is safer to preserve the agent's view.
                            history_guard.append_message(msg.clone());
                        }
                    }
                }

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

        let client = self
            .client
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Client not initialized"))?;
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

    /// Sends an instruction to the LLM and returns the response content.
    /// Does NOT print to stdout.
    pub async fn ask(&mut self, instruction: &str) -> Result<String> {
        if self.client.is_none() {
            return Err(anyhow::anyhow!("OPENAI_API_KEY not set"));
        }

        let mut msgs = Vec::new();
        let sys_prompt = crate::tui::commands::prompt::build_system_prompt(&self.cfg);

        {
            let history_guard = self.conversation_history.lock().await;
            msgs.extend(history_guard.build_messages());
        }

        msgs.push(llm::types::ChatMessage {
            role: "system".into(),
            content: Some(sys_prompt),
            tool_calls: vec![],
            tool_call_id: None,
        });

        msgs.push(llm::types::ChatMessage {
            role: "user".into(),
            content: Some(instruction.to_string()),
            tool_calls: vec![],
            tool_call_id: None,
        });

        {
            let mut history_guard = self.conversation_history.lock().await;
            history_guard.append_user(instruction);
        }

        let (updated_messages, final_msg) = llm::run_agent_loop(
            self.client.as_ref().expect("LLM client is not initialized"),
            &self.cfg.model,
            &self.tools,
            msgs,
            None,
            None,
            &self.cfg,
            None,
        )
        .await?;

        {
            let mut history_guard = self.conversation_history.lock().await;
            let existing_count = history_guard.build_messages().len();
            if updated_messages.len() > existing_count {
                for msg in updated_messages.iter().skip(existing_count) {
                    history_guard.append_message(msg.clone());
                }
            }
        }

        Ok(final_msg.content)
    }

    /// Add a hook to be executed after each instruction
    pub fn add_hook(&mut self, hook: Box<dyn crate::hooks::InstructionHook>) {
        self.hook_manager.add_hook(hook);
    }

    /// Get access to the hook manager.
    pub fn hook_manager(&mut self) -> &mut crate::hooks::HookManager {
        &mut self.hook_manager
    }

    /// Seeds the executor with existing conversation history.
    pub async fn with_history(self, messages: Vec<llm::types::ChatMessage>) -> Self {
        {
            let mut history = self.conversation_history.lock().await;
            history.overwrite_messages(messages);
        }
        self
    }

    /// Get a reference to the OpenAI client
    pub fn client(&self) -> Option<&OpenAIClient> {
        self.client.as_ref()
    }

    /// Get a reference to the FsTools
    pub fn tools(&self) -> &FsTools {
        &self.tools
    }

    /// Get a mutable reference to the FsTools
    pub fn tools_mut(&mut self) -> &mut FsTools {
        &mut self.tools
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
    let mut executor = Executor::new(cfg).await?;
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
        let temp_dir = TempDir::new().expect("Failed to create temporary directory");
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
            resume: None,     // Add resume field
            auto_compact_prompt_token_threshold:
                crate::config::DEFAULT_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD,
            auto_compact_prompt_token_threshold_overrides: HashMap::new(),
            show_diff: true,
            allowed_paths: vec![],
            allowed_commands: vec![], // Add allowed_commands
            command_timeout_ms: crate::config::DEFAULT_COMMAND_TIMEOUT_MS,
            mcp_servers: vec![crate::config::McpServerConfig::default()], // Add mcp_servers field
            rewrite_timeout_sec: 30,
        };

        let executor = Executor::new(cfg).await;
        assert!(executor.is_ok());
        let executor = executor;
        assert!(executor.is_ok());
        let executor = executor.unwrap();
        assert!(executor.client.is_none()); // Client should not be initialized without API key
    }

    #[tokio::test]
    async fn test_executor_run_no_api_key() {
        let temp_dir = TempDir::new().expect("Failed to create temporary directory");
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
            resume: None,     // Add resume field
            auto_compact_prompt_token_threshold:
                crate::config::DEFAULT_AUTO_COMPACT_PROMPT_TOKEN_THRESHOLD,
            auto_compact_prompt_token_threshold_overrides: HashMap::new(),
            show_diff: true,
            allowed_paths: vec![],
            allowed_commands: vec![], // Add allowed_commands
            command_timeout_ms: crate::config::DEFAULT_COMMAND_TIMEOUT_MS,
            mcp_servers: vec![crate::config::McpServerConfig::default()], // Add mcp_servers field
            rewrite_timeout_sec: 30,
        };

        let mut executor = Executor::new(cfg).await.expect("Failed to create executor");

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
        let rewritten = super::extract_rewritten_code(&response, &original)
            .expect("Failed to extract rewritten code");
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
        let rewritten = super::extract_rewritten_code(&response, original)
            .expect("Failed to extract rewritten code");
        assert_eq!(rewritten, snippet);
    }

    #[test]
    fn test_format_location_hint_relative_path() {
        let root = PathBuf::from("/tmp/doge_project");
        let file_path = root.join("src").join("lib.rs");
        let hint = super::format_location_hint(
            file_path
                .to_str()
                .expect("File path contains invalid UTF-8"),
            &root,
        );
        let expected = format!("src{}lib.rs", std::path::MAIN_SEPARATOR);
        assert_eq!(hint, expected);
    }

    #[test]
    fn test_format_location_hint_outside_project() {
        let root = PathBuf::from("/tmp/doge_project");
        let file_path = PathBuf::from("/var/tmp/other.rs");
        let hint = super::format_location_hint(
            file_path
                .to_str()
                .expect("File path contains invalid UTF-8"),
            &root,
        );
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
        let executor = Executor::new(cfg).await.expect("Failed to create executor");
        assert!(executor.client.is_none());
    }

    #[tokio::test]
    async fn test_executor_new_with_api_key() {
        let cfg = AppConfig {
            api_key: Some("test_key".to_string()),
            ..Default::default()
        };
        let executor = Executor::new(cfg).await.expect("Failed to create executor");
        assert!(executor.client.is_some());
    }

    /// Seed a session with conversation history in a temp project root and
    /// return its ID.
    fn seed_session(project_root: &Path, content: &str) -> String {
        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))
            .expect("Failed to create session store");
        let mut session = store.create().expect("Failed to create session");
        let mut entry = HashMap::new();
        entry.insert(
            "role".to_string(),
            serde_json::Value::String("user".to_string()),
        );
        entry.insert(
            "content".to_string(),
            serde_json::Value::String(content.to_string()),
        );
        session.add_conversation_entry(entry);
        store.save(&session).expect("Failed to save session");
        session.meta.id
    }

    #[tokio::test]
    async fn test_executor_resume_latest_restores_history() {
        let temp_dir = TempDir::new().expect("Failed to create temporary directory");
        let project_root = temp_dir.path().to_path_buf();
        let seeded_id = seed_session(&project_root, "seeded instruction");

        let cfg = AppConfig {
            project_root: project_root.clone(),
            resume: Some("latest".to_string()),
            ..Default::default()
        };
        let executor = Executor::new(cfg).await.expect("Failed to create executor");

        // The seeded conversation must be restored, and no extra fresh
        // session may shadow the latest one.
        let history = executor.conversation_history.lock().await;
        let messages = history.build_messages();
        assert!(
            messages
                .iter()
                .any(|m| m.role == "user" && m.content.as_deref() == Some("seeded instruction")),
            "resumed history should contain the seeded user message"
        );
        drop(history);

        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))
            .expect("Failed to open store");
        let summaries = store.list_with_stats().expect("Failed to list sessions");
        assert_eq!(summaries.len(), 1, "no extra session should be created");
        assert_eq!(summaries[0].meta.id, seeded_id);
    }

    #[tokio::test]
    async fn test_executor_resume_by_id_prefix() {
        let temp_dir = TempDir::new().expect("Failed to create temporary directory");
        let project_root = temp_dir.path().to_path_buf();
        let seeded_id = seed_session(&project_root, "prefix resumed");

        let prefix: String = seeded_id.chars().take(8).collect();
        let cfg = AppConfig {
            project_root: project_root.clone(),
            resume: Some(prefix),
            ..Default::default()
        };
        let executor = Executor::new(cfg).await.expect("Failed to create executor");

        let history = executor.conversation_history.lock().await;
        assert!(
            history
                .build_messages()
                .iter()
                .any(|m| m.role == "user" && m.content.as_deref() == Some("prefix resumed")),
            "prefix resume should restore the seeded conversation"
        );
    }

    #[tokio::test]
    async fn test_executor_resume_unknown_id_fails() {
        let temp_dir = TempDir::new().expect("Failed to create temporary directory");
        let cfg = AppConfig {
            project_root: temp_dir.path().to_path_buf(),
            resume: Some("deadbeef-dead-beef-dead-beefdeadbeef".to_string()),
            ..Default::default()
        };
        let result = Executor::new(cfg).await;
        assert!(result.is_err(), "resuming an unknown session ID must fail");
    }

    #[tokio::test]
    async fn test_executor_resume_latest_with_empty_store_creates_session() {
        let temp_dir = TempDir::new().expect("Failed to create temporary directory");
        let cfg = AppConfig {
            project_root: temp_dir.path().to_path_buf(),
            resume: Some("latest".to_string()),
            ..Default::default()
        };
        let executor = Executor::new(cfg).await.expect("Failed to create executor");
        assert!(executor.client.is_none());

        // A fresh session must exist so the run has somewhere to record
        // conversation history.
        let store = crate::session::SessionStore::new(temp_dir.path().join(".doge/sessions"))
            .expect("Failed to open store");
        let summaries = store.list_with_stats().expect("Failed to list sessions");
        assert_eq!(summaries.len(), 1, "a fresh session should be created");
    }

    // Additional tests could be added here, such as:
    // - Mocking the OpenAIClient to simulate successful LLM responses.
    // - Mocking the FsTools to simulate tool calls.
    // However, mocking these components would require more complex setup or dependency injection.
}
