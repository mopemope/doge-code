use super::*;
#[test]
fn routes_are_provider_specific_and_fail_closed() {
    assert_eq!(
        api(ProviderKind::OpencodeGo, "opencode-go/gpt-6-luna").unwrap(),
        ApiKind::Responses
    );
    assert_eq!(
        api(ProviderKind::OpencodeGo, "kimi-k2.6").unwrap(),
        ApiKind::ChatCompletions
    );
    assert!(api(ProviderKind::OpencodeGo, "qwen3.8-max").is_err());
    assert_eq!(
        api(ProviderKind::OpencodeZen, "qwen3.8-max").unwrap(),
        ApiKind::ChatCompletions
    );
    assert!(api(ProviderKind::OpencodeGo, "gpt-unknown").is_err());
    assert!(api(ProviderKind::OpencodeGo, "opencode/gpt-6-luna").is_err());
    assert!(validate_base(ProviderKind::OpencodeGo, ZEN_BASE).is_err());
    assert!(validate_base(ProviderKind::OpencodeZen, GO_BASE).is_err());
}

use crate::config::{LlmConfig, ReasoningMode};
use crate::llm::{ChatMessage, ChoiceMessageWithTools, OpenAIClient, ToolDef};
use httptest::{Expectation, matchers::*, responders::*};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn fixture_client(server: &httptest::Server, provider: ProviderKind) -> OpenAIClient {
    let path = if provider == ProviderKind::OpencodeGo {
        "/zen/go/v1"
    } else {
        "/zen/v1"
    };
    let mut client = OpenAIClient::new(server.url_str(path), "fixture-key")
        .expect("client")
        .with_llm_config(LlmConfig {
            max_retries: 1,
            retry_base_ms: 1,
            retry_jitter_ms: 0,
            request_timeout_ms: 2000,
            read_idle_timeout_ms: 1000,
            ..Default::default()
        });
    client.provider = provider;
    let config = client.llm_cfg.clone();
    let client = client.with_llm_config(config);
    client.for_conversation("conversation-a").expect("session")
}

fn message(role: &str, text: &str) -> ChatMessage {
    ChatMessage {
        reasoning: Default::default(),
        provider_state: None,
        role: role.into(),
        content: Some(text.into()),
        tool_calls: vec![],
        tool_call_id: None,
    }
}
fn assistant(reply: ChoiceMessageWithTools) -> ChatMessage {
    ChatMessage {
        reasoning: reply.reasoning,
        provider_state: reply.provider_state,
        role: reply.role,
        content: reply.content,
        tool_calls: reply.tool_calls,
        tool_call_id: None,
    }
}
fn text_output() -> Value {
    json!({"type":"message","id":"msg","role":"assistant","status":"completed",
        "content":[{"type":"output_text","text":"done"}]})
}
fn completed(output: Vec<Value>) -> String {
    format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":{
            "status":"completed","output":output,"usage":{"input_tokens":12,"output_tokens":4,
            "total_tokens":16,"input_tokens_details":{"cached_tokens":5},
            "output_tokens_details":{"reasoning_tokens":2}}
        }})
    )
}
async fn infer(
    client: &OpenAIClient,
    model: &str,
    history: &[ChatMessage],
    tools: &[ToolDef],
) -> anyhow::Result<ChoiceMessageWithTools> {
    crate::llm::tool_execution::requests::chat_tools_once(
        client,
        model,
        history,
        tools,
        None,
        ReasoningMode::Off,
        None,
        None,
    )
    .await
}

