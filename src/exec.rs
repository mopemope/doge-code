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

/// Agent completion is separate from transport success and answer quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecOutcome {
    pub status: crate::llm::tool_execution::AgentRunStatus,
    pub stop_reason: Option<crate::llm::tool_execution::AgentStopReason>,
}

impl From<&crate::llm::tool_execution::AgentRunResult> for ExecOutcome {
    fn from(run: &crate::llm::tool_execution::AgentRunResult) -> Self {
        Self {
            status: run.status,
            stop_reason: run.stop_reason,
        }
    }
}

impl ExecOutcome {
    pub fn is_completed(self) -> bool {
        self.status == crate::llm::tool_execution::AgentRunStatus::Completed
    }

    pub fn require_completed(self) -> Result<()> {
        if self.is_completed() {
            Ok(())
        } else {
            Err(IncompleteExecution(self).into())
        }
    }

    fn notification(self, tokens: u64, steps: usize) -> (&'static str, String) {
        if self.is_completed() {
            (
                "Doge-Code Agent Finished",
                format!("Execution Completed Successfully\nTokens: {tokens}\nSteps: {steps}"),
            )
        } else {
            (
                "Doge-Code Agent Stopped",
                format!(
                    "Execution Partial\nReason: {}\nTokens: {tokens}\nSteps: {steps}",
                    self.stop_reason.map(|r| r.as_str()).unwrap_or("unknown")
                ),
            )
        }
    }
}

#[derive(Debug)]
pub struct IncompleteExecution(pub ExecOutcome);

impl std::fmt::Display for IncompleteExecution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Agent execution is partial: {}",
            self.0.stop_reason.map(|r| r.as_str()).unwrap_or("unknown")
        )
    }
}

impl std::error::Error for IncompleteExecution {}

/// Whether desktop notifications are enabled for agent runs.
///
/// Bulk evaluation runs set `DGC_DISABLE_NOTIFICATIONS=1` to avoid
/// notification spam across many trials. Unset (or any other value) keeps
/// the default behavior so normal users are unaffected.
pub fn notifications_enabled_for_value(var: Option<&str>) -> bool {
    !matches!(var, Some("1"))
}

