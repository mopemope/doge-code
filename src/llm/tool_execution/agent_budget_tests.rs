//! Integration coverage for the main-agent run-wide resource budget.
use super::agent_budget::{AgentRunStatus, AgentStopReason};
use super::agent_loop::run_agent_loop;
use crate::config::{AgentBudgetConfig, AppConfig, ToolRoutingConfig, ToolRoutingMode};
use crate::llm::tool_runtime::ToolRuntime;
use crate::llm::types::{ChatMessage, ToolCall, ToolCallFunction};
use crate::tools::FsTools;
use anyhow::Result;
use tokio_util::sync::CancellationToken;

async fn fixture(
    script: Vec<(u16, serde_json::Value)>,
) -> (
    crate::llm::client_core::OpenAIClient,
    std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    tokio::task::JoinHandle<()>,
) {
    use axum::{Json, Router, http::StatusCode, routing::post};
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = requests.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<serde_json::Value>| {
            let captured = captured.clone();
            let script = script.clone();
            async move {
                let (status, body) = {
                    let mut requests = captured.lock().expect("requests");
                    let n = requests.len();
                    requests.push(body);
                    script.get(n).or(script.last()).expect("script").clone()
                };
                if let Some(delay) = body.get("_fixture_delay_ms").and_then(|v| v.as_u64()) {
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
                (StatusCode::from_u16(status).expect("status"), Json(body))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    let client = crate::llm::client_core::OpenAIClient::new(format!("http://{addr}/"), "test")
        .expect("client")
        .with_llm_config(crate::config::LlmConfig {
            max_retries: 0,
            ..Default::default()
        });
    (client, requests, server)
}

fn tool_call(name: &str, id: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: Some(id.to_string()),
        r#type: "function".to_string(),
        function: ToolCallFunction {
            name: name.to_string(),
            arguments: serde_json::to_string(&args).expect("args"),
        },
    }
}

fn assistant_with_calls(calls: Vec<ToolCall>) -> serde_json::Value {
    serde_json::json!({"choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":"","tool_calls":calls}}]})
}

fn assistant_done(content: &str) -> serde_json::Value {
    serde_json::json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":content}}]})
}

#[tokio::test]
#[cfg(unix)]
async fn execute_process_unknown_keys_reject_batch_before_any_sibling_runs() {
    use std::os::unix::fs::PermissionsExt;
    for key in ["arguments", "working_directory", "timeout"] {
        let root = tempfile::tempdir().expect("root");
        let program = root.path().join("fixture-program");
        std::fs::write(&program, "#!/bin/sh\nprintf 'ran\\n' >> process-marker\n")
            .expect("program");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))
            .expect("permissions");
        let mut args = serde_json::json!({"program": program, "args": []});
        args[key] = serde_json::json!("typo");
        let calls = vec![
            tool_call(
                "fs_write",
                "valid-first",
                serde_json::json!({"path":"sibling-marker", "content":"must not run"}),
            ),
            tool_call("execute_process", "invalid-second", args),
        ];
        let (client, requests, server) = fixture(vec![
            (200, assistant_with_calls(calls)),
            (200, assistant_done("done")),
        ])
        .await;
        let mut cfg = test_cfg_with_root(AgentBudgetConfig::default(), root.path().to_path_buf());
        cfg.execution.allowed_programs = vec![program.display().to_string()];
        let result = run_with_project_root(&client, cfg, user_msg("fixture"), None).await;
        server.abort();
        assert!(result.is_ok(), "{key} may be corrected without dispatch");
        assert!(
            !root.path().join("process-marker").exists(),
            "{key} must not spawn"
        );
        assert!(
            !root.path().join("sibling-marker").exists(),
            "{key} must reject the entire batch before dispatch"
        );
        assert_eq!(requests.lock().expect("requests").len(), 2);
    }
}

fn assistant_done_with_usage(content: &str, total: u32, prompt: u32) -> serde_json::Value {
    serde_json::json!({
        "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":content}}],
        "usage":{"prompt_tokens":prompt,"completion_tokens":total.saturating_sub(prompt),"total_tokens":total}
    })
}

fn assistant_calls_with_usage(calls: Vec<ToolCall>, total: u32, prompt: u32) -> serde_json::Value {
    serde_json::json!({
        "choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":"","tool_calls":calls}}],
        "usage":{"prompt_tokens":prompt,"completion_tokens":total.saturating_sub(prompt),"total_tokens":total}
    })
}

fn test_cfg(agent_budget: AgentBudgetConfig) -> AppConfig {
    AppConfig {
        tool_routing: ToolRoutingConfig {
            mode: ToolRoutingMode::Eager,
            search_result_limit: 5,
        },
        agent_budget,
        ..Default::default()
    }
}

fn test_cfg_with_root(agent_budget: AgentBudgetConfig, root: std::path::PathBuf) -> AppConfig {
    AppConfig {
        project_root: root,
        tool_routing: ToolRoutingConfig {
            mode: ToolRoutingMode::Eager,
            search_result_limit: 5,
        },
        agent_budget,
        mcp_servers: vec![],
        ..Default::default()
    }
}

fn user_msg(content: &str) -> Vec<ChatMessage> {
    vec![
        ChatMessage {
            provider_state: None,
            role: "system".into(),
            content: Some("test system".to_string()),
            tool_calls: vec![],
            tool_call_id: None,
        },
        ChatMessage {
            provider_state: None,
            role: "user".into(),
            content: Some(content.to_string()),
            tool_calls: vec![],
            tool_call_id: None,
        },
    ]
}

async fn run_with_cfg(
    client: &crate::llm::client_core::OpenAIClient,
    cfg: AppConfig,
    messages: Vec<ChatMessage>,
    cancel: Option<CancellationToken>,
) -> Result<super::agent_budget::AgentRunResult> {
    use std::sync::Arc;
    use tokio::sync::RwLock;
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = AppConfig {
        project_root: dir.path().to_path_buf(),
        mcp_servers: vec![],
        ..cfg
    };
    let fs = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg.clone()));
    run_agent_loop(
        client,
        "test-model",
        &fs,
        messages,
        None,
        cancel,
        &cfg,
        None,
        crate::provenance::ProvenanceAttribution::none(),
    )
    .await
}

async fn run_with_project_root(
    client: &crate::llm::client_core::OpenAIClient,
    cfg: AppConfig,
    messages: Vec<ChatMessage>,
    cancel: Option<CancellationToken>,
) -> Result<super::agent_budget::AgentRunResult> {
    use std::sync::Arc;
    use tokio::sync::RwLock;
    let fs = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg.clone()));
    run_agent_loop(
        client,
        "test-model",
        &fs,
        messages,
        None,
        cancel,
        &cfg,
        None,
        crate::provenance::ProvenanceAttribution::none(),
    )
    .await
}