#[test]
fn api_key_configuration_never_loads_oauth_and_rejects_mismatched_bases() {
    let temp = tempfile::tempdir().expect("project");
    for (provider, base) in [
        (ProviderKind::OpencodeGo, GO_BASE),
        (ProviderKind::OpencodeZen, ZEN_BASE),
    ] {
        let mut cfg = crate::config::AppConfig {
            project_root: temp.path().to_owned(),
            provider,
            base_url: base.into(),
            model: "gpt-6-luna".into(),
            api_key: Some("fixture-key".into()),
            ..Default::default()
        };
        let client = OpenAIClient::from_config(&cfg)
            .expect("valid config")
            .expect("client");
        assert!(!client.is_subscription());
        assert!(client.api_key_responses(&cfg.model).expect("route"));
        assert!(!client.native_responses_compaction_enabled());
        assert!(!client.responses_identity().contains("fixture-key"));
        cfg.base_url = "https://api.openai.com/v1".into();
        assert!(OpenAIClient::from_config(&cfg).is_err());
    }
    assert_eq!(
        serde_json::from_str::<ProviderKind>("\"openai-compatible\"").expect("legacy"),
        ProviderKind::OpenaiCompatible
    );
    assert_eq!(
        serde_json::from_str::<ProviderKind>("\"opencode-go\"").expect("Go"),
        ProviderKind::OpencodeGo
    );
    assert_eq!(
        serde_json::from_str::<ProviderKind>("\"opencode-zen\"").expect("Zen"),
        ProviderKind::OpencodeZen
    );
}

#[tokio::test]
async fn responses_flat_tools_opaque_reasoning_and_session_round_trip() {
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    let client = fixture_client(&server, ProviderKind::OpencodeGo);
    let tools = vec![crate::tools::tool_search::tool_def()];
    server.expect(Expectation::matching(all_of![
        request::method_path("POST", "/zen/go/v1/responses"),
        request::headers(contains(("authorization", "Bearer fixture-key"))),
        request::headers(contains(("x-opencode-session", "conversation-a"))),
        request::headers(contains(("user-agent", concat!("dgc/", env!("CARGO_PKG_VERSION"))))),
        request::body(json_decoded(|body: &Value| {
            body["model"] == "gpt-6-luna" && body["tools"][0]["type"] == "function"
                && body["tools"][0]["name"] == "tool_search" && body["store"] == false
                && body["stream"] == true && body.get("context_management").is_none()
                && body.get("reasoning").is_none() && body["input"].as_array().is_some_and(|v| v.len() == 1)
                && body["include"][0] == "reasoning.encrypted_content"
        }))
    ]).times(1).respond_with(status_code(200).append_header("content-type", "text/event-stream")
        .body(completed(vec![
            json!({"type":"reasoning","id":"rs","summary":[],"encrypted_content":"opaque-fixture"}),
            json!({"type":"function_call","id":"fc","call_id":"call","name":"tool_search",
                "arguments":"{\"query\":\"read\"}","status":"completed"})
        ]))));
    server.expect(
        Expectation::matching(all_of![
            request::method_path("POST", "/zen/go/v1/responses"),
            request::headers(contains(("x-opencode-session", "conversation-a"))),
            request::body(json_decoded(|body: &Value| {
                let input = body["input"].as_array().expect("input");
                input
                    .iter()
                    .any(|i| i["encrypted_content"] == "opaque-fixture")
                    && input
                        .iter()
                        .any(|i| i["type"] == "function_call_output" && i["call_id"] == "call")
                    && input
                        .iter()
                        .all(|i| i.get("namespace").is_none() && i["type"] != "additional_tools")
            }))
        ])
        .times(1)
        .respond_with(
            status_code(200)
                .append_header("content-type", "text/event-stream")
                .body(completed(vec![text_output()])),
        ),
    );
    let mut history = vec![message("user", "read the project")];
    let reply = infer(&client, "opencode-go/gpt-6-luna", &history, &tools)
        .await
        .expect("tool reply");
    assert_eq!(reply.tool_calls.len(), 1);
    history.push(assistant(reply));
    let mut result = message("tool", "{\"ok\":true}");
    result.tool_call_id = Some("call".into());
    history.push(result);
    let mut session = crate::session::SessionData::new();
    session
        .replace_conversation_messages(&history)
        .expect("save");
    history = session.conversation_messages().expect("resume");
    let reply = infer(&client.clone(), "gpt-6-luna", &history, &tools)
        .await
        .expect("final");
    assert_eq!(reply.content.as_deref(), Some("done"));
    assert_eq!(client.get_total_tokens_used(), 32);
    assert_eq!(client.get_total_cached_prompt_tokens(), 10);
    assert_eq!(client.get_total_reasoning_tokens_used(), 4);
}

