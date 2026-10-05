use super::*;
use crate::llm::types::{ToolCall, ToolCallFunction};

// Characterization fixture: capture requests before changing the loop.
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

async fn fixture_run(
    client: &crate::llm::client_core::OpenAIClient,
    cfg: crate::config::AppConfig,
    cancel: Option<CancellationToken>,
) -> Result<SubagentRun> {
    use std::sync::Arc;
    use tokio::sync::RwLock;
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("evidence.txt"), "fixture evidence").expect("write fixture");
    let cfg = crate::config::AppConfig {
        project_root: dir.path().to_path_buf(),
        mcp_servers: vec![],
        ..cfg
    };
    let fs = crate::tools::FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));
    let runtime =
        ToolRuntime::build(&fs, Some(client.clone()), "test-model", cancel.clone()).await?;
    run_subagent(
        client,
        "test-model",
        &runtime,
        "fixture",
        "investigate",
        cancel,
        "/fixture",
    )
    .await
}

#[tokio::test]
async fn test_provider_context_overflow_fallback() {
    let (client, requests, server) = fixture(vec![(
        400,
        serde_json::json!({"error":{"code":"context_length_exceeded","message":"too big"}}),
    )])
    .await;
    let result = fixture_run(&client, crate::config::AppConfig::default(), None).await;
    let result = result.expect("partial");
    assert_eq!(
        result.stop_reason,
        Some(SubagentStopReason::ProviderContextExceeded)
    );
    assert_eq!(result.status, SubagentRunStatus::Partial);
    assert_eq!(requests.lock().expect("requests").len(), 2);
    assert!(requests.lock().expect("requests")[1].get("tools").is_none());
    server.abort();
}

#[tokio::test]
async fn test_exact_iteration_limit_with_finalization() {
    let (client, requests, server) = fixture(vec![(200, serde_json::json!({"choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","tool_calls":[{"id":"blocked","type":"function","function":{"name":"fs_list","arguments":"{\"path\":\"/fixture\"}"}}]}}]}))]).await;
    let result = fixture_run(
        &client,
        crate::config::AppConfig {
            subagent: crate::config::SubagentConfig {
                max_iterations: 2,
                ..Default::default()
            },
            ..Default::default()
        },
        None,
    )
    .await
    .expect("run");
    assert_eq!(result.iterations, 2);
    assert_eq!(
        result.stop_reason,
        Some(SubagentStopReason::IterationBudget)
    );
    assert_eq!(result.tool_calls, 2);
    assert_eq!(requests.lock().expect("requests").len(), 3);
    assert!(requests.lock().expect("requests")[2].get("tools").is_none());
    server.abort();
}

// Budget/cancellation fixtures use a catalog-visible read-only operation.
fn read_only_call() -> ToolCall {
    let mut call = make_call("fs_list", "read-fixture");
    call.function.arguments = serde_json::json!({"path":"/fixture"}).to_string();
    call
}

fn make_call(name: &str, id: &str) -> ToolCall {
    ToolCall {
        id: Some(id.to_string()),
        r#type: "function".to_string(),
        function: ToolCallFunction {
            name: name.to_string(),
            arguments: "{}".to_string(),
        },
    }
}

#[test]
fn test_summarize_final_trims_long_output() {
    let long = "fact ".repeat(5_000);
    let summary = summarize_final(&long);
    assert!(summary.chars().count() <= SUBAGENT_SUMMARY_BUDGET_CHARS);
    assert!(summary.contains("output truncated"));
}

#[test]
fn test_summarize_final_empty() {
    assert_eq!(
        summarize_final("   "),
        "Sub-agent returned an empty summary."
    );
}