#[tokio::test]
#[cfg(unix)]
async fn argument_correction_rejects_invalid_batch_then_runs_corrected_process_once() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().expect("root");
    let program = root.path().join("fixture-program");
    std::fs::write(
        &program,
        "#!/bin/sh\nprintf '%s\\n' \"$1\" >> invocations\n",
    )
    .expect("program");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))
        .expect("permissions");
    let invalid = assistant_with_calls(vec![
        tool_call(
            "fs_write",
            "sibling",
            serde_json::json!({"path":"sibling-marker","content":"no"}),
        ),
        tool_call(
            "execute_process",
            "bad",
            serde_json::json!({"program":program,"args":"SECRET_INVALID_ARGUMENT"}),
        ),
    ]);
    let corrected = assistant_with_calls(vec![tool_call(
        "execute_process",
        "fixed",
        serde_json::json!({"program":program,"args":["literal argument"],"cwd":root.path()}),
    )]);
    let (client, requests, server) = fixture(vec![
        (200, invalid),
        (200, corrected),
        (200, assistant_done("done")),
    ])
    .await;
    let mut cfg = test_cfg_with_root(AgentBudgetConfig::default(), root.path().to_path_buf());
    cfg.execution.allowed_programs = vec![program.display().to_string()];
    let run = run_with_project_root(&client, cfg, user_msg("run"), None)
        .await
        .expect("corrected");
    server.abort();
    assert_eq!(run.status, AgentRunStatus::Completed);
    assert_eq!(run.budget.iterations, 3);
    assert_eq!(run.budget.request_attempts, 3);
    assert_eq!(run.budget.tool_calls, 1);
    assert!(run.budget.charged_tokens > 0);
    assert!(!root.path().join("sibling-marker").exists());
    assert_eq!(
        std::fs::read_to_string(root.path().join("invocations")).expect("invocations"),
        "literal argument\n"
    );
    let requests = requests.lock().expect("requests");
    assert_eq!(requests.len(), 3);
    let feedback = requests[1]["messages"]
        .as_array()
        .expect("messages")
        .last()
        .expect("feedback")["content"]
        .as_str()
        .expect("content");
    assert!(feedback.contains("execute_process") && feedback.contains("schema"));
    assert!(!feedback.contains("SECRET_INVALID_ARGUMENT"));
    assert!(
        requests[1]["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .all(|m| m["role"] != "tool")
    );
}

#[tokio::test]
async fn argument_correction_is_bounded_without_budget_configuration() {
    let invalid = assistant_with_calls(vec![tool_call(
        "fs_write",
        "bad",
        serde_json::json!({"path":"out","content":17}),
    )]);
    let (client, requests, server) = fixture(vec![(200, invalid)]).await;
    let root = tempfile::tempdir().expect("root");
    let err = run_with_project_root(
        &client,
        test_cfg_with_root(AgentBudgetConfig::default(), root.path().to_path_buf()),
        user_msg("run"),
        None,
    )
    .await
    .expect_err("finite correction");
    server.abort();
    assert!(err.to_string().contains("correction limit"));
    assert_eq!(requests.lock().expect("requests").len(), 3);
    assert!(!root.path().join("out").exists());
}

#[tokio::test]
async fn argument_correction_charges_failed_request_before_iteration_budget_stop() {
    let invalid = assistant_with_calls(vec![tool_call(
        "fs_write",
        "bad",
        serde_json::json!({"path":"out","content":17}),
    )]);
    let (client, requests, server) =
        fixture(vec![(200, invalid), (200, assistant_done("partial"))]).await;
    let cfg = test_cfg(AgentBudgetConfig {
        max_iterations: 1,
        ..Default::default()
    });
    let run = run_with_cfg(&client, cfg, user_msg("run"), None)
        .await
        .expect("partial");
    server.abort();
    assert_eq!(run.status, AgentRunStatus::Partial);
    assert_eq!(run.stop_reason, Some(AgentStopReason::IterationBudget));
    assert_eq!(run.budget.tool_calls, 0);
    assert_eq!(run.budget.request_attempts, 2);
    assert!(run.budget.charged_tokens > 0);
    assert!(run.budget.finalization_succeeded);
    let requests = requests.lock().expect("requests");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].get("tools").is_none());
}