#[tokio::test]
async fn chat_reasoning_survives_tool_result_and_session_without_unknown_knobs() {
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    let client = fixture_client(&server, ProviderKind::OpencodeGo);
    let tools = vec![crate::tools::tool_search::tool_def()];
    server.expect(Expectation::matching(all_of![
        request::method_path("POST", "/zen/go/v1/chat/completions"),
        request::headers(contains(("x-opencode-session", "conversation-a"))),
        request::headers(contains(("user-agent", concat!("dgc/", env!("CARGO_PKG_VERSION"))))),
        request::body(json_decoded(|body: &Value| body["model"] == "kimi-k2.6"
            && body.get("reasoning_effort").is_none() && body["messages"].as_array().expect("msgs").len() == 1))
    ]).times(1).respond_with(json_encoded(json!({"choices":[{"index":0,"finish_reason":"tool_calls","message":{
        "role":"assistant","content":null,"reasoning_content":"fixture-reasoning",
        "reasoning_details":[{"type":"reasoning.encrypted","data":"fixture-data"}],
        "tool_calls":[{"id":"call","type":"function","function":{"name":"tool_search","arguments":"{\"query\":\"read\"}"}}]
    }}],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}))));
    server.expect(
        Expectation::matching(all_of![
            request::method_path("POST", "/zen/go/v1/chat/completions"),
            request::headers(contains(("x-opencode-session", "conversation-a"))),
            request::body(json_decoded(
                |body: &Value| body["messages"][1]["reasoning_content"] == "fixture-reasoning"
                    && body["messages"][1]["reasoning_details"][0]["data"] == "fixture-data"
                    && body["messages"][2]["tool_call_id"] == "call"
            ))
        ])
        .times(1)
        .respond_with(json_encoded(
            json!({"choices":[{"index":0,"finish_reason":"stop",
        "message":{"role":"assistant","content":"done"}}]}),
        )),
    );
    let mut history = vec![message("user", "hi")];
    let reply = infer(&client, "opencode-go/kimi-k2.6", &history, &tools)
        .await
        .expect("tool");
    assert!(!format!("{:?}", reply.reasoning).contains("fixture-reasoning"));
    history.push(assistant(reply));
    let mut result = message("tool", "ok");
    result.tool_call_id = Some("call".into());
    history.push(result);
    let mut session = crate::session::SessionData::new();
    session
        .replace_conversation_messages(&history)
        .expect("save");
    let reply = infer(
        &client,
        "kimi-k2.6",
        &session.conversation_messages().expect("resume"),
        &tools,
    )
    .await
    .expect("final");
    assert_eq!(reply.content.as_deref(), Some("done"));
    assert_eq!(client.get_total_tokens_used(), 12);
}

#[tokio::test]
async fn responses_failure_eof_done_and_cancel_never_commit_partial_calls_or_usage() {
    for wire in [
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
        "data: [DONE]\n\n",
        "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"upstream_error\"}}}\n\n",
        "data: {\"type\":\"response.incomplete\",\"response\":{}}\n\n",
    ] {
        let server = crate::test_support::HTTP_SERVER_POOL.get_server();
        let client = fixture_client(&server, ProviderKind::OpencodeGo);
        server.expect(
            Expectation::matching(request::method_path("POST", "/zen/go/v1/responses"))
                .times(1)
                .respond_with(
                    status_code(200)
                        .append_header("content-type", "text/event-stream")
                        .body(wire),
                ),
        );
        assert!(
            infer(&client, "gpt-6-luna", &[message("user", "hi")], &[])
                .await
                .is_err()
        );
        assert_eq!(client.get_total_tokens_used(), 0);
        assert_eq!(client.usage_totals_snapshot().record_count, 0);
    }
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    let client = fixture_client(&server, ProviderKind::OpencodeGo);
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(
        client
            .chat_once("gpt-6-luna", vec![message("user", "hi")], Some(cancel))
            .await
            .is_err()
    );
    assert_eq!(client.usage_totals_snapshot().record_count, 0);
}