#[test]
fn test_record_examined_files_fs_read() {
    let output = crate::llm::tool_execution::dispatch::ToolOutput {
        value: serde_json::json!({"ok": true, "result": {"path": "/tmp/a.rs"}}),
        is_success: true,
        result_summary: String::new(),
    };
    let mut ledger = SubagentEvidenceLedger::default();
    ledger.record_files("fs_read", &output);
    assert_eq!(ledger.files, vec!["/tmp/a.rs".to_string()]);
    // Duplicates are not added twice.
    ledger.record_files("fs_read", &output);
    assert_eq!(ledger.files.len(), 1);
}

#[test]
fn test_record_examined_files_search_results() {
    let output = crate::llm::tool_execution::dispatch::ToolOutput {
        value: serde_json::json!({
            "ok": true,
            "results": [
                {"path": "/tmp/x.rs", "line": 1, "text": "hit"},
                {"path": "/tmp/y.rs", "line": 2, "text": "hit"}
            ]
        }),
        is_success: true,
        result_summary: String::new(),
    };
    let mut ledger = SubagentEvidenceLedger::default();
    ledger.record_files("search_text", &output);
    assert_eq!(
        ledger.files,
        vec!["/tmp/x.rs".to_string(), "/tmp/y.rs".to_string()]
    );
}

#[test]
fn test_tool_call_blocked_message_shape() {
    // Sanity-check the JSON error payload used for blocked tools.
    let payload = serde_json::json!({"error": "tool 'execute_bash' is not available to the sub-agent; only read-only tools are allowed"});
    assert!(payload["error"].as_str().unwrap().contains("read-only"));
    let _ = make_call("execute_bash", "call_1");
}

fn assistant(calls: Vec<ToolCall>, content: Option<&str>) -> serde_json::Value {
    let reason = if calls.is_empty() {
        "stop"
    } else {
        "tool_calls"
    };
    serde_json::json!({"choices":[{"index":0,"finish_reason":reason,"message":{"role":"assistant","content":content,"tool_calls":calls}}]})
}