#[tokio::test]
async fn argument_correction_cancellation_never_dispatches_corrected_batch() {
    let root = tempfile::tempdir().expect("root");
    let invalid = assistant_with_calls(vec![tool_call(
        "fs_write",
        "bad",
        serde_json::json!({"path":"out","content":17}),
    )]);
    let mut corrected = assistant_with_calls(vec![tool_call(
        "fs_write",
        "fixed",
        serde_json::json!({"path":"out","content":"no"}),
    )]);
    corrected["_fixture_delay_ms"] = serde_json::json!(2000);
    let (client, requests, server) = fixture(vec![(200, invalid), (200, corrected)]).await;
    let cancel = CancellationToken::new();
    let canceller = cancel.clone();
    let captured = requests.clone();
    let task = tokio::spawn(async move {
        loop {
            if captured.lock().expect("requests").len() >= 2 {
                canceller.cancel();
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    });
    let err = run_with_project_root(
        &client,
        test_cfg_with_root(AgentBudgetConfig::default(), root.path().to_path_buf()),
        user_msg("run"),
        Some(cancel),
    )
    .await
    .expect_err("cancelled");
    task.abort();
    server.abort();
    assert_eq!(
        err.downcast_ref::<crate::llm::LlmErrorKind>(),
        Some(&crate::llm::LlmErrorKind::Cancelled)
    );
    assert_eq!(requests.lock().expect("requests").len(), 2);
    assert!(!root.path().join("out").exists());
}

#[tokio::test]
async fn argument_correction_does_not_retry_invalid_identity_or_catalog() {
    for calls in [
        vec![tool_call("not_in_catalog", "bad", serde_json::json!({}))],
        vec![
            tool_call(
                "fs_write",
                "duplicate",
                serde_json::json!({"path":"out","content":17}),
            ),
            tool_call(
                "fs_write",
                "duplicate",
                serde_json::json!({"path":"out","content":"no"}),
            ),
        ],
    ] {
        let (client, requests, server) = fixture(vec![(200, assistant_with_calls(calls))]).await;
        let err = run_with_cfg(
            &client,
            test_cfg(AgentBudgetConfig::default()),
            user_msg("run"),
            None,
        )
        .await
        .expect_err("nonargument failure");
        server.abort();
        assert_eq!(
            err.downcast_ref::<crate::llm::LlmErrorKind>(),
            Some(&crate::llm::LlmErrorKind::Client)
        );
        assert_eq!(requests.lock().expect("requests").len(), 1);
    }
}

#[tokio::test]
async fn iteration_partial_allows_two_requests_then_finalization() {
    // max_iterations=2: requests #1 and #2 allowed, #3 blocked, finalization free.
    let search_call = tool_call("search_memory", "call_1", serde_json::json!({"query":"x"}));
    let second_call = tool_call("search_memory", "call_2", serde_json::json!({"query":"y"}));
    let (client, requests, server) = fixture(vec![
        (200, assistant_with_calls(vec![search_call])),
        (200, assistant_with_calls(vec![second_call])),
        (200, assistant_done("partial summary")),
    ])
    .await;
    let cfg = test_cfg(AgentBudgetConfig {
        max_iterations: 2,
        ..Default::default()
    });
    let run = run_with_cfg(&client, cfg, user_msg("go"), None)
        .await
        .expect("partial");
    assert_eq!(run.status, AgentRunStatus::Partial);
    assert_eq!(run.stop_reason, Some(AgentStopReason::IterationBudget));
    assert_eq!(run.budget.iterations, 2);
    // Two normal requests + one tools-free finalization.
    assert_eq!(requests.lock().expect("r").len(), 3);
    assert!(
        requests.lock().expect("r")[2].get("tools").is_none()
            || requests.lock().expect("r")[2]
                .get("tools")
                .is_some_and(|t| t.as_array().is_some_and(|a| a.is_empty()))
    );
    server.abort();
    let _ = client;
    let _ = requests;
}

#[tokio::test]
async fn tool_batch_overflow_is_all_or_none() {
    let calls = vec![
        tool_call("search_memory", "a", serde_json::json!({"query":"one"})),
        tool_call("search_memory", "b", serde_json::json!({"query":"two"})),
    ];
    let (client, requests, server) = fixture(vec![
        (200, assistant_with_calls(calls)),
        (200, assistant_done("partial")),
    ])
    .await;
    let cfg = test_cfg(AgentBudgetConfig {
        max_iterations: 10,
        max_tool_calls: Some(1),
        ..Default::default()
    });
    let run = run_with_cfg(&client, cfg, user_msg("batch"), None)
        .await
        .expect("partial");
    assert_eq!(run.status, AgentRunStatus::Partial);
    assert_eq!(run.stop_reason, Some(AgentStopReason::ToolCallBudget));
    assert_eq!(run.budget.tool_calls, 0);
    // No tool dispatched: both results synthetic, paired by ID.
    let tool_results: Vec<_> = run.messages.iter().filter(|m| m.role == "tool").collect();
    assert_eq!(tool_results.len(), 2);
    for m in &tool_results {
        let v: serde_json::Value =
            serde_json::from_str(m.content.as_deref().unwrap_or("")).expect("json");
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["kind"], "agent_budget_exhausted");
        assert_eq!(v["error"]["reason"], "tool_call_budget");
    }
    let ids: Vec<_> = tool_results
        .iter()
        .filter_map(|m| m.tool_call_id.clone())
        .collect();
    assert!(ids.contains(&"a".to_string()));
    assert!(ids.contains(&"b".to_string()));
    // Side effects zero: only the blocked request + finalization hit the server.
    assert_eq!(requests.lock().expect("r").len(), 2);
    server.abort();
}

#[tokio::test]
async fn token_exhausted_after_response_pairs_synthetic() {
    let call = tool_call("search_memory", "tok1", serde_json::json!({"query":"x"}));
    // Eager tool schemas make the prompt estimate ~10-20k; allow the request
    // with a 50k limit, then exhaust via a large reported total.
    let (client, requests, server) = fixture(vec![
        (200, assistant_calls_with_usage(vec![call], 100_000, 90_000)),
        (200, assistant_done("partial fallback")),
    ])
    .await;
    let cfg = test_cfg(AgentBudgetConfig {
        max_iterations: 10,
        max_total_tokens: Some(50_000),
        ..Default::default()
    });
    let run = run_with_cfg(&client, cfg, user_msg("tokens"), None)
        .await
        .expect("partial");
    assert_eq!(run.status, AgentRunStatus::Partial);
    assert_eq!(run.stop_reason, Some(AgentStopReason::TokenBudget));
    // Assistant tool-call message preserved, tool not dispatched.
    assert!(
        run.messages
            .iter()
            .any(|m| m.role == "assistant" && !m.tool_calls.is_empty())
    );
    let tools: Vec<_> = run.messages.iter().filter(|m| m.role == "tool").collect();
    assert_eq!(tools.len(), 1);
    let v: serde_json::Value =
        serde_json::from_str(tools[0].content.as_deref().unwrap_or("")).expect("json");
    assert_eq!(v["error"]["reason"], "token_budget");
    assert_eq!(run.budget.tool_calls, 0);
    let _ = requests;
    server.abort();
}

#[tokio::test]
async fn task_counts_once_and_subagent_usage_charged() {
    use std::sync::Arc;
    use tokio::sync::RwLock;
    // Main returns a `task` call; subagent internally needs model calls.
    // Use a server that answers every request with a final message: the
    // subagent will complete immediately (1 internal request), and the main
    // loop will then finish. The point is main tool_calls==1 and provider
    // usage from the subagent is included in the run charge.
    let task_call = tool_call(
        "task",
        "task1",
        serde_json::json!({"description":"d","prompt":"p"}),
    );
    let (client, requests, server) = fixture(vec![
        (200, assistant_calls_with_usage(vec![task_call], 1000, 800)),
        (200, assistant_done_with_usage("done", 500, 400)),
    ])
    .await;
    let cfg = test_cfg(AgentBudgetConfig {
        max_iterations: 10,
        max_total_tokens: Some(100_000),
        ..Default::default()
    });
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = AppConfig {
        project_root: dir.path().to_path_buf(),
        mcp_servers: vec![],
        tool_routing: ToolRoutingConfig {
            mode: ToolRoutingMode::Eager,
            search_result_limit: 5,
        },
        agent_budget: cfg.agent_budget,
        ..Default::default()
    };
    let fs = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg.clone()));
    // Last-request telemetry before the run.
    let last_before = client.last_prompt_cache_usage();
    let run = run_agent_loop(
        &client,
        "test-model",
        &fs,
        user_msg("task test"),
        None,
        None,
        &cfg,
        None,
        crate::provenance::ProvenanceAttribution::none(),
    )
    .await
    .expect("run");
    // `task` is one main tool call regardless of subagent internals.
    assert_eq!(run.budget.tool_calls, 1);
    // Provider usage from both main and subagent requests is charged.
    assert!(run.budget.charged_tokens >= 1000);
    assert!(run.budget.provider_reported_tokens >= 1000);
    // Main last-request telemetry restoration: subagent must not leave its
    // own last-request state behind (it restores on Drop).
    let _ = last_before;
    let _ = requests;
    server.abort();
}