#[tokio::test]
async fn responses_rate_limit_retry_reuses_identity_and_quota_does_not_retry_or_fallback() {
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    let client = fixture_client(&server, ProviderKind::OpencodeGo);
    server.expect(
        Expectation::matching(all_of![
            request::method_path("POST", "/zen/go/v1/responses"),
            request::headers(contains(("x-opencode-session", "conversation-a")))
        ])
        .times(2)
        .respond_with(cycle![
            status_code(429)
                .append_header("retry-after", "0")
                .body("{\"error\":{\"code\":\"rate_limit_exceeded\"}}"),
            status_code(200)
                .append_header("content-type", "text/event-stream")
                .body(completed(vec![text_output()]))
        ]),
    );
    assert_eq!(
        client
            .chat_once("gpt-6-luna", vec![message("user", "hi")], None)
            .await
            .expect("retry")
            .content,
        "done"
    );
    assert_eq!(client.usage_snapshot().attempts, 2);
    drop(server);
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    let client = fixture_client(&server, ProviderKind::OpencodeGo);
    server.expect(
        Expectation::matching(request::method_path("POST", "/zen/go/v1/responses"))
            .times(1)
            .respond_with(status_code(429).body("{\"error\":{\"code\":\"insufficient_quota\"}}")),
    );
    let error = client
        .chat_once("gpt-6-luna", vec![message("user", "hi")], None)
        .await
        .expect_err("quota");
    assert!(
        !error
            .downcast_ref::<crate::features::openai_subscription::ProviderError>()
            .expect("provider error")
            .retryable
    );
    assert_eq!(client.usage_snapshot().attempts, 1);
}

#[tokio::test]
async fn responses_tools_free_stream_and_compaction_use_same_conversation_header() {
    use futures::StreamExt;
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    let client = fixture_client(&server, ProviderKind::OpencodeGo);
    server.expect(
        Expectation::matching(all_of![
            request::method_path("POST", "/zen/go/v1/responses"),
            request::headers(contains(("x-opencode-session", "conversation-a"))),
            request::body(json_decoded(|body: &Value| body["tools"] == json!([])
                && !body.to_string().contains("opaque-fixture")
                && !body.to_string().contains("fixture-reasoning")))
        ])
        .times(2)
        .respond_with(
            status_code(200)
                .append_header("content-type", "text/event-stream")
                .body(completed(vec![text_output()])),
        ),
    );
    let mut stream = client
        .chat_stream("gpt-6-luna", &[message("user", "hi")], None)
        .await
        .expect("stream");
    assert_eq!(stream.next().await.expect("chunk").expect("text"), "done");
    assert!(stream.next().await.is_none());
    let mut previous = message("assistant", "prior answer");
    previous.provider_state = Some(
        crate::features::openai_subscription::ProviderState::assistant(
            client.responses_identity(),
            "gpt-6-luna".into(),
            vec![
                json!({"type":"reasoning","id":"rs","summary":[],"encrypted_content":"opaque-fixture"}),
                text_output(),
            ],
        ),
    );
    let compact = crate::llm::compact_conversation_history_ref(
        &client,
        "gpt-6-luna",
        &[
            message("user", "previous task"),
            previous,
            message("user", "continue"),
        ],
    )
    .await
    .expect("summary");
    assert!(compact.metadata.success);
}