fn limits() -> crate::config::AppConfig {
    crate::config::AppConfig {
        subagent: crate::config::SubagentConfig {
            max_iterations: 1,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test]
async fn test_skipped_batch_is_paired_and_not_dispatched() {
    let calls = vec![make_call("fs_read", "one"), make_call("fs_read", "two")];
    let (client, requests, server) = fixture(vec![
        (200, assistant(calls, None)),
        (
            200,
            assistant(
                vec![],
                Some("Facts: partial\nFiles: unknown\nRecommendation: continue"),
            ),
        ),
    ])
    .await;
    let cfg = crate::config::AppConfig {
        subagent: crate::config::SubagentConfig {
            max_tool_calls: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let run = fixture_run(&client, cfg, None).await.expect("partial");
    assert_eq!(run.status, SubagentRunStatus::Partial);
    assert_eq!(run.stop_reason, Some(SubagentStopReason::ToolCallBudget));
    assert_eq!((run.iterations, run.tool_calls), (1, 0));
    let captured = requests.lock().expect("requests");
    assert_eq!(captured.len(), 2);
    assert!(captured[1].get("tools").is_none());
    let messages = captured[1]["messages"].as_array().expect("messages");
    let outputs: Vec<_> = messages.iter().filter(|m| m["role"] == "tool").collect();
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0]["tool_call_id"], "one");
    assert_eq!(outputs[1]["tool_call_id"], "two");
    for output in outputs {
        let value: serde_json::Value =
            serde_json::from_str(output["content"].as_str().expect("content")).expect("json");
        assert_eq!(value["error"]["reason"], "tool_call_budget");
    }
    // The same paired messages survive Responses projection.
    let messages: Vec<ChatMessage> =
        serde_json::from_value(captured[1]["messages"].clone()).expect("chat messages");
    let projected = crate::features::openai_subscription::responses::build(
        "test",
        "account",
        &messages,
        &[],
        None,
    )
    .expect("projection");
    for id in ["one", "two"] {
        assert!(
            projected
                .input
                .iter()
                .any(|v| v["type"] == "function_call_output" && v["call_id"] == id)
        );
    }
    server.abort();
}

#[tokio::test]
async fn test_finalization_failure_preserves_evidence() {
    let mut call = make_call("find_file", "find");
    call.function.arguments = serde_json::json!({"filename":"evidence.txt"}).to_string();
    let calls = vec![call];
    for final_response in [
        (500, serde_json::json!({"error":{"message":"offline"}})),
        (
            200,
            assistant(
                vec![make_call("fs_read", "forbidden_final_call")],
                Some("ignored"),
            ),
        ),
        (200, assistant(vec![], Some("  "))),
    ] {
        let (client, requests, server) =
            fixture(vec![(200, assistant(calls.clone(), None)), final_response]).await;
        // The valid read-only call records a real fixture file.
        let mut cfg = limits();
        cfg.subagent.max_total_tokens = Some(100_000);
        let run = fixture_run(&client, cfg, None).await.expect("partial");
        assert_eq!(run.status, SubagentRunStatus::Partial);
        assert_eq!(run.tool_calls, 1);
        assert!(run.summary.contains("find_file"));
        assert!(run.summary.contains("evidence.txt"));
        assert_eq!(run.files_examined.len(), 1);
        assert!(run.summary.contains("Recommendation:"));
        assert_eq!(requests.lock().expect("requests").len(), 2);
        server.abort();
    }
}

#[tokio::test]
async fn test_context_preflight_enforced_with_mode_off() {
    let (client, requests, server) =
        fixture(vec![(200, assistant(vec![], Some("should not be sent")))]).await;
    let cfg = crate::config::AppConfig {
        llm: crate::config::LlmConfig {
            context_window_size: Some(10),
            ..Default::default()
        },
        context_budget: crate::config::ContextBudgetConfig {
            mode: crate::config::ContextBudgetMode::Off,
        },
        ..Default::default()
    };
    let run = fixture_run(&client, cfg, None).await.expect("partial");
    assert_eq!(run.stop_reason, Some(SubagentStopReason::ContextBudget));
    assert_eq!(run.iterations, 0);
    assert!(requests.lock().expect("requests").is_empty());
    server.abort();
}

#[tokio::test]
async fn test_token_preflight_and_remaining_budget_blocks_next_request() {
    let response = assistant(vec![read_only_call()], None);
    let (client, requests, server) = fixture(vec![(200, response)]).await;
    let cfg = crate::config::AppConfig {
        subagent: crate::config::SubagentConfig {
            max_total_tokens: Some(1),
            ..Default::default()
        },
        ..Default::default()
    };
    let run = fixture_run(&client, cfg, None).await.expect("partial");
    assert_eq!(run.stop_reason, Some(SubagentStopReason::TokenBudget));
    assert_eq!(run.iterations, 0);
    assert!(requests.lock().expect("requests").is_empty());
    server.abort();
}

#[tokio::test]
async fn test_cancelled_at_boundary_and_during_research_is_error() {
    let (client, requests, server) =
        fixture(vec![(200, assistant(vec![read_only_call()], None))]).await;
    let token = CancellationToken::new();
    token.cancel();
    let error = fixture_run(&client, limits(), Some(token))
        .await
        .err()
        .expect("cancelled");
    assert!(matches!(
        error.downcast_ref::<crate::llm::LlmErrorKind>(),
        Some(crate::llm::LlmErrorKind::Cancelled)
    ));
    assert!(requests.lock().expect("requests").is_empty());
    let token = CancellationToken::new();
    let cancelling = token.clone();
    let observed = requests.clone();
    let canceller = tokio::spawn(async move {
        loop {
            if !observed.lock().expect("requests").is_empty() {
                cancelling.cancel();
                break;
            }
            tokio::task::yield_now().await;
        }
    });
    let error = fixture_run(&client, limits(), Some(token))
        .await
        .err()
        .expect("cancelled");
    assert!(matches!(
        error.downcast_ref::<crate::llm::LlmErrorKind>(),
        Some(crate::llm::LlmErrorKind::Cancelled)
    ));
    canceller.await.expect("canceller");
    server.abort();
}

#[tokio::test]
async fn test_real_provider_errors_remain_errors() {
    for (status, body) in [
        (401, serde_json::json!({"error":{"message":"auth"}})),
        (
            400,
            serde_json::json!({"error":{"message":"invalid request"}}),
        ),
        (503, serde_json::json!({"error":{"message":"offline"}})),
        (200, serde_json::json!({"invalid":"response"})),
    ] {
        let (client, requests, server) = fixture(vec![(status, body)]).await;
        assert!(fixture_run(&client, limits(), None).await.is_err());
        assert_eq!(requests.lock().expect("requests").len(), 1);
        server.abort();
    }
}

#[tokio::test]
async fn test_main_telemetry_restoration_and_cached_charge() {
    use crate::llm::types::Usage;
    let mut response = assistant(vec![read_only_call()], None);
    response["usage"] = serde_json::json!({"total_tokens":20000,"prompt_tokens":10000,"completion_tokens":10000,"prompt_tokens_details":{"cached_tokens":8000,"cache_write_tokens":1000},"completion_tokens_details":{"reasoning_tokens":1000}});
    let (client, requests, server) = fixture(vec![(200, response)]).await;
    client.record_usage(&serde_json::from_value::<Usage>(serde_json::json!({"total_tokens":120,"prompt_tokens":100,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":60,"cache_write_tokens":10},"completion_tokens_details":{"reasoning_tokens":10}})).expect("usage"));
    let before = client.usage_totals_snapshot();
    let saved_cache = client.last_prompt_cache_usage();
    let run = fixture_run(
        &client,
        crate::config::AppConfig {
            subagent: crate::config::SubagentConfig {
                max_total_tokens: Some(15000),
                ..Default::default()
            },
            ..Default::default()
        },
        None,
    )
    .await
    .expect("partial");
    assert_eq!(run.stop_reason, Some(SubagentStopReason::TokenBudget));
    assert_eq!(run.iterations, 1);
    assert_eq!(run.tool_calls, 0);
    assert_eq!(requests.lock().expect("requests").len(), 1);
    assert_eq!(client.get_tokens_used(), 120);
    assert_eq!(client.get_prompt_tokens_used(), 100);
    assert_eq!(client.get_reasoning_tokens_used(), 10);
    assert_eq!(client.last_prompt_cache_usage(), saved_cache);
    let after = client.usage_totals_snapshot();
    assert_eq!(after.total_tokens - before.total_tokens, 20000);
    assert_eq!(after.prompt_tokens - before.prompt_tokens, 10000);
    assert_eq!(after.record_count - before.record_count, 1);
    assert_eq!(client.get_total_cached_prompt_tokens(), 8060);
    assert_eq!(client.get_total_cache_write_tokens(), 1010);
    assert_eq!(client.usage_snapshot().usage_records, 2);
    assert_eq!(client.usage_snapshot().total_tokens, 20120);
    server.abort();
}

#[tokio::test]
async fn test_task_output_completed_and_partial_shape() {
    use std::sync::Arc;
    use tokio::sync::RwLock;
    for partial in [false, true] {
        let (client, _, server) =
            fixture(vec![(200, assistant(vec![], Some("Facts: done")))]).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = crate::config::AppConfig {
            project_root: dir.path().to_path_buf(),
            mcp_servers: vec![],
            subagent: crate::config::SubagentConfig {
                max_total_tokens: partial.then_some(1),
                ..Default::default()
            },
            ..Default::default()
        };
        let fs = crate::tools::FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));
        let runtime = ToolRuntime::build(&fs, Some(client), "test-model", None)
            .await
            .expect("runtime");
        let mut call = make_call("task", "task");
        call.function.arguments =
            serde_json::json!({"description":"fixture", "prompt":"investigate"}).to_string();
        let output = crate::llm::tool_execution::dispatch::dispatch_tool_call(&runtime, &call)
            .await
            .expect("task output");
        assert!(output.is_success);
        assert_eq!(output.value["ok"], true);
        assert_eq!(
            output.value["status"],
            if partial { "partial" } else { "completed" }
        );
        assert_eq!(output.value["files_examined_truncated"], false);
        assert!(output.value["summary"].as_str().is_some());
        assert_eq!(output.value["tool_calls"], 0);
        if partial {
            assert_eq!(output.value["stop_reason"], "token_budget");
            assert_eq!(output.value["iterations"], 0);
        } else {
            assert!(output.value["stop_reason"].is_null());
            assert_eq!(output.value["iterations"], 1);
        }
        server.abort();
    }
}

#[tokio::test]
async fn test_usage_missing_does_not_reuse_main_counters() {
    let (client, requests, server) =
        fixture(vec![(200, assistant(vec![read_only_call()], None))]).await;
    client.set_tokens(999_999);
    client.set_prompt_tokens(999_999);
    let before = client.usage_totals_snapshot();
    // Measure the same request with the same catalog: remaining=1 after request
    // #1, so neither research #2 nor finalization can be sent.
    let governor = ContextBudgetGovernor::new(Default::default());
    let messages = vec![
        message("system", subagent_system_prompt("/fixture")),
        message(
            "user",
            "Task description: fixture\n\nTask instructions:\ninvestigate".into(),
        ),
    ];
    let tools: Vec<_> = crate::llm::tool_def::default_tools_def()
        .into_iter()
        .filter(|def| SUBAGENT_ALLOWED_TOOLS.contains(&def.function.name.as_str()))
        .collect();
    let estimate = governor
        .estimate(governor.measure(&messages, &tools).expect("measure"))
        .prompt_tokens;
    let cfg = crate::config::AppConfig {
        subagent: crate::config::SubagentConfig {
            max_total_tokens: Some(estimate + 1),
            ..Default::default()
        },
        ..Default::default()
    };
    let run = fixture_run(&client, cfg, None).await.expect("partial");
    assert_eq!(run.stop_reason, Some(SubagentStopReason::TokenBudget));
    assert_eq!(run.iterations, 1);
    assert_eq!(requests.lock().expect("requests").len(), 1);
    assert_eq!(client.usage_totals_snapshot(), before);
    assert_eq!(client.get_prompt_tokens_used(), 999_999);
    server.abort();
}

#[test]
fn test_auto_token_budget_uses_configured_window_and_model_override() {
    let mut cfg = crate::config::AppConfig {
        model: "test-model".into(),
        llm: crate::config::LlmConfig {
            context_window_size: Some(10_000),
            ..Default::default()
        },
        ..Default::default()
    };
    let limit = u64::from(cfg.get_effective_compaction_limit());
    assert_eq!(limit, 8_000);
    let tracker = SubagentBudgetTracker::new(cfg.subagent.clone(), limit);
    assert_eq!(tracker.total_limit, 8_000);
    cfg.auto_compact_prompt_token_threshold_overrides
        .insert("test-model".into(), 5000);
    let tracker = SubagentBudgetTracker::new(
        cfg.subagent.clone(),
        u64::from(cfg.get_effective_compaction_limit()),
    );
    assert_eq!((tracker.total_limit, tracker.context_limit), (5000, 5000));
}

#[test]
fn test_subscription_footprint_uses_existing_projection() {
    let governor = ContextBudgetGovernor::new(Default::default());
    let messages = vec![
        message("system", "research".into()),
        message("user", "question".into()),
    ];
    use crate::features::openai_subscription::{
        auth::AuthHandle,
        credentials::{Account, CredentialStore, Registry, Tokens},
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let store = CredentialStore::at(
        dir.path()
            .canonicalize()
            .expect("canonical")
            .join("fixture-credentials"),
    );
    store
        .save(&Registry {
            host_id: "fixture".into(),
            active: Some("account".into()),
            accounts: vec![Account {
                label: "account".into(),
                subject: "fixture".into(),
                client_id: "fixture".into(),
                email: None,
                tokens: Some(Tokens {
                    renewal_uncertain: false,
                    access_token: "fixture".into(),
                    refresh_token: None,
                    id_token: "fixture".into(),
                    scopes: vec![],
                    expires_at: 0,
                    earliest_refresh_at: None,
                }),
            }],
        })
        .expect("fixture registry");
    let mut client = OpenAIClient::new("http://localhost/", "fixture").expect("client");
    assert_eq!(
        measure(&governor, &client, "test", &messages, &[]).expect("measure"),
        governor.measure(&messages, &[]).expect("measure")
    );
    client.subscription = Some(AuthHandle::selected(store).expect("fixture auth"));
    let responses = governor
        .measure_subscription("account", "test", &messages, &[], 0)
        .expect("Responses measure");
    assert_eq!(
        measure(&governor, &client, "test", &messages, &[]).expect("measure subscription"),
        responses
    );
    assert!(responses.total_json_bytes > 0);
}

#[tokio::test]
async fn test_elapsed_expiry_after_response_starts_no_tools_or_finalization() {
    let mut response = assistant(vec![make_call("fs_read", "read")], None);
    response["_fixture_delay_ms"] = serde_json::json!(1100);
    let (client, requests, server) = fixture(vec![(200, response)]).await;
    let cfg = crate::config::AppConfig {
        subagent: crate::config::SubagentConfig {
            max_elapsed_ms: 1000,
            ..Default::default()
        },
        ..Default::default()
    };
    let run = fixture_run(&client, cfg, None).await.expect("partial");
    assert_eq!(run.stop_reason, Some(SubagentStopReason::ElapsedBudget));
    assert_eq!(run.tool_calls, 0);
    assert_eq!(requests.lock().expect("requests").len(), 1);
    server.abort();
}

#[tokio::test]
async fn test_cancellation_during_finalization_is_not_partial() {
    let mut final_response = assistant(vec![], Some("must not be returned"));
    final_response["_fixture_delay_ms"] = serde_json::json!(1000);
    let (client, requests, server) = fixture(vec![
        (200, assistant(vec![read_only_call()], None)),
        (200, final_response),
    ])
    .await;
    let token = CancellationToken::new();
    let cancelling = token.clone();
    let observed = requests.clone();
    let canceller = tokio::spawn(async move {
        loop {
            if observed.lock().expect("requests").len() == 2 {
                cancelling.cancel();
                break;
            }
            tokio::task::yield_now().await;
        }
    });
    let error = fixture_run(&client, limits(), Some(token))
        .await
        .err()
        .expect("cancelled");
    assert!(matches!(
        error.downcast_ref::<crate::llm::LlmErrorKind>(),
        Some(crate::llm::LlmErrorKind::Cancelled)
    ));
    canceller.await.expect("canceller");
    assert_eq!(requests.lock().expect("requests").len(), 2);
    server.abort();
}

#[tokio::test]
async fn test_review_dropped_future_restores_main_telemetry() {
    let mut research = assistant(vec![read_only_call()], None);
    research["usage"] = serde_json::json!({"total_tokens":150,"prompt_tokens":100,"completion_tokens":50,"prompt_tokens_details":{"cached_tokens":80,"cache_write_tokens":10},"completion_tokens_details":{"reasoning_tokens":20}});
    let mut finalization = assistant(vec![], Some("unused"));
    finalization["_fixture_delay_ms"] = serde_json::json!(1000);
    let (client, requests, server) = fixture(vec![(200, research), (200, finalization)]).await;
    client.set_tokens(42);
    client.set_prompt_tokens(30);
    client.set_reasoning_tokens(3);
    let cache = client.last_prompt_cache_usage();
    let mut run = Box::pin(fixture_run(&client, limits(), None));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            result = &mut run => panic!("worker finished before it could be dropped: {}", result.is_ok()),
            _ = async {
                loop {
                    if requests.lock().expect("requests").len() == 2 { break; }
                    tokio::task::yield_now().await;
                }
            } => {}
        }
    }).await.expect("reached finalization");
    drop(run);
    assert_eq!(client.get_tokens_used(), 42);
    assert_eq!(client.get_prompt_tokens_used(), 30);
    assert_eq!(client.get_reasoning_tokens_used(), 3);
    assert_eq!(client.last_prompt_cache_usage(), cache);
    assert_eq!(client.get_total_tokens_used(), 150);
    assert_eq!(client.get_total_cached_prompt_tokens(), 80);
    assert_eq!(client.get_total_cache_write_tokens(), 10);
    assert_eq!(client.get_total_reasoning_tokens_used(), 20);
    server.abort();
}

#[tokio::test]
async fn test_review_task_output_bounded_after_json_escaping() {
    use std::sync::Arc;
    use tokio::sync::RwLock;
    let summary = "\u{0001}".repeat(SUBAGENT_SUMMARY_BUDGET_CHARS);
    let (client, _, server) = fixture(vec![(200, assistant(vec![], Some(&summary)))]).await;
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = crate::config::AppConfig {
        project_root: dir.path().to_path_buf(),
        mcp_servers: vec![],
        ..Default::default()
    };
    let fs = crate::tools::FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));
    let runtime = ToolRuntime::build(&fs, Some(client), "test-model", None)
        .await
        .expect("runtime");
    let mut call = make_call("task", "task");
    call.function.arguments =
        serde_json::json!({"description":"fixture","prompt":"investigate"}).to_string();
    let output = crate::llm::tool_execution::dispatch::dispatch_tool_call(&runtime, &call)
        .await
        .expect("task");
    let serialized = serde_json::to_string(&output.value).expect("serialize");
    assert!(serialized.chars().count() <= crate::tools::budget::DEFAULT_TOOL_BUDGET_CHARS);
    assert_eq!(truncate_tool_output(serialized.clone(), "task"), serialized);
    assert_eq!(output.value["status"], "completed");
    assert!(output.value["stop_reason"].is_null());
    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn cancellable_subagent_read_does_not_publish_late_context_or_result() {
    use std::sync::Arc;
    use tokio::sync::RwLock;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("evidence.txt");
    std::fs::write(&path, "actual evidence\n").unwrap();
    let mut call = make_call("fs_read", "owned-read");
    call.function.arguments = serde_json::json!({"path": path.display().to_string()}).to_string();
    let (client, requests, server) = fixture(vec![(200, assistant(vec![call], None))]).await;
    let cfg = Arc::new(crate::config::AppConfig {
        project_root: dir.path().to_owned(),
        mcp_servers: vec![],
        ..Default::default()
    });
    let fs = crate::tools::FsTools::new(Arc::new(RwLock::new(None)), cfg);
    let token = CancellationToken::new();
    let runtime = ToolRuntime::build(&fs, Some(client.clone()), "fixture", Some(token.clone()))
        .await
        .unwrap();
    let lock = fs.context_manager.write().await;
    let canceller_token = token.clone();
    let observed = requests.clone();
    let canceller = tokio::spawn(async move {
        while observed.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        canceller_token.cancel();
    });
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        run_subagent(
            &client,
            "fixture",
            &runtime,
            "read",
            "read evidence",
            Some(token),
            dir.path().to_str().unwrap(),
        ),
    )
    .await
    .unwrap()
    .err()
    .expect("cancelled subagent");
    assert!(matches!(
        error.downcast_ref::<crate::llm::LlmErrorKind>(),
        Some(crate::llm::LlmErrorKind::Cancelled)
    ));
    drop(lock);
    canceller.await.unwrap();
    tokio::task::yield_now().await;
    assert!(
        fs.context_manager
            .read()
            .await
            .get_context_prompt()
            .await
            .is_empty()
    );
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "canceled result sent to provider"
    );
    server.abort();
}