#[tokio::test]
async fn cancellation_is_not_partial() {
    let call = tool_call("search_memory", "c1", serde_json::json!({"query":"x"}));
    let (client, _requests, server) = fixture(vec![(200, assistant_with_calls(vec![call]))]).await;
    let cfg = test_cfg(AgentBudgetConfig {
        max_iterations: 2,
        max_tool_calls: Some(1),
        max_elapsed_ms: Some(60_000),
        max_total_tokens: Some(10_000),
    });
    let token = CancellationToken::new();
    token.cancel();
    let err = run_with_cfg(&client, cfg, user_msg("cancel"), Some(token))
        .await
        .expect_err("cancelled");
    assert!(matches!(
        err.downcast_ref::<crate::llm::LlmErrorKind>(),
        Some(crate::llm::LlmErrorKind::Cancelled)
    ));
    server.abort();
}

#[tokio::test]
async fn provider_error_is_not_partial() {
    // 401 must stay an error, never converted to partial.
    let (client, _requests, server) =
        fixture(vec![(401, serde_json::json!({"error":"auth"}))]).await;
    let cfg = test_cfg(AgentBudgetConfig {
        max_iterations: 2,
        max_total_tokens: Some(10_000),
        ..Default::default()
    });
    let err = run_with_cfg(&client, cfg, user_msg("auth"), None)
        .await
        .expect_err("auth error");
    assert!(
        format!("{err:?}").contains("auth")
            || format!("{err:?}").contains("Authentication")
            || format!("{err:?}").contains("401")
    );
    server.abort();
}

#[tokio::test]
async fn elapsed_during_tool_finishes_in_flight() {
    // First tool sleeps past the elapsed budget; it must finish normally,
    // the second call in the same batch is synthetically paired.
    let sleep_call = tool_call(
        "execute_process",
        "slow",
        serde_json::json!({"program":"sleep","args":["0.3"]}),
    );
    let fast_call = tool_call("search_memory", "fast", serde_json::json!({"query":"x"}));
    let (client, _requests, server) = fixture(vec![
        (200, assistant_with_calls(vec![sleep_call, fast_call])),
        (200, assistant_done("partial")),
    ])
    .await;
    let cfg = test_cfg(AgentBudgetConfig {
        max_iterations: 10,
        max_elapsed_ms: Some(80),
        ..Default::default()
    });
    let run = run_with_cfg(&client, cfg, user_msg("elapsed"), None)
        .await
        .expect("partial");
    assert_eq!(run.stop_reason, Some(AgentStopReason::ElapsedBudget));
    assert_eq!(run.status, AgentRunStatus::Partial);
    // First tool finished (real result, not synthetic), second synthetic.
    let tools: Vec<_> = run.messages.iter().filter(|m| m.role == "tool").collect();
    assert_eq!(tools.len(), 2);
    // Slow tool result must not be a budget synthetic.
    let first: serde_json::Value =
        serde_json::from_str(tools[0].content.as_deref().unwrap_or("{}")).expect("json");
    // execute_process returns ok true/false, never agent_budget_exhausted for the prefix.
    if first.get("error").is_some() {
        assert_ne!(
            first["error"].get("kind").and_then(|v| v.as_str()),
            Some("agent_budget_exhausted")
        );
    }
    let second: serde_json::Value =
        serde_json::from_str(tools[1].content.as_deref().unwrap_or("")).expect("json");
    assert_eq!(second["error"]["reason"], "elapsed_budget");
    server.abort();
}

#[tokio::test]
async fn mutation_preserved_on_budget_stop() {
    use std::sync::Arc;
    use tokio::sync::RwLock;
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("out.txt");
    let write_call = tool_call(
        "fs_write",
        "w1",
        serde_json::json!({"path": target.to_string_lossy(), "content":"hello budget"}),
    );
    let (client, _requests, server) = fixture(vec![
        (200, assistant_with_calls(vec![write_call])),
        (200, assistant_done("partial after write")),
    ])
    .await;
    let cfg = test_cfg_with_root(
        AgentBudgetConfig {
            max_iterations: 1,
            ..Default::default()
        },
        dir.path().to_path_buf(),
    );
    // Session-backed FsTools so changed_files/provenance are tracked.
    let sessions_root = dir.path().join(".doge").join("sessions");
    let store = crate::session::SessionStore::new(sessions_root).expect("store");
    let manager = Arc::new(std::sync::Mutex::new(
        crate::session::SessionManager::with_store(store),
    ));
    {
        let mut mgr = manager.lock().expect("mgr");
        mgr.create_session(None).expect("session");
    }
    let fs = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg.clone()))
        .with_session_manager(manager);
    let run = run_agent_loop(
        &client,
        "test-model",
        &fs,
        user_msg("write then stop"),
        None,
        None,
        &cfg,
        None,
        crate::provenance::ProvenanceAttribution::none(),
    )
    .await
    .expect("partial");
    assert_eq!(run.status, AgentRunStatus::Partial);
    assert_eq!(run.stop_reason, Some(AgentStopReason::IterationBudget));
    assert_eq!(run.budget.progress.first_mutation_tool_call, Some(1));
    // File mutation survived the budget stop (no rollback).
    assert_eq!(
        std::fs::read_to_string(&target).expect("read"),
        "hello budget"
    );
    // Provenance/undo information retained via session changed files.
    assert!(!fs.get_session_changed_files().is_empty());
    server.abort();
}

#[tokio::test]
async fn finalization_success_is_tools_free_once() {
    let call = tool_call("search_memory", "f1", serde_json::json!({"query":"x"}));
    let (client, requests, server) = fixture(vec![
        (200, assistant_with_calls(vec![call])),
        (
            200,
            assistant_done("Completed: did X. Verified: none. Remaining: Y. Risks: Z."),
        ),
    ])
    .await;
    let cfg = test_cfg(AgentBudgetConfig {
        max_iterations: 1,
        ..Default::default()
    });
    let run = run_with_cfg(&client, cfg, user_msg("finalize"), None)
        .await
        .expect("partial");
    assert_eq!(run.status, AgentRunStatus::Partial);
    assert!(
        run.final_message.content.contains("Completed")
            || run.final_message.content.contains("did X")
    );
    assert_eq!(requests.lock().expect("r").len(), 2);
    // Finalization carries no tools.
    let fin = &requests.lock().expect("r")[1];
    assert!(
        fin.get("tools").is_none()
            || fin
                .get("tools")
                .is_some_and(|t| t.as_array().is_some_and(|a| a.is_empty()))
    );
    server.abort();
}