#[tokio::test]
async fn immutable_conversation_clones_and_chat_auxiliary_headers() {
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    let client = fixture_client(&server, ProviderKind::OpencodeGo);
    let other = client
        .for_conversation("conversation-b")
        .expect("new conversation");
    for id in ["conversation-a", "conversation-b"] {
        server.expect(
            Expectation::matching(all_of![
                request::method_path("POST", "/zen/go/v1/chat/completions"),
                request::headers(contains(("x-opencode-session", id))),
                request::headers(contains((
                    "user-agent",
                    concat!("dgc/", env!("CARGO_PKG_VERSION"))
                ))),
                request::body(json_decoded(|body: &Value| body["model"] == "kimi-k2.6"))
            ])
            .times(1)
            .respond_with(json_encoded(
                json!({"choices":[{"index":0,"finish_reason":"stop",
            "message":{"role":"assistant","content":"done"}}]}),
            )),
        );
    }
    for c in [&client, &other] {
        assert_eq!(
            c.chat_once("opencode-go/kimi-k2.6", vec![message("user", "hi")], None)
                .await
                .expect("aux")
                .content,
            "done"
        );
    }
    assert_eq!(client.opencode_session, "conversation-a");
    assert_eq!(other.opencode_session, "conversation-b");
    assert!(client.for_conversation("bad\r\nheader").is_err());
}

#[test]
fn standard_responses_rejects_oauth_markers_namespaces_and_cross_provider_history() {
    use crate::features::openai_subscription::{ProviderState, responses::build_api_key};
    let mut msg = message("developer", "");
    msg.provider_state = Some(ProviderState::activation(
        "identity".into(),
        "gpt-6-luna".into(),
        vec!["tool_search".into()],
    ));
    assert!(build_api_key("gpt-6-luna", "identity", &[msg], &[], None).is_err());
    let mut msg = message("assistant", "prior");
    msg.provider_state = Some(ProviderState::assistant(
        "identity".into(),
        "gpt-6-luna".into(),
        vec![
            json!({"type":"function_call","call_id":"call","name":"tool_search","namespace":"dgc","arguments":"{}"}),
        ],
    ));
    assert!(build_api_key("gpt-6-luna", "identity", &[msg], &[], None).is_err());
    let mut msg = message("assistant", "prior");
    msg.provider_state = Some(ProviderState::assistant(
        "other-provider".into(),
        "gpt-6-luna".into(),
        vec![text_output()],
    ));
    assert!(build_api_key("gpt-6-luna", "identity", &[msg], &[], None).is_err());
}

#[test]
fn inference_configuration_does_not_cross_provider_or_legacy_scopes() {
    for source in [
        None,
        Some(ProviderKind::OpenaiCompatible),
        Some(ProviderKind::Openai),
        Some(ProviderKind::OpencodeZen),
    ] {
        assert!(config_value(ProviderKind::OpencodeGo, source, Some("fixture-key")).is_none());
    }
    assert_eq!(
        config_value(
            ProviderKind::OpencodeGo,
            Some(ProviderKind::OpencodeGo),
            Some("fixture-key")
        ),
        Some("fixture-key")
    );
    assert_eq!(
        config_value(ProviderKind::OpenaiCompatible, None, Some("legacy-key")),
        Some("legacy-key")
    );
    for target in [
        ProviderKind::OpenaiCompatible,
        ProviderKind::Openai,
        ProviderKind::OpencodeZen,
    ] {
        assert!(
            config_value(target, Some(ProviderKind::OpencodeGo), Some("fixture-key")).is_none()
        );
    }
    assert!(
        config_value(
            ProviderKind::OpenaiCompatible,
            Some(ProviderKind::OpencodeZen),
            Some("fixture-key")
        )
        .is_none()
    );
    let parsed: crate::config::FileConfig =
        toml::from_str("provider = 'opencode-go'\nmodel = 'opencode-go/gpt-6-luna'\n")
            .expect("new config");
    assert_eq!(parsed.provider, Some(ProviderKind::OpencodeGo));
    assert!(parsed.api_key.is_none());
    let legacy: crate::config::FileConfig =
        toml::from_str("model = 'gpt-4o-mini'\n").expect("legacy config");
    assert!(legacy.provider.is_none());
}

