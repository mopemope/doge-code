use crate::llm::tool_runtime::ToolRuntime;
use crate::llm::types::ToolCall;
use anyhow::{Result, anyhow};
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

    // Each subsystem owns its own timeout. In particular, do not wrap every
    // tool in a command-derived global timeout: command_timeout_ms = 0 means
    // unlimited for managed finite commands and must not become a hidden
    // 125-second dispatcher limit here.
    match name {
        // FS-related
        "fs_list" => fs::fs_list(runtime, &args_val).await,
        "fs_read" => fs::fs_read(runtime, &args_val).await,
        "search_text" => fs::search_text(runtime, &args_val).await,
        "fs_write" => fs::fs_write(runtime, &args_val).await,
        "find_file" => fs::find_file(runtime, &args_val).await,
        "fs_read_many_files" => fs::fs_read_many_files(runtime, &args_val).await,

        // Analysis / repomap
        "search_repomap" => analysis::search_repomap(runtime, &args_val).await,

        // Tools and helpers
        "execute_process" => tools::execute_process(runtime, &args_val).await,
        "execute_bash" => tools::execute_bash(runtime, &args_val).await,
        "execute_shell" => tools::execute_shell(runtime, &args_val).await,
        "edit" => tools::edit(runtime, &args_val).await,
        "apply_patch" => tools::apply_patch(runtime, &args_val).await,
        "task" => tools::task(runtime, &args_val).await,
        "plan_write" => tools::plan_write(runtime, &args_val).await,
        "plan_read" => tools::plan_read(runtime, &args_val).await,
        "undo" => tools::undo(runtime, &args_val).await,
        "read_memory" => tools::read_memory(runtime, &args_val).await,
        "write_memory" => tools::write_memory(runtime, &args_val).await,
        "list_memories" => tools::list_memories(runtime, &args_val).await,
        "search_memory" => tools::search_memory(runtime, &args_val).await,
        "run_workflow" => tools::run_workflow(runtime, &args_val).await,
        "doc_generate" => tools::doc_generate(runtime, &args_val).await,
        "search_history" => tools::search_history(runtime, &args_val).await,

        other => {
            if let Some(result) = runtime.fs.call_remote_tool(other, &args_val).await? {
                Ok(ToolOutput {
                    value: result.clone(),
                    is_success: true, // Remote tools don't yet have a standardized success flag, assume true if Ok
                    result_summary: format!(
                        "Remote tool result: {}",
                        serde_json::to_string(&result).unwrap_or_default()
                    ),
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
    use crate::config::AppConfig;
    use crate::llm::types::ToolCallFunction;
    use crate::tools::FsTools;
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::tempdir;
    use tokio::sync::RwLock;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn test_execute_bash_failure_flag() -> Result<()> {
        let dir = tempdir()?;
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            ..AppConfig::default()
        });
        let fs_tools = FsTools::new(Arc::new(RwLock::new(None)), config);
        let runtime = ToolRuntime::build(&fs_tools, None, "test-model", None).await?;

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
}