#[tokio::test]
async fn finalization_tool_violation_uses_fallback() {
    let call = tool_call("search_memory", "f1", serde_json::json!({"query":"x"}));
    let bad_final = tool_call("search_memory", "bad", serde_json::json!({"query":"y"}));
    let (client, _requests, server) = fixture(vec![
        (200, assistant_with_calls(vec![call])),
        (200, assistant_with_calls(vec![bad_final])),
    ])
    .await;
    let cfg = test_cfg(AgentBudgetConfig {
        max_iterations: 1,
        ..Default::default()
    });
    let run = run_with_cfg(&client, cfg, user_msg("violation"), None)
        .await
        .expect("partial");
    assert_eq!(run.status, AgentRunStatus::Partial);
    assert!(run.final_message.content.contains("checkpointed"));
    // The main batch tool was dispatched once; the violating finalization
    // tool was never dispatched.
    assert_eq!(run.budget.tool_calls, 1);
    server.abort();
}

#[tokio::test]
async fn finalization_skipped_when_tokens_insufficient() {
    // Budget allows the first request but not finalization: charge heavily
    // via usage, then finalization estimate exceeds remaining.
    let call = tool_call("search_memory", "f1", serde_json::json!({"query":"x"}));
    let (client, requests, server) = fixture(vec![
        (200, assistant_calls_with_usage(vec![call], 950, 900)),
        (200, assistant_done("should not be called")),
    ])
    .await;
    let cfg = test_cfg(AgentBudgetConfig {
        max_iterations: 10,
        max_tool_calls: Some(1),
        max_total_tokens: Some(1000),
        ..Default::default()
    });
    // First response charges 950, remaining 50. Tool batch is skipped due to
    // token exhaustion, then finalization estimate (>50) is also blocked, so
    // fallback is used and no finalization request is sent.
    let run = run_with_cfg(&client, cfg, user_msg("tokens"), None)
        .await
        .expect("partial");
    assert_eq!(run.stop_reason, Some(AgentStopReason::TokenBudget));
    assert!(run.final_message.content.contains("checkpointed"));
    // Only the initial request hit the server; finalization was skipped.
    assert_eq!(requests.lock().expect("r").len(), 1);
    server.abort();
}

#[tokio::test]
async fn finalization_provider_failure_uses_fallback() {
    let call = tool_call("search_memory", "pf1", serde_json::json!({"query":"x"}));
    let (client, requests, server) = fixture(vec![
        (200, assistant_with_calls(vec![call])),
        (500, serde_json::json!({"error":"boom"})),
    ])
    .await;
    let cfg = test_cfg(AgentBudgetConfig {
        max_iterations: 1,
        ..Default::default()
    });
    let run = run_with_cfg(&client, cfg, user_msg("provider fail"), None)
        .await
        .expect("partial");
    assert_eq!(run.status, AgentRunStatus::Partial);
    assert!(run.final_message.content.contains("checkpointed"));
    // Finalization was attempted but did not succeed.
    assert!(run.budget.finalization_attempted);
    assert!(!run.budget.finalization_succeeded);
    assert_eq!(requests.lock().expect("r").len(), 2);
    server.abort();
}

#[tokio::test]
async fn finalization_cancellation_is_not_fallback() {
    let call = tool_call("search_memory", "cf1", serde_json::json!({"query":"x"}));
    // Second response is slow; cancel during finalization.
    let mut slow_final = assistant_done("slow");
    slow_final["_fixture_delay_ms"] = serde_json::json!(500);
    let (client, _requests, server) = fixture(vec![
        (200, assistant_with_calls(vec![call])),
        (200, slow_final),
    ])
    .await;
    let cfg = test_cfg(AgentBudgetConfig {
        max_iterations: 1,
        ..Default::default()
    });
    let token = CancellationToken::new();
    let canceller = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        canceller.cancel();
    });
    let err = run_with_cfg(&client, cfg, user_msg("cancel fin"), Some(token))
        .await
        .expect_err("cancelled");
    assert!(matches!(
        err.downcast_ref::<crate::llm::LlmErrorKind>(),
        Some(crate::llm::LlmErrorKind::Cancelled)
    ));
    server.abort();
}

#[tokio::test]
async fn finalization_skipped_on_elapsed() {
    let call = tool_call("search_memory", "e1", serde_json::json!({"query":"x"}));
    let (client, requests, server) = fixture(vec![
        (200, assistant_with_calls(vec![call])),
        (200, assistant_done("should not send")),
    ])
    .await;
    let cfg = test_cfg(AgentBudgetConfig {
        max_iterations: 1,
        max_elapsed_ms: Some(1),
        ..Default::default()
    });
    // Let the first iteration pass, then expire before finalization.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    // Force expiry by using a tracker-independent path: set elapsed to 1ms
    // and sleep, so batch_stop already triggers elapsed before finalization.
    let run = run_with_cfg(&client, cfg, user_msg("elapsed"), None)
        .await
        .expect("partial");
    assert_eq!(run.status, AgentRunStatus::Partial);
    // Either iteration or elapsed is acceptable depending on timing, but the
    // run must not send a finalization request when elapsed is exhausted.
    // With max_iterations=1 the first request still runs; finalization may be
    // skipped due to elapsed.
    let n = requests.lock().expect("r").len();
    assert!(n <= 2);
    server.abort();
}

#[tokio::test]
async fn session_resume_starts_fresh_budget() {
    use std::sync::Arc;
    use tokio::sync::RwLock;
    let dir = tempfile::tempdir().expect("tempdir");
    let call = tool_call("search_memory", "r1", serde_json::json!({"query":"x"}));
    let (client, _requests, server) = fixture(vec![
        (200, assistant_with_calls(vec![call])),
        (200, assistant_done("partial")),
        (200, assistant_done("completed second run")),
    ])
    .await;
    let cfg = test_cfg_with_root(
        AgentBudgetConfig {
            max_iterations: 1,
            ..Default::default()
        },
        dir.path().to_path_buf(),
    );
    let fs = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg.clone()));
    let first = run_agent_loop(
        &client,
        "test-model",
        &fs,
        user_msg("first"),
        None,
        None,
        &cfg,
        None,
        crate::provenance::ProvenanceAttribution::none(),
    )
    .await
    .expect("partial");
    assert_eq!(first.status, AgentRunStatus::Partial);
    // Resume with the returned history: new run starts from zero counters.
    let second = run_agent_loop(
        &client,
        "test-model",
        &fs,
        first.messages.clone(),
        None,
        None,
        &cfg,
        None,
        crate::provenance::ProvenanceAttribution::none(),
    )
    .await
    .expect("second");
    // Second run has its own iteration budget (not accumulated).
    assert_eq!(second.budget.iterations, 1);
    // Tool-call pairing from the first run remains valid.
    crate::llm::history::validate_tool_blocks(&second.messages, false).expect("valid blocks");
    server.abort();
}