#[tokio::test]
async fn zen_chat_and_go_chat_stream_use_distinct_routes_and_headers() {
    use futures::StreamExt;
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    let zen = fixture_client(&server, ProviderKind::OpencodeZen);
    server.expect(Expectation::matching(all_of![
        request::method_path("POST", "/zen/v1/chat/completions"),
        request::headers(contains(("authorization", "Bearer fixture-key"))),
        request::headers(contains(("x-opencode-session", "conversation-a"))),
        request::body(json_decoded(|body: &Value| body["model"] == "qwen3.8-max" && body.get("reasoning_effort").is_none()))
    ]).times(1).respond_with(json_encoded(json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"zen"}}]}))));
    assert_eq!(
        infer(&zen, "opencode/qwen3.8-max", &[message("user", "hi")], &[])
            .await
            .expect("Zen")
            .content
            .as_deref(),
        Some("zen")
    );
    let go = fixture_client(&server, ProviderKind::OpencodeGo);
    server.expect(Expectation::matching(all_of![
        request::method_path("POST", "/zen/go/v1/chat/completions"),
        request::headers(contains(("x-opencode-session", "conversation-a"))),
        request::headers(contains(("user-agent", concat!("dgc/", env!("CARGO_PKG_VERSION")))))
    ]).times(1).respond_with(status_code(200).append_header("content-type", "text/event-stream").body(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"go\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
    )));
    let mut stream = go
        .chat_stream("kimi-k2.6", &[message("user", "hi")], None)
        .await
        .expect("Go stream");
    let mut text = String::new();
    while let Some(chunk) = stream.next().await {
        text.push_str(&chunk.expect("chunk"));
    }
    assert_eq!(text, "go");
    assert!(
        infer(&go, "qwen3.8-max", &[message("user", "hi")], &[])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn responses_refusal_cannot_become_auxiliary_edit_text_and_usage_is_retained() {
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    let client = fixture_client(&server, ProviderKind::OpencodeGo);
    server.expect(
        Expectation::matching(request::method_path("POST", "/zen/go/v1/responses"))
            .times(1)
            .respond_with(
                status_code(200)
                    .append_header("content-type", "text/event-stream")
                    .body(completed(vec![
                        json!({"type":"message","id":"msg","role":"assistant","status":"completed",
                "content":[{"type":"refusal","refusal":"declined"}]}),
                    ])),
            ),
    );
    let error = client
        .chat_once("gpt-6-luna", vec![message("user", "edit")], None)
        .await
        .expect_err("refusal");
    assert_eq!(
        error.downcast_ref::<crate::llm::LlmErrorKind>(),
        Some(&crate::llm::LlmErrorKind::Incomplete)
    );
    assert_eq!(client.get_total_tokens_used(), 16);
}

#[tokio::test]
async fn local_responses_compaction_protects_unseen_opaque_tool_batch_and_can_continue() {
    use crate::llm::tool_execution::history::HistoryManager;
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    let client = fixture_client(&server, ProviderKind::OpencodeGo);
    let temp = tempfile::tempdir().expect("project");
    let cfg = crate::config::AppConfig {
        project_root: temp.path().to_owned(),
        provider: ProviderKind::OpencodeGo,
        model: "gpt-6-luna".into(),
        no_repomap: true,
        ..Default::default()
    };
    let repomap = std::sync::Arc::new(tokio::sync::RwLock::new(None));
    let fs = crate::tools::FsTools::new(repomap, std::sync::Arc::new(cfg.clone()));
    let mut history = HistoryManager::new(
        client.clone(),
        vec![
            message("system", "Coding agent"),
            message("user", "original task"),
            message("assistant", "completed historical work"),
            message("user", "follow-up"),
            message("assistant", "completed follow-up"),
        ],
        None,
        fs,
        cfg,
    )
    .without_checkpoints();
    let raw = vec![
        json!({"type":"reasoning","id":"rs-protected","summary":[],"encrypted_content":"unseen-opaque"}),
        json!({"type":"function_call","id":"fc-protected","call_id":"unseen-call","name":"tool_search","arguments":"{\"query\":\"read\"}","status":"completed"}),
    ];
    let (reply, _) = crate::features::openai_subscription::responses::completed(
        &json!({"status":"completed","output":raw}),
        &client.responses_identity(),
        "gpt-6-luna",
        &[crate::tools::tool_search::tool_def()],
    )
    .expect("assistant");
    history.push(assistant(reply));
    history.push_tool_result(Some("unseen-call".into()), "unseen-tool-result".into());
    let before =
        serde_json::to_value(&history.as_slice()[history.len() - 2..]).expect("protected suffix");
    server.expect(
        Expectation::matching(all_of![
            request::method_path("POST", "/zen/go/v1/responses"),
            request::headers(contains(("x-opencode-session", "conversation-a"))),
            request::body(json_decoded(|body: &Value| body["tools"] == json!([])
                && !body.to_string().contains("unseen-opaque")
                && !body.to_string().contains("unseen-tool-result")))
        ])
        .times(1)
        .respond_with(
            status_code(200)
                .append_header("content-type", "text/event-stream")
                .body(completed(vec![text_output()])),
        ),
    );
    assert!(
        history
            .compact_manually(CancellationToken::new())
            .await
            .expect("compact")
    );
    assert_eq!(history.unseen_count(), 1);
    let after = serde_json::to_value(&history.as_slice()[history.len() - 2..]).expect("suffix");
    assert_eq!(before, after);
    server.expect(
        Expectation::matching(all_of![
            request::method_path("POST", "/zen/go/v1/responses"),
            request::headers(contains(("x-opencode-session", "conversation-a"))),
            request::body(json_decoded(|body: &Value| body["input"]
                .as_array()
                .expect("input")
                .iter()
                .any(|i| i["encrypted_content"] == "unseen-opaque")
                && body["input"]
                    .as_array()
                    .expect("input")
                    .iter()
                    .any(|i| i["type"] == "function_call_output"
                        && i["output"] == "unseen-tool-result")))
        ])
        .times(1)
        .respond_with(
            status_code(200)
                .append_header("content-type", "text/event-stream")
                .body(completed(vec![text_output()])),
        ),
    );
    let reply = infer(
        &client,
        "gpt-6-luna",
        history.as_slice(),
        &[crate::tools::tool_search::tool_def()],
    )
    .await
    .expect("continue");
    assert_eq!(reply.content.as_deref(), Some("done"));
}

#[tokio::test]
async fn main_agent_uses_persisted_conversation_ids_and_subagent_inherits_snapshot() {
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    let client = fixture_client(&server, ProviderKind::OpencodeGo);
    let temp = tempfile::tempdir().expect("project");
    let cfg = crate::config::AppConfig {
        project_root: temp.path().to_owned(),
        model: "gpt-6-luna".into(),
        no_repomap: true,
        show_diff: false,
        ..Default::default()
    };
    let store = crate::session::SessionStore::new(temp.path().join("sessions")).expect("store");
    let manager = std::sync::Arc::new(std::sync::Mutex::new(
        crate::session::SessionManager::with_store(store),
    ));
    manager
        .lock()
        .expect("manager")
        .create_session(None)
        .expect("session");
    let first_id = manager
        .lock()
        .expect("manager")
        .get_current_session_id()
        .expect("id");
    let repomap = std::sync::Arc::new(tokio::sync::RwLock::new(None));
    let fs = crate::tools::FsTools::new(repomap, std::sync::Arc::new(cfg.clone()))
        .with_session_manager(manager.clone());
    server.expect(
        Expectation::matching(all_of![
            request::method_path("POST", "/zen/go/v1/responses"),
            request::headers(contains(("x-opencode-session", first_id.clone())))
        ])
        .times(2)
        .respond_with(
            status_code(200)
                .append_header("content-type", "text/event-stream")
                .body(completed(vec![text_output()])),
        ),
    );
    let run = crate::llm::run_agent_loop(
        &client,
        "gpt-6-luna",
        &fs,
        vec![message("user", "Explain only")],
        None,
        None,
        &cfg,
        None,
        crate::provenance::ProvenanceAttribution::none(),
    )
    .await
    .expect("main agent");
    assert_eq!(run.final_message.content, "done");
    let worker_client = client.for_conversation(&first_id).expect("worker snapshot");
    let runtime = crate::llm::tool_runtime::ToolRuntime::build(
        &fs,
        Some(worker_client.clone()),
        "gpt-6-luna",
        None,
    )
    .await
    .expect("runtime");
    let worker = crate::llm::tool_execution::run_subagent(
        &worker_client,
        "gpt-6-luna",
        &runtime,
        "Research",
        "Explain only",
        None,
        temp.path().to_str().expect("path"),
    )
    .await
    .expect("worker");
    assert_eq!(worker.summary, "done");
    manager
        .lock()
        .expect("manager")
        .create_session(None)
        .expect("new session");
    let next_id = manager
        .lock()
        .expect("manager")
        .get_current_session_id()
        .expect("id");
    assert_ne!(first_id, next_id);
    server.expect(
        Expectation::matching(all_of![
            request::method_path("POST", "/zen/go/v1/responses"),
            request::headers(contains(("x-opencode-session", next_id)))
        ])
        .times(1)
        .respond_with(
            status_code(200)
                .append_header("content-type", "text/event-stream")
                .body(completed(vec![text_output()])),
        ),
    );
    let run = crate::llm::run_agent_loop(
        &client,
        "gpt-6-luna",
        &fs,
        vec![message("user", "New question")],
        None,
        None,
        &cfg,
        None,
        crate::provenance::ProvenanceAttribution::none(),
    )
    .await
    .expect("new main agent");
    assert_eq!(run.final_message.content, "done");
    assert_eq!(worker_client.opencode_session, first_id);
}

#[tokio::test]
async fn opencode_redirects_do_not_switch_billing_routes() {
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    let client = fixture_client(&server, ProviderKind::OpencodeGo);
    server.expect(
        Expectation::matching(request::method_path("POST", "/zen/go/v1/responses"))
            .times(1)
            .respond_with(
                status_code(307)
                    .append_header("location", server.url_str("/zen/v1/responses"))
                    .body("{\"error\":{\"code\":\"wrong_route\"}}"),
            ),
    );
    assert!(
        client
            .chat_once("gpt-6-luna", vec![message("user", "hi")], None)
            .await
            .is_err()
    );
    assert_eq!(client.usage_snapshot().attempts, 1);
}

#[test]
fn catalog_entries_are_unique_and_match_runtime_routes() {
    for provider in [ProviderKind::OpencodeGo, ProviderKind::OpencodeZen] {
        let mut ids = std::collections::HashSet::new();
        for spec in catalog(provider) {
            assert!(ids.insert(spec.id), "duplicate {}", spec.id);
            assert_eq!(api(provider, spec.id).ok(), spec.api.adapter());
            assert_eq!(lookup(provider, spec.id).unwrap().api, spec.api);
        }
        assert!(
            catalog(provider)
                .iter()
                .any(|spec| spec.api.adapter().is_none())
        );
    }
    assert_eq!(
        lookup(ProviderKind::OpencodeGo, "qwen3.8-max")
            .unwrap()
            .api
            .name(),
        "Messages"
    );
    assert_eq!(
        lookup(ProviderKind::OpencodeZen, "qwen3.8-max")
            .unwrap()
            .api
            .name(),
        "Chat Completions"
    );
    assert!(lookup(ProviderKind::OpencodeGo, "opencode/gpt-6-luna").is_err());
}
