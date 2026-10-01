use crate::llm::tool_runtime::ToolRuntime;
use crate::llm::types::ToolCall;
use crate::tools::tool_search::TOOL_SEARCH_TOOL_NAME;
use anyhow::{Result, anyhow};
use serde_json::json;
use tracing::debug;

mod analysis;
mod fs;
mod tools;

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub value: serde_json::Value,
    pub is_success: bool,
    pub result_summary: String,
}

pub async fn dispatch_tool_call(runtime: &ToolRuntime<'_>, call: &ToolCall) -> Result<ToolOutput> {
    debug!("dispatching tool call");
    if call.r#type != "function" {
        return Err(anyhow!("unsupported tool type: {}", call.r#type));
    }
    let name = call.function.name.as_str();
    let args_val: serde_json::Value = serde_json::from_str(&call.function.arguments)
        .map_err(|e| anyhow!("invalid tool args: {e}"))?;

    // Fail closed: a deferred tool must never execute from a guessed name.
    // The model has to discover and activate it via `tool_search` first, so
    // the capability becomes schema-visible before any side effect can run.
    if name != TOOL_SEARCH_TOOL_NAME
        && runtime.knows_tool(name)
        && !runtime.is_tool_active(name).await
    {
        let value = json!({
            "ok": false,
            "error": {
                "kind": "tool_not_active",
                "tool": name,
                "message": "This tool is deferred. Search for and activate the required capability with tool_search first.",
            },
            "warnings": [],
        });
        return Ok(ToolOutput {
            value: value.clone(),
            is_success: false,
            result_summary: format!("Tool '{name}' is deferred; use tool_search first"),
        });
    }

    dispatch_inner(runtime, name, &args_val).await
}

/// Sub-agent dispatch: the read-only allowlist is enforced by the sub-agent
/// loop itself, so the main-run deferred gate does not apply here. This keeps
/// sub-agent research working without leaking its tools into the main agent's
/// active set.
pub async fn dispatch_subagent_tool_call(
    runtime: &ToolRuntime<'_>,
    call: &ToolCall,
) -> Result<ToolOutput> {
    debug!("dispatching sub-agent tool call");
    if call.r#type != "function" {
        return Err(anyhow!("unsupported tool type: {}", call.r#type));
    }
    let name = call.function.name.as_str();
    let args_val: serde_json::Value = serde_json::from_str(&call.function.arguments)
        .map_err(|e| anyhow!("invalid tool args: {e}"))?;
    dispatch_inner(runtime, name, &args_val).await
}

async fn dispatch_inner(
    runtime: &ToolRuntime<'_>,
    name: &str,
    args_val: &serde_json::Value,
) -> Result<ToolOutput> {
    // Each subsystem owns its own timeout. In particular, do not wrap every
    // tool in a command-derived global timeout: command_timeout_ms = 0 means
    // unlimited for managed finite commands and must not become a hidden
    // 125-second dispatcher limit here.
    match name {
        // FS-related
        "fs_list" => fs::fs_list(runtime, args_val).await,
        "fs_read" => fs::fs_read(runtime, args_val).await,
        "search_text" => fs::search_text(runtime, args_val).await,
        "fs_write" => fs::fs_write(runtime, args_val).await,
        "find_file" => fs::find_file(runtime, args_val).await,
        "fs_read_many_files" => fs::fs_read_many_files(runtime, args_val).await,

        // Analysis / repomap
        "search_repomap" => analysis::search_repomap(runtime, args_val).await,

        // Tools and helpers
        "execute_process" => tools::execute_process(runtime, args_val).await,
        "execute_bash" => tools::execute_bash(runtime, args_val).await,
        "execute_shell" => tools::execute_shell(runtime, args_val).await,
        "edit" => tools::edit(runtime, args_val).await,
        "apply_patch" => tools::apply_patch(runtime, args_val).await,
        "task" => tools::task(runtime, args_val).await,
        "plan_write" => tools::plan_write(runtime, args_val).await,
        "plan_read" => tools::plan_read(runtime, args_val).await,
        "provenance_read" => tools::provenance_read(runtime, args_val).await,
        "requirements_write" => tools::requirements_write(runtime, args_val).await,
        "requirements_read" => tools::requirements_read(runtime, args_val).await,
        "undo" => tools::undo(runtime, args_val).await,
        "read_memory" => tools::read_memory(runtime, args_val).await,
        "write_memory" => tools::write_memory(runtime, args_val).await,
        "list_memories" => tools::list_memories(runtime, args_val).await,
        "search_memory" => tools::search_memory(runtime, args_val).await,
        "run_workflow" => tools::run_workflow(runtime, args_val).await,
        "doc_generate" => tools::doc_generate(runtime, args_val).await,
        "search_history" => tools::search_history(runtime, args_val).await,
        "tool_search" => tools::tool_search(runtime, args_val).await,

        other => {
            if let Some(outcome) = runtime
                .fs
                .call_remote_tool(other, args_val, runtime.cancel_token.clone())
                .await?
            {
                Ok(ToolOutput {
                    value: outcome.value,
                    // MCP transport success is not tool success. The remote
                    // normalizer derives this from CallToolResult.is_error.
                    is_success: outcome.success,
                    result_summary: outcome.summary,
                })
            } else {
                Err(anyhow!("unknown tool: {other}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, McpServerConfig, McpTransport};
    use crate::llm::types::ToolCallFunction;
    use crate::tools::FsTools;
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::tempdir;
    use tokio::sync::RwLock;
    use tokio_util::sync::CancellationToken;

    /// Behavior tests target tool semantics, not routing: activate the named
    /// deferred tools first (mirrors a prior `tool_search` call).
    async fn activate_test_tools(runtime: &ToolRuntime<'_>, names: &[&str]) {
        let owned: Vec<String> = names.iter().map(|s| (*s).to_string()).collect();
        runtime.tool_catalog.activate(&owned).await;
    }

    #[tokio::test]
    async fn test_remote_tool_error_preserves_failure_flag() -> Result<()> {
        let dir = tempdir()?;
        let app = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            ..AppConfig::default()
        });
        let handle = crate::mcp::server::spawn_mcp_server(
            "127.0.0.1:0",
            app.clone(),
            Arc::new(RwLock::new(None)),
        )
        .await?;
        let mut config = (*app).clone();
        config.mcp_servers = vec![McpServerConfig {
            name: "local".to_string(),
            enabled: true,
            address: Some(format!("http://{}/mcp", handle.local_addr())),
            transport: McpTransport::Http,
            ..McpServerConfig::default()
        }];
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(config));
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;
        activate_test_tools(&runtime, &["mcp_local_fs_read"]).await;
        let call = ToolCall {
            id: Some("remote_error".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "mcp_local_fs_read".to_string(),
                arguments: json!({
                    "path": dir.path().join("missing.txt").to_string_lossy()
                })
                .to_string(),
            },
        };

        let output = dispatch_tool_call(&runtime, &call).await?;
        assert!(!output.is_success);
        assert_eq!(output.value["ok"], false);
        assert_eq!(output.value["is_error"], true);
        handle.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_bash_failure_flag() -> Result<()> {
        let dir = tempdir()?;
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            ..AppConfig::default()
        });
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), config);
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;
        activate_test_tools(&runtime, &["execute_bash"]).await;

        let tool_call = ToolCall {
            id: Some("call_1".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "execute_bash".to_string(),
                arguments: json!({
                    "command": "exit 1"
                })
                .to_string(),
            },
        };

        let output = dispatch_tool_call(&runtime, &tool_call).await?;
        assert!(
            !output.is_success,
            "execute_bash(exit 1) should be marked as failure"
        );
        assert_eq!(output.value["exit_code"], 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_bash_success_flag() -> Result<()> {
        let dir = tempdir()?;
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            ..AppConfig::default()
        });
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), config);
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;
        activate_test_tools(&runtime, &["execute_bash"]).await;

        let tool_call = ToolCall {
            id: Some("call_2".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "execute_bash".to_string(),
                arguments: json!({
                    "command": "echo hello"
                })
                .to_string(),
            },
        };

        let output = dispatch_tool_call(&runtime, &tool_call).await?;
        assert!(
            output.is_success,
            "execute_bash(echo hello) should be marked as success"
        );
        assert_eq!(output.value["exit_code"], 0);
        Ok(())
    }

    #[tokio::test]
    async fn test_zero_command_timeout_does_not_add_dispatch_timeout() -> Result<()> {
        let dir = tempdir()?;
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            command_timeout_ms: 0,
            ..AppConfig::default()
        });
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), config);
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;
        let tool_call = ToolCall {
            id: Some("call_zero_timeout".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "execute_process".to_string(),
                arguments: json!({ "program": "printf", "args": ["ok"] }).to_string(),
            },
        };
        let output = dispatch_tool_call(&runtime, &tool_call).await?;
        assert!(output.is_success);
        assert_eq!(output.value["status"], "completed");
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_process_request_timeout_still_applies() -> Result<()> {
        let dir = tempdir()?;
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            command_timeout_ms: 0,
            ..AppConfig::default()
        });
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), config);
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;
        let tool_call = ToolCall {
            id: Some("call_request_timeout".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "execute_process".to_string(),
                arguments: json!({
                    "program": "sleep",
                    "args": ["30"],
                    "timeout_ms": 50
                })
                .to_string(),
            },
        };
        let output = dispatch_tool_call(&runtime, &tool_call).await?;
        assert!(!output.is_success);
        assert_eq!(output.value["status"], "timed_out");
        assert!(output.value["exit_code"].is_null());
        Ok(())
    }

    #[tokio::test]
    async fn test_dispatch_preserves_typed_cancellation() -> Result<()> {
        let dir = tempdir()?;
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            ..AppConfig::default()
        });
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), config);
        let token = CancellationToken::new();
        token.cancel();
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", Some(token)).await?;
        activate_test_tools(&runtime, &["execute_bash"]).await;
        let tool_call = ToolCall {
            id: Some("call_cancelled".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "execute_bash".to_string(),
                arguments: json!({ "command": "echo should-not-run" }).to_string(),
            },
        };

        let error = dispatch_tool_call(&runtime, &tool_call)
            .await
            .expect_err("cancelled execution should fail");
        assert!(error.downcast_ref::<crate::llm::LlmErrorKind>().is_some());
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_shell_success_flag() -> Result<()> {
        let dir = tempdir()?;
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            ..AppConfig::default()
        });
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), config);
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;
        activate_test_tools(&runtime, &["execute_shell"]).await;

        let tool_call = ToolCall {
            id: Some("call_3".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "execute_shell".to_string(),
                arguments: json!({
                    "command": "echo shell-ok"
                })
                .to_string(),
            },
        };

        let output = dispatch_tool_call(&runtime, &tool_call).await?;
        assert!(
            output.is_success,
            "execute_shell(echo shell-ok) should be marked as success"
        );
        assert_eq!(output.value["exit_code"], 0);
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_shell_failure_flag() -> Result<()> {
        let dir = tempdir()?;
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            ..AppConfig::default()
        });
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), config);
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;
        activate_test_tools(&runtime, &["execute_shell"]).await;

        let tool_call = ToolCall {
            id: Some("call_4".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "execute_shell".to_string(),
                arguments: json!({
                    "command": "nonexistent_doge_command_xyz"
                })
                .to_string(),
            },
        };

        let output = dispatch_tool_call(&runtime, &tool_call).await?;
        assert!(
            !output.is_success,
            "execute_shell(nonexistent command) should be marked as failure"
        );
        assert_ne!(output.value["exit_code"], 0);
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_process_is_registered() {
        let names: Vec<String> = crate::llm::tool_def::default_tools_def()
            .iter()
            .map(|def| def.function.name.clone())
            .collect();
        assert!(
            names.contains(&"execute_process".to_string()),
            "tools: {names:?}"
        );
    }

    #[tokio::test]
    async fn test_execute_process_success_flag() -> Result<()> {
        let dir = tempdir()?;
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            ..AppConfig::default()
        });
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), config);
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;

        let tool_call = ToolCall {
            id: Some("call_proc_ok".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "execute_process".to_string(),
                arguments: json!({ "program": "echo", "args": ["hello"] }).to_string(),
            },
        };

        let output = dispatch_tool_call(&runtime, &tool_call).await?;
        assert!(output.is_success, "execute_process(echo) should succeed");
        assert_eq!(output.value["ok"], true);
        assert_eq!(output.value["success"], true);
        assert_eq!(output.value["status"], "completed");
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_process_failure_ok_mirrors_success() -> Result<()> {
        let dir = tempdir()?;
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            ..AppConfig::default()
        });
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), config);
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;

        let tool_call = ToolCall {
            id: Some("call_proc_fail".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "execute_process".to_string(),
                arguments: json!({ "program": "bash", "args": ["-c", "exit 3"] }).to_string(),
            },
        };

        let output = dispatch_tool_call(&runtime, &tool_call).await?;
        assert!(!output.is_success);
        assert_eq!(output.value["ok"], false);
        assert_eq!(output.value["success"], false);
        assert_eq!(output.value["status"], "completed");
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_process_policy_denied() -> Result<()> {
        use crate::config::{ExecutionConfig, ExecutionMode};
        let dir = tempdir()?;
        let exec = ExecutionConfig {
            mode: ExecutionMode::Allowlist,
            allowed_programs: vec!["cargo".to_string()],
            allow_shell: false,
            ..ExecutionConfig::default()
        };
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            execution: exec,
            execution_configured: true,
            ..AppConfig::default()
        });
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), config);
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;

        let tool_call = ToolCall {
            id: Some("call_proc_deny".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "execute_process".to_string(),
                arguments: json!({ "program": "git", "args": ["status"] }).to_string(),
            },
        };

        let output = dispatch_tool_call(&runtime, &tool_call).await?;
        assert!(!output.is_success);
        assert_eq!(output.value["ok"], false);
        assert_eq!(output.value["status"], "policy_denied");
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_bash_ok_mirrors_success() -> Result<()> {
        let dir = tempdir()?;
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            ..AppConfig::default()
        });
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), config);
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;
        activate_test_tools(&runtime, &["execute_bash"]).await;

        let tool_call = ToolCall {
            id: Some("call_bash_ok".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "execute_bash".to_string(),
                arguments: json!({ "command": "exit 1" }).to_string(),
            },
        };

        let output = dispatch_tool_call(&runtime, &tool_call).await?;
        assert!(!output.is_success);
        assert_eq!(output.value["ok"], false);
        assert_eq!(output.value["success"], false);
        Ok(())
    }

    #[tokio::test]
    async fn test_shell_disabled_denies_bash_and_shell() -> Result<()> {
        use crate::config::{ExecutionConfig, ExecutionMode};
        let dir = tempdir()?;
        let exec = ExecutionConfig {
            mode: ExecutionMode::Allowlist,
            allowed_programs: vec!["cargo".to_string()],
            allow_shell: false,
            ..ExecutionConfig::default()
        };
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            execution: exec,
            execution_configured: true,
            ..AppConfig::default()
        });
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), config);
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;
        activate_test_tools(&runtime, &["execute_bash", "execute_shell"]).await;

        for name in ["execute_bash", "execute_shell"] {
            let args = json!({ "command": "echo hi" }).to_string();
            let tool_call = ToolCall {
                id: Some(format!("call_{name}")),
                r#type: "function".to_string(),
                function: ToolCallFunction {
                    name: name.to_string(),
                    arguments: args,
                },
            };
            let output = dispatch_tool_call(&runtime, &tool_call).await?;
            assert!(!output.is_success, "{name} should be denied");
            assert_eq!(output.value["ok"], false);
            assert_eq!(output.value["success"], false);
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_task_tool_is_registered() {
        // The `task` tool must be registered in default_tools_def so the LLM
        // can call it (guards against the registration-gap pitfall).
        let names: Vec<String> = crate::llm::tool_def::default_tools_def()
            .iter()
            .map(|def| def.function.name.clone())
            .collect();
        assert!(names.contains(&"task".to_string()), "tools: {names:?}");
    }

    #[tokio::test]
    async fn test_task_tool_requires_client() -> Result<()> {
        // Dispatch resolves the `task` arm; without a client it must fail
        // with a clear error instead of falling through to "unknown tool".
        let dir = tempdir()?;
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            ..AppConfig::default()
        });
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), config);
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;

        let tool_call = ToolCall {
            id: Some("call_task".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "task".to_string(),
                arguments: json!({
                    "description": "explore",
                    "prompt": "list the modules"
                })
                .to_string(),
            },
        };

        let err = dispatch_tool_call(&runtime, &tool_call)
            .await
            .expect_err("task without client should fail");
        assert!(
            err.to_string().contains("LLM client is not configured"),
            "unexpected error: {err}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_task_tool_validates_args() -> Result<()> {
        let dir = tempdir()?;
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            ..AppConfig::default()
        });
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), config);
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;

        let tool_call = ToolCall {
            id: Some("call_task_bad".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "task".to_string(),
                arguments: json!({ "description": "missing prompt" }).to_string(),
            },
        };

        let err = dispatch_tool_call(&runtime, &tool_call)
            .await
            .expect_err("task without prompt should fail");
        assert!(
            err.to_string().contains("prompt") || err.to_string().contains("invalid"),
            "unexpected error: {err}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_provenance_read_is_registered() {
        let names: Vec<String> = crate::llm::tool_def::default_tools_def()
            .iter()
            .map(|def| def.function.name.clone())
            .collect();
        assert!(
            names.contains(&"provenance_read".to_string()),
            "tools: {names:?}"
        );
    }

    #[tokio::test]
    async fn test_requirements_tools_are_registered() {
        let names: Vec<String> = crate::llm::tool_def::default_tools_def()
            .iter()
            .map(|def| def.function.name.clone())
            .collect();
        assert!(
            names.contains(&"requirements_write".to_string()),
            "tools: {names:?}"
        );
        assert!(
            names.contains(&"requirements_read".to_string()),
            "tools: {names:?}"
        );
    }

    #[tokio::test]
    async fn test_requirements_write_needs_directive_via_dispatch() -> Result<()> {
        let dir = tempdir()?;
        let project_root = dir.path().to_path_buf();
        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
        let manager = Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            store,
            current_session: None,
        }));
        {
            let mut mgr = manager.lock().unwrap();
            mgr.create_session(None)?;
        }
        let config = Arc::new(AppConfig {
            project_root: project_root.clone(),
            ..AppConfig::default()
        });
        let fs_tools =
            FsTools::new(Arc::new(RwLock::new(None)), config).with_session_manager(manager);
        // No directive attribution: must fail without creating requirements.
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;
        activate_test_tools(&runtime, &["requirements_write", "requirements_read"]).await;
        let tool_call = ToolCall {
            id: Some("call_req".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "requirements_write".to_string(),
                arguments: json!({
                    "upserts": [{"id": "r1", "statement": "do things"}]
                })
                .to_string(),
            },
        };
        let output = dispatch_tool_call(&runtime, &tool_call).await?;
        assert!(!output.is_success);
        assert!(
            output
                .result_summary
                .contains("without an observed directive")
                || output
                    .value
                    .to_string()
                    .contains("without an observed directive")
        );
        // With a directive, the same call succeeds.
        let directive = crate::tools::provenance::record_directive_observed(
            &fs_tools,
            crate::provenance::DirectiveOrigin::TuiPrompt,
            "do things",
            "do things",
        )?;
        let runtime2 = ToolRuntime::build_with_attribution(
            &fs_tools,
            None,
            "test-model",
            None,
            crate::provenance::ProvenanceAttribution::with_directive(directive.event_id),
        )
        .await?;
        activate_test_tools(&runtime2, &["requirements_write", "requirements_read"]).await;
        let output2 = dispatch_tool_call(&runtime2, &tool_call).await?;
        assert!(output2.is_success, "value: {}", output2.value);
        // requirements_read via dispatch returns coverage.
        let read_call = ToolCall {
            id: Some("call_req_read".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "requirements_read".to_string(),
                arguments: json!({}).to_string(),
            },
        };
        let read_out = dispatch_tool_call(&runtime2, &read_call).await?;
        assert!(read_out.is_success);
        assert!(read_out.value.get("requirements").is_some());
        Ok(())
    }

    #[tokio::test]
    async fn test_provenance_read_dispatch_returns_coverage() -> Result<()> {
        let dir = tempdir()?;
        let project_root = dir.path().to_path_buf();
        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
        let manager = Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            store,
            current_session: None,
        }));
        {
            let mut mgr = manager.lock().unwrap();
            mgr.create_session(None)?;
        }
        let config = Arc::new(AppConfig {
            project_root: project_root.clone(),
            ..AppConfig::default()
        });
        let fs_tools =
            FsTools::new(Arc::new(RwLock::new(None)), config).with_session_manager(manager);
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;
        activate_test_tools(&runtime, &["provenance_read"]).await;

        let tool_call = ToolCall {
            id: Some("call_prov".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "provenance_read".to_string(),
                arguments: json!({}).to_string(),
            },
        };
        let output = dispatch_tool_call(&runtime, &tool_call).await?;
        assert!(output.is_success);
        assert!(output.value.get("events").is_some());
        assert!(output.value.get("coverage").is_some());
        assert!(output.value.get("next_cursor").is_some());
        Ok(())
    }

    #[tokio::test]
    async fn test_edit_dispatch_records_text_edit_and_undo() -> Result<()> {
        let dir = tempdir()?;
        let project_root = dir.path().to_path_buf();
        let target = project_root.join("a.txt");
        std::fs::write(&target, "hello\n")?;
        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
        let manager = Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            store,
            current_session: None,
        }));
        {
            let mut mgr = manager.lock().unwrap();
            mgr.create_session(None)?;
        }
        let session_id = manager.lock().unwrap().current_session_id().unwrap();
        let session_dir = project_root.join(".doge/sessions").join(&session_id);
        let config = Arc::new(AppConfig {
            project_root: project_root.clone(),
            ..AppConfig::default()
        });
        let fs_tools =
            FsTools::new(Arc::new(RwLock::new(None)), config).with_session_manager(manager);
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;
        activate_test_tools(&runtime, &["edit", "undo"]).await;

        let tool_call = ToolCall {
            id: Some("call_edit".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "edit".to_string(),
                arguments: json!({
                    "file_path": target.to_str().unwrap(),
                    "target_block": "hello",
                    "new_block": "goodbye",
                })
                .to_string(),
            },
        };
        let output = dispatch_tool_call(&runtime, &tool_call).await?;
        assert!(output.is_success, "value: {}", output.value);
        assert_eq!(output.value["changed"], true);
        assert_eq!(std::fs::read_to_string(&target)?, "goodbye\n");

        let store = crate::provenance::ProvenanceStore::new(session_dir.clone());
        let loaded = store.load_all()?;
        assert_eq!(loaded.events.len(), 1);
        match &loaded.events[0].event {
            crate::provenance::ProvenanceEvent::ChangeCommitted(c) => {
                assert_eq!(c.change_kind, crate::provenance::ChangeKind::TextEdit);
            }
            _ => panic!("expected change"),
        }

        // Undo via dispatch restores and records an Undo event.
        let undo_call = ToolCall {
            id: Some("call_undo".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "undo".to_string(),
                arguments: json!({}).to_string(),
            },
        };
        let undo_out = dispatch_tool_call(&runtime, &undo_call).await?;
        assert!(undo_out.is_success, "value: {}", undo_out.value);
        assert_eq!(undo_out.value["changed"], true);
        assert_eq!(std::fs::read_to_string(&target)?, "hello\n");
        let loaded = store.load_all()?;
        assert_eq!(loaded.events.len(), 2);

        // No-op edit records nothing further.
        let noop_call = ToolCall {
            id: Some("call_noop".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "edit".to_string(),
                arguments: json!({
                    "file_path": target.to_str().unwrap(),
                    "target_block": "hello",
                    "new_block": "hello",
                })
                .to_string(),
            },
        };
        let noop_out = dispatch_tool_call(&runtime, &noop_call).await?;
        assert!(noop_out.is_success);
        assert_eq!(noop_out.value["changed"], false);
        let loaded = store.load_all()?;
        assert_eq!(loaded.events.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_process_records_verification_observed() -> Result<()> {
        let dir = tempdir()?;
        let project_root = dir.path().to_path_buf();
        std::fs::write(project_root.join("dummy.py"), "x = 1\n")?;
        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
        let manager = Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            store,
            current_session: None,
        }));
        {
            let mut mgr = manager.lock().unwrap();
            mgr.create_session(None)?;
        }
        let session_id = manager.lock().unwrap().current_session_id().unwrap();
        let session_dir = project_root.join(".doge/sessions").join(&session_id);
        let config = Arc::new(AppConfig {
            project_root: project_root.clone(),
            ..AppConfig::default()
        });
        let fs_tools =
            FsTools::new(Arc::new(RwLock::new(None)), config).with_session_manager(manager);
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;

        let tool_call = ToolCall {
            id: Some("call_verify".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "execute_process".to_string(),
                arguments: json!({
                    "program": "python3",
                    "args": ["-m", "py_compile", "dummy.py"]
                })
                .to_string(),
            },
        };
        let output = dispatch_tool_call(&runtime, &tool_call).await?;
        assert!(output.is_success, "value: {}", output.value);

        let store = crate::provenance::ProvenanceStore::new(session_dir);
        let loaded = store.load_all()?;
        let verifications: Vec<_> = loaded
            .events
            .iter()
            .filter(|e| {
                matches!(
                    e.event,
                    crate::provenance::ProvenanceEvent::VerificationObserved(_)
                )
            })
            .collect();
        assert_eq!(verifications.len(), 1);
        if let crate::provenance::ProvenanceEvent::VerificationObserved(v) = &verifications[0].event
        {
            assert_eq!(
                v.verification_kind,
                crate::provenance::VerificationKind::SyntaxCheck
            );
            assert_eq!(
                v.source,
                crate::provenance::VerificationSource::ExecuteProcess
            );
            assert!(v.outcome.success);
            assert_eq!(v.command.program, "python3");
            assert!(!format!("{:?}", v).contains("API_KEY"));
            assert!(v.output_digest.starts_with("blake3:"));
        } else {
            panic!("expected verification");
        }
        // Raw event JSON must not contain environment values.
        for env in loaded.events {
            let raw = serde_json::to_string(&env).unwrap();
            assert!(!raw.contains("OPENAI_API_KEY"));
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_process_policy_denied_records_nothing() -> Result<()> {
        use crate::config::{ExecutionConfig, ExecutionMode};
        let dir = tempdir()?;
        let project_root = dir.path().to_path_buf();
        let exec = ExecutionConfig {
            mode: ExecutionMode::Allowlist,
            allowed_programs: vec!["cargo".to_string()],
            allow_shell: false,
            ..ExecutionConfig::default()
        };
        let store = crate::session::SessionStore::new(project_root.join(".doge/sessions"))?;
        let manager = Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            store,
            current_session: None,
        }));
        {
            let mut mgr = manager.lock().unwrap();
            mgr.create_session(None)?;
        }
        let session_id = manager.lock().unwrap().current_session_id().unwrap();
        let session_dir = project_root.join(".doge/sessions").join(&session_id);
        let config = Arc::new(AppConfig {
            project_root: project_root.clone(),
            execution: exec,
            execution_configured: true,
            ..AppConfig::default()
        });
        let fs_tools =
            FsTools::new(Arc::new(RwLock::new(None)), config).with_session_manager(manager);
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;
        let tool_call = ToolCall {
            id: Some("call_deny".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "execute_process".to_string(),
                arguments: json!({"program": "python3", "args": ["-m", "py_compile", "x.py"]})
                    .to_string(),
            },
        };
        let output = dispatch_tool_call(&runtime, &tool_call).await?;
        assert!(!output.is_success);
        assert_eq!(output.value["status"], "policy_denied");
        let store = crate::provenance::ProvenanceStore::new(session_dir);
        let loaded = store.load_all()?;
        assert!(loaded.events.is_empty());
        Ok(())
    }

    fn deferred_test_runtime(fs_tools: &FsTools) -> ToolRuntime<'_> {
        let catalog = crate::llm::tool_catalog::ToolCatalog::from_parts(
            crate::llm::tool_def::default_tools_def(),
            &[],
            &crate::config::ToolRoutingConfig {
                mode: crate::config::ToolRoutingMode::Deferred,
                search_result_limit: 5,
            },
        );
        ToolRuntime::from_catalog_for_test(fs_tools, catalog)
    }

    fn deferred_test_fs() -> (tempfile::TempDir, FsTools) {
        // Keep the TempDir alive by returning it alongside FsTools.
        let dir = tempdir().expect("tempdir");
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            ..AppConfig::default()
        });
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), config);
        (dir, fs_tools)
    }

    #[tokio::test]
    async fn test_deferred_inactive_builtin_fails_closed() -> Result<()> {
        let (_dir, fs_tools) = deferred_test_fs();
        let runtime = deferred_test_runtime(&fs_tools);
        assert!(!runtime.is_tool_active("apply_patch").await);
        assert!(runtime.knows_tool("apply_patch"));

        let before = "original\n";
        let target = fs_tools.config.project_root.join("deferred.txt");
        std::fs::write(&target, before)?;
        let tool_call = ToolCall {
            id: Some("call_deferred_edit".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "apply_patch".to_string(),
                arguments: json!({
                    "file_path": target.to_str().unwrap(),
                    "patch": "--- a\n+++ b\n",
                })
                .to_string(),
            },
        };
        let output = dispatch_tool_call(&runtime, &tool_call).await?;
        assert!(!output.is_success);
        assert_eq!(output.value["ok"], false);
        assert_eq!(output.value["error"]["kind"], "tool_not_active");
        assert_eq!(output.value["error"]["tool"], "apply_patch");
        // Fail-closed: no side effect happened.
        assert_eq!(std::fs::read_to_string(&target)?, before);
        Ok(())
    }

    #[tokio::test]
    async fn test_tool_search_activates_then_dispatch_succeeds() -> Result<()> {
        let (_dir, fs_tools) = deferred_test_fs();
        let runtime = deferred_test_runtime(&fs_tools);
        assert!(!runtime.is_tool_active("edit").await);

        let search_call = ToolCall {
            id: Some("call_search".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "tool_search".to_string(),
                arguments: json!({"query": "surgical text edit file block"}).to_string(),
            },
        };
        let search_out = dispatch_tool_call(&runtime, &search_call).await?;
        assert!(search_out.is_success, "value: {}", search_out.value);
        assert_eq!(search_out.value["ok"], true);
        // Full schemas are never echoed in search results.
        let rendered = search_out.value.to_string();
        assert!(!rendered.contains("\"parameters\""));
        assert!(runtime.is_tool_active("edit").await);

        // Activation is sticky and dispatch now executes the tool.
        let target = fs_tools.config.project_root.join("activated.txt");
        std::fs::write(&target, "hello\n")?;
        let edit_call = ToolCall {
            id: Some("call_edit_after_search".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "edit".to_string(),
                arguments: json!({
                    "file_path": target.to_str().unwrap(),
                    "target_block": "hello",
                    "new_block": "goodbye",
                })
                .to_string(),
            },
        };
        let edit_out = dispatch_tool_call(&runtime, &edit_call).await?;
        assert!(edit_out.is_success, "value: {}", edit_out.value);
        assert_eq!(std::fs::read_to_string(&target)?, "goodbye\n");
        Ok(())
    }

    #[tokio::test]
    async fn test_tool_search_reports_already_active() -> Result<()> {
        let (_dir, fs_tools) = deferred_test_fs();
        let runtime = deferred_test_runtime(&fs_tools);
        let search_call = ToolCall {
            id: Some("call_search_twice".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "tool_search".to_string(),
                arguments: json!({"query": "surgical text edit file block"}).to_string(),
            },
        };
        let first = dispatch_tool_call(&runtime, &search_call).await?;
        assert!(first.is_success);
        assert!(
            first.value["activated"]
                .as_array()
                .is_some_and(|a| !a.is_empty()),
            "first search should activate, value: {}",
            first.value
        );
        // Second identical search: nothing new to activate; matches surface
        // as already_active instead of duplicating the active set.
        let before = runtime.tool_catalog.active_count().await;
        let second = dispatch_tool_call(&runtime, &search_call).await?;
        assert!(second.is_success);
        assert_eq!(second.value["activated"].as_array().map(Vec::len), Some(0));
        assert!(
            second.value["already_active"]
                .as_array()
                .is_some_and(|a| !a.is_empty()),
            "value: {}",
            second.value
        );
        assert_eq!(runtime.tool_catalog.active_count().await, before);
        Ok(())
    }

    #[tokio::test]
    async fn test_tool_search_empty_query_is_invalid() -> Result<()> {
        let (_dir, fs_tools) = deferred_test_fs();
        let runtime = deferred_test_runtime(&fs_tools);
        let search_call = ToolCall {
            id: Some("call_empty_search".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "tool_search".to_string(),
                arguments: json!({"query": "   "}).to_string(),
            },
        };
        let output = dispatch_tool_call(&runtime, &search_call).await?;
        assert!(!output.is_success);
        assert_eq!(output.value["error"]["kind"], "invalid_query");
        Ok(())
    }

    #[tokio::test]
    async fn test_inactive_remote_tool_never_reaches_mcp() -> Result<()> {
        let (_dir, fs_tools) = deferred_test_fs();
        // A catalog-only remote entry with no backing MCP server: the
        // fail-closed gate must reject before any transport is touched.
        let remote_def = crate::llm::types::ToolDef {
            kind: "function".into(),
            function: crate::llm::types::ToolFunctionDef {
                name: "mcp_github_create_issue".to_string(),
                description: "Create a GitHub issue".to_string(),
                parameters: json!({"type": "object", "properties": {}}),
                strict: None,
            },
        };
        let entry = crate::llm::tool_catalog::ToolCatalogEntry {
            definition: remote_def,
            source: crate::llm::tool_catalog::ToolSource::RemoteMcp {
                server_name: "github".to_string(),
                remote_name: "create_issue".to_string(),
            },
            searchable_text: "mcp_github_create_issue create a github issue github create_issue"
                .to_string(),
        };
        let catalog = crate::llm::tool_catalog::ToolCatalog::from_entries(
            vec![entry],
            &crate::config::ToolRoutingConfig {
                mode: crate::config::ToolRoutingMode::Deferred,
                search_result_limit: 5,
            },
        );
        let runtime = ToolRuntime::from_catalog_for_test(&fs_tools, catalog);
        assert!(runtime.knows_tool("mcp_github_create_issue"));
        assert!(!runtime.is_tool_active("mcp_github_create_issue").await);

        let tool_call = ToolCall {
            id: Some("call_remote_guessed".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "mcp_github_create_issue".to_string(),
                arguments: json!({"title": "hi"}).to_string(),
            },
        };
        let output = dispatch_tool_call(&runtime, &tool_call).await?;
        assert!(!output.is_success);
        assert_eq!(output.value["error"]["kind"], "tool_not_active");
        // The MCP normalizer always sets these fields; their absence proves
        // the remote path was never invoked (no side effect possible).
        assert!(output.value.get("is_error").is_none());
        assert!(output.value.get("server").is_none());

        // Unknown tools stay unknown (not deferred).
        let unknown_call = ToolCall {
            id: Some("call_unknown".to_string()),
            r#type: "function".to_string(),
            function: ToolCallFunction {
                name: "no_such_tool_xyz".to_string(),
                arguments: json!({}).to_string(),
            },
        };
        let err = dispatch_tool_call(&runtime, &unknown_call)
            .await
            .expect_err("unknown tool should error");
        assert!(err.to_string().contains("unknown tool"));
        Ok(())
    }

    #[tokio::test]
    async fn test_eager_mode_matches_legacy_inventory() -> Result<()> {
        // Eager mode is the escape hatch: every built-in plus every remote
        // tool is visible, and `tool_search` adds no extra schema.
        let all_builtin = crate::llm::tool_def::default_tools_def();
        let catalog = crate::llm::tool_catalog::ToolCatalog::from_parts(
            all_builtin.clone(),
            &[],
            &crate::config::ToolRoutingConfig {
                mode: crate::config::ToolRoutingMode::Eager,
                search_result_limit: 5,
            },
        );
        assert!(!catalog.is_deferred());
        let active = catalog.active_tool_defs().await;
        let mut active_names: Vec<String> =
            active.iter().map(|d| d.function.name.clone()).collect();
        active_names.sort();
        let mut legacy_names: Vec<String> = all_builtin
            .iter()
            .map(|d| d.function.name.clone())
            .collect();
        legacy_names.sort();
        assert_eq!(active_names, legacy_names);
        assert!(
            !catalog.is_active("tool_search").await,
            "eager mode must not advertise tool_search"
        );
        Ok(())
    }
}