pub(crate) fn notifications_enabled() -> bool {
    notifications_enabled_for_value(std::env::var("DGC_DISABLE_NOTIFICATIONS").ok().as_deref())
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

        let client = OpenAIClient::from_config(&cfg)?;

        // Initialize the durable conversation buffer. It owns no system
        // prompt and no token budget; context reduction lives in
        // HistoryManager / the Context Budget Governor.
        let conversation_history = Arc::new(tokio::sync::Mutex::new(ChatHistory::new()));

        // If resume is requested, load the specified (or latest) session and
        // populate history. Targets are decoded and validated before they
        // become current: a malformed target fails loudly instead of
        // committing a session whose conversation cannot be restored.
        if let Some(resume_id) = cfg.resume.as_deref() {
            let resumed = {
                let mut session_mgr = session_manager
                    .lock()
                    .map_err(|e| anyhow::anyhow!("Failed to lock session manager: {}", e))?;
                match resume_id {
                    "latest" => session_mgr.load_latest_validated_excluding(None)?,
                    id => Some(
                        session_mgr
                            .switch_to_validated_session(id)
                            .with_context(|| format!("Failed to resume session '{id}'"))?,
                    ),
                }
            };

            match resumed {
                Some((session, messages)) => {
                    info!("Resuming session: {}", session.meta.id);
                    let mut history = conversation_history.lock().await;
                    history.replace(crate::llm::durable_conversation_messages(messages));
                }
                None => {
                    // "latest" with no pre-existing sessions: start fresh.
                    info!("No sessions to resume; starting a new session");
                    let mut session_mgr = session_manager
                        .lock()
                        .map_err(|e| anyhow::anyhow!("Failed to lock session manager: {}", e))?;
                    session_mgr.create_session(None)?;
                }
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

    /// Build one turn's request without mutating the outer history:
    /// default system prompt + durable conversation snapshot + the current
    /// user instruction.
    async fn build_request_messages(
        &self,
        instruction: &str,
    ) -> Vec<crate::llm::types::ChatMessage> {
        let snapshot = self.conversation_history.lock().await.snapshot();
        let sys_prompt = crate::tui::commands::prompt::build_system_prompt(&self.cfg);
        let mut msgs = Vec::with_capacity(snapshot.len() + 2);
        msgs.push(llm::types::ChatMessage {
            provider_state: None,
            role: "system".into(),
            content: Some(sys_prompt),
            tool_calls: vec![],
            tool_call_id: None,
        });
        msgs.extend(snapshot);
        msgs.push(llm::types::ChatMessage {
            provider_state: None,
            role: "user".into(),
            content: Some(instruction.to_string()),
            tool_calls: vec![],
            tool_call_id: None,
        });
        msgs
    }

    /// Commit the agent loop's canonical result: project to durable messages,
    /// replace the outer buffer as-is, and persist the whole durable
    /// conversation to the session. Never a count/index-based delta, so a
    /// compacted (shorter, reordered) history commits exactly.
    async fn commit_canonical_history(
        &self,
        updated_messages: &[crate::llm::types::ChatMessage],
    ) -> Result<()> {
        let durable = crate::llm::durable_conversation_messages(updated_messages.iter().cloned());
        {
            let mut history = self.conversation_history.lock().await;
            history.replace(durable);
        }
        self.persist_outer_history().await
    }

    /// Persist the current outer conversation to the session store so a later
    /// `--resume` sees exactly this conversation.
    async fn persist_outer_history(&self) -> Result<()> {
        let snapshot = self.conversation_history.lock().await.snapshot();
        if let Some(manager) = self
            .tools
            .get_session_manager_wrapper()
            .get_session_manager()
            .clone()
        {
            let mut sm = manager
                .lock()
                .map_err(|e| anyhow::anyhow!("Failed to lock session manager: {}", e))?;
            sm.update_current_session_with_history(&snapshot)?;
        }
        Ok(())
    }

    /// Restore the provider-independent checkpoint, including interrupted calls.
    async fn restore_history_after_failure(&self) -> Result<()> {
        let manager = self
            .tools
            .get_session_manager_wrapper()
            .get_session_manager()
            .as_ref()
            .context("No session manager for failure recovery")?;
        let messages = {
            let session_manager = crate::utils::safe_std_lock(manager, "session_manager")?;
            let session = session_manager
                .current_session
                .as_ref()
                .context("No current session for failure recovery")?;
            crate::llm::durable_conversation_messages(session.conversation_messages()?)
        };
        self.conversation_history.lock().await.replace(messages);
        Ok(())
    }

    /// Final CLI checkpoint retry reuses the canonical SessionData payload.
    pub(crate) fn flush_session(&self) -> Result<()> {
        if let Some(manager) = self
            .tools
            .get_session_manager_wrapper()
            .get_session_manager()
        {
            let mut manager = crate::utils::safe_std_lock(manager, "session_manager")?;
            if let Err(error) = manager.flush_before_transition() {
                return Err(manager.recover_capacity_exit(error));
            }
            if let Some(message) = manager.checkpoint_warning() {
                eprintln!("Warning: {message}");
            }
        }
        Ok(())
    }

    /// Runs the executor with the given instruction.
    /// Sends the instruction to the LLM, handles tool calls, and prints the final response to stdout.
    pub async fn run(&mut self, instruction: &str, json: bool) -> Result<ExecOutcome> {
        self.run_with_cancel(instruction, json, None).await
    }

    pub async fn run_with_cancel(
        &mut self,
        instruction: &str,
        json: bool,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<ExecOutcome> {
        // Build this turn's request from the durable snapshot. The outer
        // history stays untouched until the canonical result commits.
        let msgs = self.build_request_messages(instruction).await;
        crate::llm::validate_tool_blocks(&msgs, false)?;

        // Missing client means the agent never starts: record no directive
        // (matches TUI busy/missing-key semantics).
        let client_ref = match self.client.as_ref() {
            Some(client) => client,
            None => anyhow::bail!("OpenAI client not initialized"),
        };

        // Record the observed user directive (exec run). A recording failure
        // never aborts the turn: mark provenance_incomplete and continue
        // with directive_id = None.
        let attribution = match crate::tools::provenance::record_directive_observed(
            &self.tools,
            crate::provenance::DirectiveOrigin::ExecRun,
            instruction,
            instruction,
        ) {
            Ok(env) => crate::provenance::ProvenanceAttribution::with_directive(env.event_id),
            Err(e) => {
                tracing::warn!(error = %e, "provenance.directive_record_failed");
                let _ = self.tools.mark_current_session_provenance_failure();
                crate::provenance::ProvenanceAttribution::none()
            }
        };

        let usage_before = client_ref.usage_snapshot();
        // Call run_agent_loop
        let res = llm::run_agent_loop(
            client_ref,
            &self.cfg.model,
            &self.tools,
            msgs,
            None,
            cancel,
            &self.cfg,
            None, // No TuiExecutor for exec mode
            attribution,
        )
        .await;

        let usage = client_ref
            .usage_snapshot()
            .difference(&usage_before)
            .report();
        // Get token usage after the agent loop completes
        let tokens_used = self
            .client
            .as_ref()
            .map(|c| c.get_total_prompt_tokens_used())
            .unwrap_or(0);

        let res = match res {
            Ok(run) => match self.commit_canonical_history(&run.messages).await {
                Ok(()) => Ok(run),
                Err(error) => Err(error.context("failed to persist exec conversation history")),
            },
            Err(error) => Err(error),
        };

        match res {
            Ok(run) => {
                let outcome = ExecOutcome::from(&run);
                // Freeze navigation immediately after the canonical save, before hooks.
                let review_link = self.tools.get_current_session().and_then(|s| {
                    crate::features::evidence_report::ReviewLink::new(
                        &self.cfg.project_root,
                        &s.meta.id,
                    )
                });
                let updated_messages = run.messages;
                let final_msg = run.final_message;
                let status_str = match run.status {
                    crate::llm::tool_execution::AgentRunStatus::Completed => "completed",
                    crate::llm::tool_execution::AgentRunStatus::Partial => "partial",
                };
                let stop_reason = run.stop_reason.map(|r| r.as_str().to_string());
                let budget = serde_json::json!({
                    "progress": run.budget.progress,
                    "iterations": run.budget.iterations,
                    "tool_calls": run.budget.tool_calls,
                    "charged_tokens": run.budget.charged_tokens,
                    "elapsed_ms": run.budget.elapsed_ms,
                    "provider_reported_tokens": run.budget.provider_reported_tokens,
                    "estimated_tokens": run.budget.estimated_tokens,
                    "request_attempts": run.budget.request_attempts,
                    "usage_records": run.budget.usage_records,
                });
                // Canonical result: replace the outer buffer and persist the
                // whole durable conversation (never a count-based delta).

                // Execute hooks after the agent loop completes
                let final_assistant_msg = crate::llm::types::ChatMessage {
                    provider_state: None,
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
                        "success": outcome.is_completed(),
                        "status": status_str,
                        "stop_reason": stop_reason,
                        "budget": budget,
                        "response": response,
                        "tokens_used": tokens_used,
                        "usage": usage,
                        "tools_called": tools_called,
                        "conversation_length": updated_messages.len(),
                        "review_handoff": review_link
                    });
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&output).unwrap_or_else(|_| {
                            r#"{"error": "JSON serialization failed"}"#.to_string()
                        })
                    );
                } else {
                    println!("{}", final_msg.content);
                    if run.status == crate::llm::tool_execution::AgentRunStatus::Partial {
                        eprintln!(
                            "Agent stopped with partial result: {}",
                            stop_reason.as_deref().unwrap_or("unknown")
                        );
                    } else {
                        eprintln!("Total prompt tokens used: {}", tokens_used);
                    }
                }

                if !json && let Some(link) = review_link {
                    for line in link.lines() {
                        eprintln!("{line}");
                    }
                }
                if !json && notifications_enabled() {
                    let (title, summary) =
                        outcome.notification(tokens_used, updated_messages.len());
                    if let Err(e) = Notification::new().summary(title).body(&summary).show() {
                        tracing::warn!("Failed to send desktop notification: {}", e);
                    }
                }
                Ok(outcome)
            }
            Err(e) => {
                if let Err(recovery_error) = self.restore_history_after_failure().await {
                    tracing::error!(%recovery_error, "could not restore canonical checkpoint");
                }
                tracing::error!("LLM execution failed: {}", e);
                if json {
                    let output = serde_json::json!({
                        "success": false,
                        "error": e.to_string(),
                        "tokens_used": tokens_used,
                        "usage": usage
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
                    if notifications_enabled() {
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
                Err(e)
            }
        }
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
            provider_state: None,
            role: "system".into(),
            content: Some(sys_prompt),
            tool_calls: vec![],
            tool_call_id: None,
        });

        msgs.push(llm::types::ChatMessage {
            provider_state: None,
            role: "user".into(),
            content: Some(request.clone()),
            tool_calls: vec![],
            tool_call_id: None,
        });

        // Rewrite: raw_input is the user-supplied prompt; effective is the
        // generated rewrite request. Never store the generated request alone
        // as the user input.
        let attribution = match crate::tools::provenance::record_directive_observed(
            &self.tools,
            crate::provenance::DirectiveOrigin::ExecRewrite,
            prompt,
            &request,
        ) {
            Ok(env) => crate::provenance::ProvenanceAttribution::with_directive(env.event_id),
            Err(e) => {
                tracing::warn!(error = %e, "provenance.directive_record_failed");
                let _ = self.tools.mark_current_session_provenance_failure();
                crate::provenance::ProvenanceAttribution::none()
            }
        };

        let res = tokio::time::timeout(
            std::time::Duration::from_secs(self.cfg.rewrite_timeout_sec),
            llm::run_agent_loop(
                client,
                &model,
                &fs_tools,
                msgs,
                None,
                None,
                &self.cfg,
                None,
                attribution,
            ),
        )
        .await;

        let tokens_used = client.get_total_prompt_tokens_used() as u32;

        match res {
            Ok(Ok(run)) => {
                let outcome = ExecOutcome::from(&run);
                let updated_messages = run.messages;
                let final_msg = run.final_message;
                let tools_called = collect_tools_called(&updated_messages);
                // Execute hooks after the agent loop completes
                let final_assistant_msg = crate::llm::types::ChatMessage {
                    provider_state: None,
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

                if !outcome.is_completed() {
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "success": false, "status": "partial", "stop_reason": outcome.stop_reason,
                                "mode": "rewrite", "response": final_msg.content,
                                "tokens_used": tokens_used, "file_path": original_file_path,
                                "display_path": display_path,
                            }))?
                        );
                    } else {
                        eprintln!("{}", IncompleteExecution(outcome));
                    }
                    return outcome.require_completed();
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
                    if notifications_enabled()
                        && let Err(e) = Notification::new()
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

        // Request from the durable snapshot; the outer history is untouched
        // until the canonical result commits.
        let msgs = self.build_request_messages(instruction).await;

        let attribution = match crate::tools::provenance::record_directive_observed(
            &self.tools,
            crate::provenance::DirectiveOrigin::ExecAsk,
            instruction,
            instruction,
        ) {
            Ok(env) => crate::provenance::ProvenanceAttribution::with_directive(env.event_id),
            Err(e) => {
                tracing::warn!(error = %e, "provenance.directive_record_failed");
                let _ = self.tools.mark_current_session_provenance_failure();
                crate::provenance::ProvenanceAttribution::none()
            }
        };

        let res = llm::run_agent_loop(
            self.client.as_ref().expect("LLM client is not initialized"),
            &self.cfg.model,
            &self.tools,
            msgs,
            None,
            None,
            &self.cfg,
            None,
            attribution,
        )
        .await;
        match res {
            Ok(run) => {
                self.commit_canonical_history(&run.messages).await?;
                ExecOutcome::from(&run).require_completed()?;
                Ok(run.final_message.content)
            }
            Err(e) => {
                if let Err(recovery_error) = self.restore_history_after_failure().await {
                    tracing::error!(%recovery_error, "could not restore canonical checkpoint");
                }
                Err(e)
            }
        }
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
            history.replace(crate::llm::durable_conversation_messages(messages));
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
            provider: Default::default(),
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
            execution: crate::config::ExecutionConfig::default(),
            execution_configured: false,
            mcp_servers: vec![crate::config::McpServerConfig::default()], // Add mcp_servers field
            local_mcp_server: crate::config::LocalMcpServerConfig::default(),
            rewrite_timeout_sec: 30,
            tool_routing: crate::config::ToolRoutingConfig::default(),
            reasoning: crate::config::ReasoningConfig::default(),
            context_budget: crate::config::ContextBudgetConfig::default(),
            subagent: Default::default(),
            agent_budget: Default::default(),
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
            provider: Default::default(),
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
            execution: crate::config::ExecutionConfig::default(),
            execution_configured: false,
            mcp_servers: vec![crate::config::McpServerConfig::default()], // Add mcp_servers field
            local_mcp_server: crate::config::LocalMcpServerConfig::default(),
            rewrite_timeout_sec: 30,
            tool_routing: crate::config::ToolRoutingConfig::default(),
            reasoning: crate::config::ReasoningConfig::default(),
            context_budget: crate::config::ContextBudgetConfig::default(),
            subagent: Default::default(),
            agent_budget: Default::default(),
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
    fn test_notification_guard_defaults_to_enabled() {
        // Pure parser: no environment manipulation, so parallel test
        // execution cannot race on process-global env state.
        assert!(super::notifications_enabled_for_value(None));
        assert!(!super::notifications_enabled_for_value(Some("1")));
        assert!(super::notifications_enabled_for_value(Some("0")));
        assert!(super::notifications_enabled_for_value(Some("")));
        assert!(super::notifications_enabled_for_value(Some("true")));
    }

    #[test]
    fn test_collect_tools_called_preserves_order_and_duplicates() {
        let messages = vec![
            ChatMessage {
                provider_state: None,
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
                provider_state: None,
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
                provider_state: None,
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
    async fn test_executor_resume_malformed_fails_closed() {
        let temp_dir = TempDir::new().expect("Failed to create temporary directory");
        let project_root = temp_dir.path().to_path_buf();
        let seeded_id = seed_typed_session(&project_root, &[exec_msg("user", Some("ok"))]);
        // Corrupt one entry on disk.
        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))
            .expect("Failed to open store");
        let mut session = store.load(&seeded_id).expect("load");
        let mut bad = HashMap::new();
        bad.insert("role".to_string(), serde_json::json!(123));
        session.add_conversation_entry(bad);
        store.save(&session).expect("save corrupt");

        let cfg = AppConfig {
            project_root: project_root.clone(),
            resume: Some(seeded_id),
            ..Default::default()
        };
        match Executor::new(cfg).await {
            Ok(_) => panic!("malformed entry must fail resume, never partially load"),
            Err(err) => assert!(
                format!("{err:?}").contains("index 1"),
                "error chain must name the entry index: {err:?}"
            ),
        }
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

    #[tokio::test]
    async fn test_exec_directive_origins_and_hashes() {
        // Exec run/ask: raw == effective. Rewrite: raw (user prompt) !=
        // effective (generated request). Hashes are exact-byte BLAKE3.
        let run_raw = "Add cache";
        let run_effective = "Add cache";
        assert_eq!(
            crate::provenance::directive_content_hash(run_raw),
            crate::provenance::directive_content_hash(run_effective)
        );
        let rewrite_raw = "Optimize this";
        let rewrite_effective = build_rewrite_prompt(rewrite_raw, "fn f() {}", Some("src/lib.rs"));
        assert_ne!(rewrite_raw, rewrite_effective);
        assert_ne!(
            crate::provenance::directive_content_hash(rewrite_raw),
            crate::provenance::directive_content_hash(&rewrite_effective)
        );
        // Record helper works for exec origins without network.
        let temp_dir = TempDir::new().unwrap();
        let cfg = AppConfig {
            project_root: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let executor = Executor::new(cfg).await.unwrap();
        for origin in [
            crate::provenance::DirectiveOrigin::ExecRun,
            crate::provenance::DirectiveOrigin::ExecAsk,
            crate::provenance::DirectiveOrigin::ExecRewrite,
        ] {
            let env = crate::tools::provenance::record_directive_observed(
                executor.tools(),
                origin,
                "raw",
                "effective",
            )
            .unwrap();
            match &env.event {
                crate::provenance::ProvenanceEvent::DirectiveObserved(d) => {
                    assert_eq!(d.origin, origin);
                }
                _ => panic!("expected directive"),
            }
        }
    }

    // Additional tests could be added here, such as:
    // - Mocking the OpenAIClient to simulate successful LLM responses.
    // - Mocking the FsTools to simulate tool calls.
    // However, mocking these components would require more complex setup or dependency injection.

    fn exec_msg(role: &str, content: Option<&str>) -> ChatMessage {
        ChatMessage {
            provider_state: None,
            role: role.to_string(),
            content: content.map(str::to_string),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    #[test]
    fn incomplete_outcomes_preserve_budget_reasons_and_do_not_notify_success() {
        use crate::llm::tool_execution::{AgentRunStatus, AgentStopReason};
        for reason in [
            AgentStopReason::IterationBudget,
            AgentStopReason::ToolCallBudget,
            AgentStopReason::TokenBudget,
            AgentStopReason::ElapsedBudget,
        ] {
            let outcome = ExecOutcome {
                status: AgentRunStatus::Partial,
                stop_reason: Some(reason),
            };
            let error = outcome.require_completed().expect_err("partial");
            assert_eq!(
                error
                    .downcast_ref::<IncompleteExecution>()
                    .expect("typed outcome")
                    .0
                    .stop_reason,
                Some(reason)
            );
            let (title, body) = outcome.notification(100, 4);
            assert_eq!(title, "Doge-Code Agent Stopped");
            assert!(body.contains(reason.as_str()));
            assert!(!body.contains("Successfully"));
        }
        let complete = ExecOutcome {
            status: AgentRunStatus::Completed,
            stop_reason: None,
        };
        complete.require_completed().expect("completed");
        assert!(
            complete
                .notification(100, 4)
                .1
                .contains("Completed Successfully")
        );
    }

    #[tokio::test]
    async fn partial_rewrite_does_not_publish_code_as_completed() {
        use httptest::{Expectation, matchers::*, responders::*};
        let server = crate::test_support::HTTP_SERVER_POOL.get_server();
        let root = TempDir::new().expect("tempdir");
        let path = root.path().join("input.rs");
        std::fs::write(&path, "fn old() {}\n").expect("fixture");
        let read = serde_json::json!({"choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":"reading","tool_calls":[{"id":"read","type":"function","function":{"name":"fs_read","arguments":serde_json::json!({"path":path}).to_string()}}]}}]});
        let partial = serde_json::json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"```rust\nfn candidate() {}\n```"}}]});
        server.expect(
            Expectation::matching(request::method_path("POST", "/v1/chat/completions"))
                .times(2)
                .respond_with(cycle![json_encoded(read), json_encoded(partial)]),
        );
        let mut cfg = exec_cfg_with_server(root.path(), &server);
        cfg.agent_budget.max_iterations = 1;
        let mut executor = Executor::new(cfg).await.expect("executor");
        let error = executor
            .run_rewrite("fix", "fn old() {}", None, true)
            .await
            .expect_err("partial rewrite");
        assert_eq!(
            error
                .downcast_ref::<IncompleteExecution>()
                .expect("typed")
                .0
                .stop_reason,
            Some(crate::llm::tool_execution::AgentStopReason::IterationBudget)
        );
        assert_eq!(
            std::fs::read_to_string(path).expect("fixture"),
            "fn old() {}\n"
        );
    }

    fn tool_invocation_msg(id: &str) -> ChatMessage {
        ChatMessage {
            provider_state: None,
            role: "assistant".into(),
            content: None,
            tool_calls: vec![ToolCall {
                id: Some(id.to_string()),
                r#type: "function".into(),
                function: ToolCallFunction {
                    name: "fs_read".to_string(),
                    arguments: "{}".to_string(),
                },
            }],
            tool_call_id: None,
        }
    }

    fn tool_result_msg(id: &str, content: &str) -> ChatMessage {
        ChatMessage {
            provider_state: None,
            role: "tool".into(),
            content: Some(content.to_string()),
            tool_calls: vec![],
            tool_call_id: Some(id.to_string()),
        }
    }

    /// Seed a session with fully typed messages (tool pairs, order kept).
    fn seed_typed_session(project_root: &Path, messages: &[ChatMessage]) -> String {
        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))
            .expect("Failed to create session store");
        let mut session = store.create().expect("Failed to create session");
        session
            .replace_conversation_messages(messages)
            .expect("Failed to seed conversation");
        store.save(&session).expect("Failed to save session");
        session.meta.id
    }

    fn assistant_done_response() -> serde_json::Value {
        serde_json::json!({
            "id": "test",
            "choices": [
                {"index": 0, "message": {"role": "assistant", "content": "done"}}
            ]
        })
    }

    fn exec_cfg_with_server(project_root: &Path, server: &httptest::Server) -> AppConfig {
        AppConfig {
            project_root: project_root.to_path_buf(),
            api_key: Some("test-key".to_string()),
            base_url: format!("{}/", server.url_str("")),
            model: "gpt-test".to_string(),
            no_repomap: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_exec_resume_restores_exact_history() {
        // A saved tool-call pair must reach the next turn in order, not just
        // as a bag containing one user message.
        let temp_dir = TempDir::new().expect("Failed to create temporary directory");
        let project_root = temp_dir.path().to_path_buf();
        let seeded = vec![
            exec_msg("user", Some("read the file")),
            tool_invocation_msg("call-a"),
            tool_result_msg("call-a", "file body"),
            exec_msg("assistant", Some("got it")),
        ];
        let seeded_id = seed_typed_session(&project_root, &seeded);

        let cfg = AppConfig {
            project_root: project_root.clone(),
            resume: Some(seeded_id),
            ..Default::default()
        };
        let executor = Executor::new(cfg).await.expect("Failed to create executor");
        let restored = executor.conversation_history.lock().await.snapshot();
        assert_eq!(restored.len(), 4, "exact history must be restored");
        assert_eq!(restored[0].content.as_deref(), Some("read the file"));
        assert_eq!(
            restored[1]
                .tool_calls
                .first()
                .and_then(|tc| tc.id.as_deref()),
            Some("call-a")
        );
        assert_eq!(restored[2].tool_call_id.as_deref(), Some("call-a"));
        assert_eq!(restored[2].content.as_deref(), Some("file body"));
        assert_eq!(restored[3].content.as_deref(), Some("got it"));
    }

    #[tokio::test]
    async fn test_exec_success_replaces_history_after_compaction_shape_change() {
        // The canonical result may be shorter than the input (compaction).
        // Committing must replace as-is; count-based deltas would drop it.
        let temp_dir = TempDir::new().expect("Failed to create temporary directory");
        let cfg = AppConfig {
            project_root: temp_dir.path().to_path_buf(),
            no_repomap: true,
            ..Default::default()
        };
        let executor = Executor::new(cfg).await.expect("Failed to create executor");
        {
            let mut history = executor.conversation_history.lock().await;
            history.replace(vec![
                exec_msg("user", Some("old 1")),
                exec_msg("assistant", Some("old 2")),
                tool_invocation_msg("call-old"),
                tool_result_msg("call-old", "old output"),
                exec_msg("user", Some("old 3")),
            ]);
        }
        // Simulated post-compaction canonical result: shorter, reordered.
        let compacted = vec![
            exec_msg("user", Some("summary of old work")),
            exec_msg("assistant", Some("fresh answer")),
        ];
        executor
            .commit_canonical_history(&compacted)
            .await
            .expect("commit");
        let committed = executor.conversation_history.lock().await.snapshot();
        assert_eq!(committed.len(), 2);
        assert_eq!(committed[0].content.as_deref(), Some("summary of old work"));
        assert_eq!(committed[1].content.as_deref(), Some("fresh answer"));
    }

    #[tokio::test]
    async fn test_exec_success_persists_updated_history_without_duplication() {
        use httptest::{Expectation, matchers::*, responders::*};
        let server = crate::test_support::HTTP_SERVER_POOL.get_server();
        server.expect(
            Expectation::matching(request::method_path("POST", "/v1/chat/completions"))
                .times(1)
                .respond_with(json_encoded(assistant_done_response())),
        );

        let temp_dir = TempDir::new().expect("Failed to create temporary directory");
        let project_root = temp_dir.path().to_path_buf();
        let cfg = exec_cfg_with_server(&project_root, &server);
        let mut executor = Executor::new(cfg).await.expect("Failed to create executor");
        executor
            .run("remember alpha", true)
            .await
            .expect("exec run");

        // In-memory canonical state: exactly one user message, no system
        // residue, no duplicated instruction.
        let snapshot = executor.conversation_history.lock().await.snapshot();
        assert!(
            snapshot.iter().all(|m| m.role != "system"),
            "request-scoped system messages must not persist"
        );
        let user_msgs: Vec<_> = snapshot.iter().filter(|m| m.role == "user").collect();
        assert_eq!(user_msgs.len(), 1, "current instruction must appear once");
        assert_eq!(user_msgs[0].content.as_deref(), Some("remember alpha"));
        assert!(
            snapshot
                .iter()
                .any(|m| m.role == "assistant" && m.content.as_deref() == Some("done"))
        );

        // Reopen the store: the updated conversation must be persisted.
        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))
            .expect("Failed to open store");
        let summaries = store.list_with_stats().expect("Failed to list sessions");
        assert_eq!(summaries.len(), 1);
        let saved = store
            .load(&summaries[0].meta.id)
            .expect("Failed to load session");
        let messages = saved.conversation_messages().expect("decode");
        assert_eq!(messages.len(), snapshot.len());
        assert_eq!(messages[0].content.as_deref(), Some("remember alpha"));
    }

    #[tokio::test]
    async fn test_exec_resume_continues_and_persists_second_turn() {
        use httptest::{Expectation, matchers::*, responders::*};
        let server = crate::test_support::HTTP_SERVER_POOL.get_server();
        // Two turns against one stub endpoint.
        server.expect(
            Expectation::matching(all_of![
                request::method_path("POST", "/v1/chat/completions"),
                request::body(matches("remember alpha")),
            ])
            .times(2)
            .respond_with(json_encoded(assistant_done_response())),
        );

        let temp_dir = TempDir::new().expect("Failed to create temporary directory");
        let project_root = temp_dir.path().to_path_buf();
        let mut first = Executor::new(exec_cfg_with_server(&project_root, &server))
            .await
            .expect("first executor");
        first.run("remember alpha", true).await.expect("first run");
        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))
            .expect("Failed to open store");
        let session_id = store.list_with_stats().expect("list").remove(0).meta.id;

        drop(first); // The previous process releases ownership before resume.
        // Second process resumes the same session: the first instruction
        // must be part of the resumed history and the request.
        let mut second = Executor::new(AppConfig {
            resume: Some(session_id.clone()),
            ..exec_cfg_with_server(&project_root, &server)
        })
        .await
        .expect("second executor");
        let resumed = second.conversation_history.lock().await.snapshot();
        assert!(
            resumed
                .iter()
                .any(|m| m.role == "user" && m.content.as_deref() == Some("remember alpha")),
            "resumed history must contain the first instruction"
        );
        second
            .run("what did I ask previously?", true)
            .await
            .expect("second run");

        let saved = store.load(&session_id).expect("reload");
        let messages = saved.conversation_messages().expect("decode");
        let users: Vec<_> = messages.iter().filter(|m| m.role == "user").collect();
        assert_eq!(users.len(), 2, "both turns persisted exactly once");
        assert_eq!(users[0].content.as_deref(), Some("remember alpha"));
        assert_eq!(
            users[1].content.as_deref(),
            Some("what did I ask previously?")
        );
    }
}