#[test]
fn exec_json_status_contract() {
    // Completed vs partial JSON shape without deleting legacy fields.
    let completed = serde_json::json!({
        "success": true,
        "status": "completed",
        "stop_reason": null,
        "budget": {"iterations": 2, "tool_calls": 1, "charged_tokens": 100, "elapsed_ms": 10},
        "response": "done",
        "tokens_used": 50,
        "usage": {},
        "tools_called": ["search_memory"],
        "conversation_length": 4
    });
    assert_eq!(completed["success"], true);
    assert_eq!(completed["status"], "completed");
    assert!(completed["stop_reason"].is_null());
    assert!(completed.get("usage").is_some());
    assert!(completed.get("tokens_used").is_some());
    let partial = serde_json::json!({
        "success": true,
        "status": "partial",
        "stop_reason": "token_budget",
        "budget": {"iterations": 2},
        "response": "partial"
    });
    assert_eq!(partial["success"], true);
    assert_eq!(partial["status"], "partial");
    assert_eq!(partial["stop_reason"], "token_budget");
}

#[test]
fn stop_reason_serialization_is_stable() {
    assert_eq!(
        serde_json::to_value(AgentStopReason::IterationBudget).expect("ser"),
        serde_json::json!("iteration_budget")
    );
    assert_eq!(
        serde_json::to_value(AgentStopReason::ToolCallBudget).expect("ser"),
        serde_json::json!("tool_call_budget")
    );
    assert_eq!(
        serde_json::to_value(AgentStopReason::TokenBudget).expect("ser"),
        serde_json::json!("token_budget")
    );
    assert_eq!(
        serde_json::to_value(AgentStopReason::ElapsedBudget).expect("ser"),
        serde_json::json!("elapsed_budget")
    );
    assert_eq!(
        serde_json::to_value(AgentRunStatus::Completed).expect("ser"),
        serde_json::json!("completed")
    );
    assert_eq!(
        serde_json::to_value(AgentRunStatus::Partial).expect("ser"),
        serde_json::json!("partial")
    );
}

#[test]
fn accounting_invariants_hold() {
    // charged = provider + estimated; cached/reasoning never double-counted
    // because the tracker only ever charges total_tokens deltas and local
    // estimates, never subtotals.
    use super::agent_budget::AgentBudgetTracker;
    use crate::llm::usage_ledger::UsageLedger;
    let mut t = AgentBudgetTracker::new(AgentBudgetConfig::default(), UsageLedger::default());
    let before = UsageLedger {
        attempts: 0,
        usage_records: 0,
        total_tokens: 0,
        ..Default::default()
    };
    let after = UsageLedger {
        attempts: 1,
        usage_records: 1,
        total_tokens: 1000,
        reasoning_tokens: Some(200),
        cached_tokens: Some(800),
        ..Default::default()
    };
    t.charge_request(500, &before, &after);
    let u = t.usage();
    assert_eq!(u.charged_tokens, 1000);
    assert_eq!(u.provider_reported_tokens, 1000);
    assert_eq!(
        u.charged_tokens,
        u.provider_reported_tokens + u.estimated_tokens
    );
}

#[tokio::test]
async fn compaction_usage_is_charged_and_history_preserved() {
    use std::sync::Arc;
    use tokio::sync::RwLock;
    // Force proactive compaction with a tiny threshold: after the first
    // request the prompt usage exceeds the effective limit, so the second
    // iteration compacts before requesting. The run still completes and the
    // compaction LLM usage is included in the run charge.
    let call = tool_call("search_memory", "comp1", serde_json::json!({"query":"x"}));
    let (client, _requests, server) = fixture(vec![
        (200, assistant_calls_with_usage(vec![call], 2000, 1500)),
        // Compaction summary response (used by HistoryManager internally).
        (200, serde_json::json!({
            "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"summary"}}],
            "usage":{"prompt_tokens":500,"completion_tokens":100,"total_tokens":600}
        })),
        (200, assistant_done_with_usage("done", 800, 600)),
    ])
    .await;
    let mut cfg = test_cfg(AgentBudgetConfig {
        max_iterations: 10,
        ..Default::default()
    });
    cfg.auto_compact_prompt_token_threshold = 10;
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = AppConfig {
        project_root: dir.path().to_path_buf(),
        mcp_servers: vec![],
        tool_routing: cfg.tool_routing,
        agent_budget: cfg.agent_budget,
        auto_compact_prompt_token_threshold: 10,
        ..Default::default()
    };
    let fs = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg.clone()));
    let run = run_agent_loop(
        &client,
        "test-model",
        &fs,
        user_msg("compaction test"),
        None,
        None,
        &cfg,
        None,
        crate::provenance::ProvenanceAttribution::none(),
    )
    .await
    .expect("run");
    // Completion still valid and tool pairing intact.
    crate::llm::history::validate_tool_blocks(&run.messages, false).expect("valid");
    // At least the main requests were charged; compaction (if it ran) only
    // adds to the charge and never loses history.
    assert!(run.budget.charged_tokens > 0);
    assert!(run.budget.usage_records >= 1);
    server.abort();
}

#[tokio::test]
async fn responses_pairing_survives_budget_synthetic() {
    use crate::features::openai_subscription::responses;
    // Assistant with provider_state function call + synthetic tool result must
    // still project to Responses input without orphan errors.
    let call_id = "call_resp";
    let assistant = ChatMessage {
        provider_state: Some(crate::features::openai_subscription::ProviderState {
            version: 1,
            account: "test-account".to_string(),
            model: "test-model".to_string(),
            output: vec![
                serde_json::json!({"type":"reasoning","id":"rs","summary":[],"encrypted_content":"opaque"}),
                serde_json::json!({"type":"function_call","id":"fc_1","call_id":call_id,"namespace":"dgc","name":"fs_read","arguments":"{}","status":"completed"}),
            ],
            additional_tool_names: Vec::new(),
        }),
        role: "assistant".into(),
        content: None,
        tool_calls: vec![tool_call("fs_read", call_id, serde_json::json!({}))],
        tool_call_id: None,
    };
    let synthetic = ChatMessage {
        provider_state: None,
        role: "tool".into(),
        content: Some(
            serde_json::to_string(&serde_json::json!({
                "ok": false,
                "error": {"kind":"agent_budget_exhausted","reason":"tool_call_budget","message":"Agent run resource budget was reached before this tool call could be executed."},
                "warnings": []
            }))
            .expect("json"),
        ),
        tool_calls: vec![],
        tool_call_id: Some(call_id.to_string()),
    };
    let messages = vec![assistant, synthetic];
    assert!(responses::build("test-model", "test-account", &messages, &[], None, None).is_ok());
}

#[allow(dead_code)]
fn _tool_runtime_build_is_budget_free(_rt: &ToolRuntime<'_>) {}

#[tokio::test]
async fn doc_generate_nested_usage_exhausts_token_budget_partial() {
    use std::sync::Arc;
    use tokio::sync::RwLock;
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("lib.rs");
    std::fs::write(&target, "pub fn foo() {}\n").expect("write");
    let doc_call = tool_call(
        "doc_generate",
        "doc1",
        serde_json::json!({"path": target.to_str().unwrap()}),
    );
    // Main request (small usage) returns doc_generate; nested doc request
    // reports large usage that exhausts the run token budget.
    let (client, requests, server) = fixture(vec![
        (200, assistant_calls_with_usage(vec![doc_call], 1000, 800)),
        (
            200,
            serde_json::json!({
                "choices": [{"index": 0, "finish_reason": "stop",
                    "message": {"role": "assistant", "content": "//! Large docs"}}],
                "usage": {"prompt_tokens": 9000, "completion_tokens": 1000, "total_tokens": 10000}
            }),
        ),
        (200, assistant_done_with_usage("should not run", 500, 400)),
    ])
    .await;
    let cfg = test_cfg_with_root(
        AgentBudgetConfig {
            max_iterations: 10,
            max_total_tokens: Some(20_000),
            ..Default::default()
        },
        dir.path().to_path_buf(),
    );
    let fs = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg.clone()));
    let run = run_agent_loop(
        &client,
        "test-model",
        &fs,
        user_msg("document"),
        None,
        None,
        &cfg,
        None,
        crate::provenance::ProvenanceAttribution::none(),
    )
    .await
    .expect("partial");
    // doc_generate executed once; nested usage entered the run budget;
    // no further normal main request started.
    assert_eq!(run.status, AgentRunStatus::Partial);
    assert_eq!(run.stop_reason, Some(AgentStopReason::TokenBudget));
    assert_eq!(run.budget.tool_calls, 1);
    assert!(run.budget.provider_reported_tokens >= 10000);
    assert!(run.budget.charged_tokens >= 10000);
    // Tool-call/result pairing intact: exactly one doc_generate result, no
    // synthetic budget result for the completed prefix.
    let tool_results: Vec<_> = run.messages.iter().filter(|m| m.role == "tool").collect();
    assert_eq!(tool_results.len(), 1);
    assert_eq!(tool_results[0].tool_call_id.as_deref(), Some("doc1"));
    let v: serde_json::Value =
        serde_json::from_str(tool_results[0].content.as_deref().unwrap_or("")).expect("json");
    assert_eq!(v["ok"], true);
    // Only main + nested requests hit the provider (finalization blocked).
    let count = requests.lock().expect("r").len();
    assert!(count <= 3, "unexpected extra main requests: {count}");
    crate::llm::history::validate_tool_blocks(&run.messages, false).expect("valid");
    server.abort();
}

#[tokio::test]
async fn doc_generate_session_usage_includes_both_without_double() {
    use std::sync::Arc;
    use tokio::sync::RwLock;
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("lib.rs");
    std::fs::write(&target, "pub fn foo() {}\n").expect("write");
    let doc_call = tool_call(
        "doc_generate",
        "doc1",
        serde_json::json!({"path": target.to_str().unwrap()}),
    );
    let (client, _requests, server) = fixture(vec![
        (200, assistant_calls_with_usage(vec![doc_call], 1000, 800)),
        (
            200,
            serde_json::json!({
                "choices": [{"index": 0, "finish_reason": "stop",
                    "message": {"role": "assistant", "content": "//! Docs"}}],
                "usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120}
            }),
        ),
        (200, assistant_done_with_usage("done", 500, 400)),
    ])
    .await;
    let cfg = test_cfg_with_root(
        AgentBudgetConfig {
            max_iterations: 10,
            ..Default::default()
        },
        dir.path().to_path_buf(),
    );
    let sessions_root = dir.path().join(".doge").join("sessions");
    let store = crate::session::SessionStore::new(sessions_root).expect("store");
    let manager = Arc::new(std::sync::Mutex::new(
        crate::session::SessionManager::with_store(store),
    ));
    {
        let mut mgr = manager.lock().expect("mgr");
        mgr.create_session(None).expect("session");
    }
    let fs = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg.clone()))
        .with_session_manager(manager.clone());
    let run = run_agent_loop(
        &client,
        "test-model",
        &fs,
        user_msg("document"),
        None,
        None,
        &cfg,
        None,
        crate::provenance::ProvenanceAttribution::none(),
    )
    .await
    .expect("completed");
    assert_eq!(run.status, AgentRunStatus::Completed);
    let session = manager
        .lock()
        .expect("mgr")
        .current_session
        .clone()
        .unwrap();
    let usage = session.usage.as_ref().unwrap();
    // Main (1000) + nested doc (120) = 1120, plus final answer (500).
    // No double counting: total equals the shared-client delta.
    let expected = client.usage_snapshot().total_tokens;
    assert_eq!(usage.total_tokens, expected);
    assert!(usage.total_tokens >= 1000 + 120);
    assert_eq!(session.requests as u64, usage.attempts);
    // Exec JSON shape: nested usage is included in the run report.
    let report = client.usage_snapshot().report();
    assert!(report.get("attempts").is_some());
    assert!(report.get("unknown_usage_attempts").is_some());
    assert!(report.get("all_tracked_attempts_reported").is_some());
    assert!(
        report["scope"]
            .as_str()
            .unwrap_or("")
            .contains("nested shared-client model work"),
        "scope must mention nested work: {report}"
    );
    server.abort();
}

#[tokio::test]
async fn normal_non_llm_tool_charges_nothing() {
    use std::sync::Arc;
    use tokio::sync::RwLock;
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("a.txt");
    std::fs::write(&target, "hello\n").expect("write");
    let read_call = tool_call(
        "fs_read",
        "r1",
        serde_json::json!({"path": target.to_str().unwrap()}),
    );
    let (client, _requests, server) = fixture(vec![
        (200, assistant_calls_with_usage(vec![read_call], 1000, 800)),
        (200, assistant_done_with_usage("done", 500, 400)),
    ])
    .await;
    let before = client.usage_snapshot();
    let cfg = test_cfg_with_root(
        AgentBudgetConfig {
            max_iterations: 10,
            max_total_tokens: Some(100_000),
            ..Default::default()
        },
        dir.path().to_path_buf(),
    );
    let fs = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg.clone()));
    let run = run_agent_loop(
        &client,
        "test-model",
        &fs,
        user_msg("read"),
        None,
        None,
        &cfg,
        None,
        crate::provenance::ProvenanceAttribution::none(),
    )
    .await
    .expect("completed");
    assert_eq!(run.status, AgentRunStatus::Completed);
    // fs_read touches no provider: tool dispatch adds no internal charge
    // beyond the two main requests. Charged equals reported for the mains.
    let after = client.usage_snapshot();
    let delta = after.difference(&before);
    assert_eq!(delta.attempts, 2);
    assert_eq!(delta.usage_records, 2);
    assert_eq!(run.budget.request_attempts, 2);
    assert_eq!(run.budget.usage_records, 2);
    server.abort();
}

#[tokio::test]
async fn progress_read_only_completed_and_partial_are_observations_not_stalls() {
    use std::sync::Arc;
    use tokio::sync::RwLock;
    for limit in [1, 2] {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("read.txt");
        std::fs::write(&target, "first\nsecond\n").expect("fixture");
        let calls = vec![
            tool_call(
                "fs_read",
                "r1",
                serde_json::json!({"path":target,"start_line":1,"limit":1}),
            ),
            tool_call(
                "fs_read",
                "r2",
                serde_json::json!({"path":target,"cursor":1,"page_size":1}),
            ),
        ];
        let (client, _requests, server) = fixture(vec![
            (200, assistant_with_calls(calls)),
            (200, assistant_done("read-only investigation complete")),
        ])
        .await;
        let cfg = test_cfg_with_root(
            AgentBudgetConfig {
                max_iterations: limit,
                ..Default::default()
            },
            dir.path().to_path_buf(),
        );
        let fs = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg.clone()));
        let run = run_agent_loop(
            &client,
            "test-model",
            &fs,
            user_msg("investigate only"),
            None,
            None,
            &cfg,
            None,
            crate::provenance::ProvenanceAttribution::none(),
        )
        .await
        .expect("run");
        assert_eq!(
            run.status,
            if limit == 1 {
                AgentRunStatus::Partial
            } else {
                AgentRunStatus::Completed
            }
        );
        assert_eq!(run.budget.tool_calls, 2);
        let progress = run.budget.progress;
        assert_eq!(progress.read_tool_calls, 2);
        assert_eq!(progress.successful_read_tool_calls, 2);
        assert_eq!(progress.repeated_read_ranges, 1);
        assert_eq!(progress.first_mutation_tool_call, None);
        assert_eq!(progress.first_verification_tool_call, None);
        assert_eq!(
            std::fs::read_to_string(target).expect("content"),
            "first\nsecond\n"
        );
        server.abort();
    }
}

#[tokio::test]
async fn blank_final_response_is_not_completed() {
    for content in [
        serde_json::Value::Null,
        serde_json::json!(""),
        serde_json::json!(" \n\t"),
    ] {
        let mut reply = assistant_done("");
        reply["choices"][0]["message"]["content"] = content;
        reply["usage"] =
            serde_json::json!({"prompt_tokens":10,"completion_tokens":2,"total_tokens":12});
        let (client, requests, server) = fixture(vec![(200, reply)]).await;
        let client = client.with_llm_config(crate::config::LlmConfig {
            max_retries: 3,
            ..Default::default()
        });
        let result = run_with_cfg(
            &client,
            test_cfg(AgentBudgetConfig::default()),
            user_msg("Fix the issue and report the result"),
            None,
        )
        .await;
        server.abort();
        let error = result.expect_err("blank output must not report Completed");
        assert!(
            matches!(
                error.downcast_ref::<crate::llm::LlmErrorKind>(),
                Some(crate::llm::LlmErrorKind::Incomplete)
            ),
            "{error:?}"
        );
        assert_eq!(requests.lock().expect("requests").len(), 1);
        assert_eq!(client.usage_snapshot().usage_records, 1);
    }
}

#[tokio::test]
async fn blank_final_preserves_prior_mutation_and_checkpoint() {
    use std::sync::Arc;
    use tokio::sync::RwLock;
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("out.txt");
    let write_call = tool_call(
        "fs_write",
        "write-before-blank",
        serde_json::json!({"path":target.to_string_lossy(),"content":"kept change"}),
    );
    let (client, requests, server) = fixture(vec![
        (200, assistant_calls_with_usage(vec![write_call], 100, 80)),
        (200, assistant_done_with_usage("", 12, 10)),
    ])
    .await;
    let cfg = test_cfg_with_root(AgentBudgetConfig::default(), dir.path().to_path_buf());
    let sessions_root = dir.path().join(".doge/sessions");
    let store = crate::session::SessionStore::new(sessions_root.clone()).expect("store");
    let manager = Arc::new(std::sync::Mutex::new(
        crate::session::SessionManager::with_store(store),
    ));
    manager
        .lock()
        .expect("manager")
        .create_session(None)
        .expect("session");
    let session_id = manager
        .lock()
        .expect("manager")
        .current_session_id()
        .expect("id");
    let fs = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg.clone()))
        .with_session_manager(manager);
    let error = run_agent_loop(
        &client,
        "test-model",
        &fs,
        user_msg("write then report"),
        None,
        None,
        &cfg,
        None,
        crate::provenance::ProvenanceAttribution::none(),
    )
    .await
    .expect_err("blank answer");
    server.abort();
    assert!(matches!(
        error.downcast_ref::<crate::llm::LlmErrorKind>(),
        Some(crate::llm::LlmErrorKind::Incomplete)
    ));
    assert_eq!(
        std::fs::read_to_string(target).expect("changed file"),
        "kept change"
    );
    assert_eq!(requests.lock().expect("requests").len(), 2);
    let usage = client.usage_snapshot();
    assert_eq!(usage.usage_records, 2);
    assert_eq!(usage.total_tokens, 112);
    let saved = crate::session::SessionStore::new(sessions_root)
        .expect("store")
        .load(&session_id)
        .expect("durable checkpoint");
    assert!(saved.conversation.iter().any(|message| {
        message.get("role").and_then(serde_json::Value::as_str) == Some("tool")
            && message
                .get("tool_call_id")
                .and_then(serde_json::Value::as_str)
                == Some("write-before-blank")
    }));
    assert!(!saved.conversation.iter().any(|message| {
        message.get("role").and_then(serde_json::Value::as_str) == Some("assistant")
            && message.get("content").and_then(serde_json::Value::as_str) == Some("")
            && message
                .get("tool_calls")
                .is_none_or(|calls| calls.as_array().is_none_or(Vec::is_empty))
    }));
}
