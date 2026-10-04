use super::*;
use crate::llm::{ChatMessage, ToolDef};
use anyhow::Result;
use credentials::{Account, CredentialStore, Registry, Tokens};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn temp_store() -> (tempfile::TempDir, CredentialStore) {
    let temp = tempfile::tempdir().expect("temporary credential directory");
    let root = temp
        .path()
        .canonicalize()
        .expect("canonical temp path")
        .join("credentials");
    (temp, CredentialStore::at(root))
}
fn registry() -> Registry {
    Registry {
        host_id: "urn:uuid:test-host".into(),
        active: Some("test-account".into()),
        accounts: vec![Account {
            label: "test-account".into(),
            subject: "test-subject".into(),
            client_id: "test-client".into(),
            email: None,
            tokens: Some(Tokens {
                renewal_uncertain: false,
                access_token: "test-access-secret".into(),
                refresh_token: Some("test-refresh-secret".into()),
                id_token: "test-id-secret".into(),
                scopes: vec!["chatgpt.tokens.use.direct".into()],
                expires_at: auth::now() + 3600,
                earliest_refresh_at: None,
            }),
        }],
    }
}
fn user(text: &str) -> ChatMessage {
    ChatMessage {
        provider_state: None,
        role: "user".into(),
        content: Some(text.into()),
        tool_calls: vec![],
        tool_call_id: None,
    }
}
fn tools() -> Vec<ToolDef> {
    vec![crate::tools::tool_search::tool_def()]
}
fn response(output: Vec<Value>) -> Value {
    json!({"status":"completed","output":output,"usage":{"input_tokens":12,"output_tokens":4,"total_tokens":16,"input_tokens_details":{"cached_tokens":0},"output_tokens_details":{"reasoning_tokens":0}}})
}
fn text_output() -> Value {
    json!({"type":"message","id":"msg_test","role":"assistant","status":"completed","content":[{"type":"output_text","text":"こんにちは"}]})
}
fn call_output() -> Value {
    json!({"type":"function_call","id":"fc_test","call_id":"call_test","namespace":"dgc","name":"tool_search","arguments":"{\"query\":\"read\"}","status":"completed"})
}

#[test]
fn pkce_matches_rfc_vector() {
    assert_eq!(
        auth::challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}
#[test]
fn callback_rejects_wrong_state() {
    assert!(auth::parse_callback("/auth/callback?code=x&state=wrong", "right").is_err());
}
#[test]
fn callback_rejects_duplicate_parameter() {
    assert!(auth::parse_callback("/auth/callback?code=x&state=s&state=s", "s").is_err());
}
#[test]
fn callback_rejects_declined_consent() {
    assert!(auth::parse_callback("/auth/callback?error=access_denied&state=s", "s").is_err());
}
#[test]
fn callback_preserves_issued_client() {
    let parsed = auth::parse_callback("/auth/callback?code=x&state=s&client_id=oaiapp_test", "s")
        .expect("callback");
    assert_eq!(parsed.client_id.as_deref(), Some("oaiapp_test"));
}
#[test]
fn callback_rejects_wrong_path() {
    assert!(auth::parse_callback("/callback?code=x&state=s", "s").is_err());
}
#[test]
fn credential_roundtrip_and_debug_redaction() {
    let (_temp, store) = temp_store();
    store.save(&registry()).expect("save");
    let restored = store.load().expect("load");
    assert_eq!(
        restored.accounts[0]
            .tokens
            .as_ref()
            .expect("tokens")
            .access_token,
        "test-access-secret"
    );
    let debug = format!("{restored:?} {:?}", restored.accounts[0]);
    assert!(!debug.contains("secret"));
}
#[cfg(unix)]
#[test]
fn credentials_are_owner_only_and_reject_symlinks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (temp, store) = temp_store();
    store.save(&registry()).expect("save");
    let root = temp
        .path()
        .canonicalize()
        .expect("root")
        .join("credentials");
    assert_eq!(
        std::fs::metadata(root.join("accounts.json"))
            .expect("meta")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&root).expect("meta").permissions().mode() & 0o777,
        0o700
    );
    let bad = temp.path().canonicalize().expect("root").join("link");
    symlink(&root, &bad).expect("link");
    assert!(CredentialStore::at(bad).load().is_err());
    std::fs::set_permissions(
        root.join("accounts.json"),
        std::fs::Permissions::from_mode(0o644),
    )
    .expect("chmod");
    assert!(store.load().is_err());
}
#[tokio::test]
async fn credential_lock_wait_is_cancellable() {
    let (_temp, store) = temp_store();
    let cancel = CancellationToken::new();
    let _lock = store.lock(&cancel).await.expect("lock");
    let other = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        other.cancel();
    });
    assert!(store.lock(&cancel).await.is_err());
}
#[test]
fn request_has_namespace_and_preview_fields_only() {
    let request = serde_json::to_value(
        responses::build(
            "test-model",
            "test-account",
            &[user("hello")],
            &tools(),
            None,
        )
        .expect("request"),
    )
    .expect("json");
    assert_eq!(request["store"], false);
    assert_eq!(request["stream"], true);
    assert_eq!(request["tools"][0]["type"], "namespace");
    assert_eq!(request["tools"][0]["tools"][0]["type"], "function");
    for field in [
        "temperature",
        "metadata",
        "max_output_tokens",
        "previous_response_id",
    ] {
        assert!(request.get(field).is_none());
    }
}
#[test]
fn system_role_maps_to_developer_without_user_promotion() {
    let mut system = user("guidance");
    system.role = "system".into();
    let request = serde_json::to_value(
        responses::build("m", "a", &[system, user("untrusted")], &[], None).expect("request"),
    )
    .expect("json");
    assert_eq!(request["input"][0]["role"], "developer");
    assert_eq!(request["input"][1]["role"], "user");
}
#[test]
fn completed_preserves_call_id_and_reasoning() {
    let reasoning =
        json!({"type":"reasoning","id":"rs_test","encrypted_content":"opaque-test","summary":[]});
    let (reply, usage) = responses::completed(
        &response(vec![reasoning.clone(), call_output()]),
        "a",
        "m",
        &tools(),
    )
    .expect("reply");
    assert_eq!(reply.tool_calls[0].id.as_deref(), Some("call_test"));
    assert_eq!(reply.provider_state.expect("state").output[0], reasoning);
    assert_eq!(
        usage
            .expect("usage")
            .prompt_tokens_details
            .expect("details")
            .cached_tokens,
        Some(0)
    );
}
#[test]
fn completed_rejects_unknown_namespace_and_tool() {
    let mut call = call_output();
    call["namespace"] = json!("foreign");
    assert!(responses::completed(&response(vec![call]), "a", "m", &tools()).is_err());
    assert!(responses::completed(&response(vec![call_output()]), "a", "m", &[]).is_err());
}
#[test]
fn completed_rejects_duplicate_calls_and_invalid_arguments() {
    assert!(
        responses::completed(
            &response(vec![call_output(), call_output()]),
            "a",
            "m",
            &tools()
        )
        .is_err()
    );
    let mut call = call_output();
    call["arguments"] = json!("{");
    assert!(responses::completed(&response(vec![call]), "a", "m", &tools()).is_err());
}
#[test]
fn completed_rejects_incomplete_status() {
    let mut body = response(vec![text_output()]);
    body["status"] = json!("incomplete");
    assert!(responses::completed(&body, "a", "m", &[]).is_err());
}
#[test]
fn raw_output_roundtrips_without_projection_duplication() {
    let (reply, _) = responses::completed(
        &response(vec![text_output(), call_output()]),
        "a",
        "m",
        &tools(),
    )
    .expect("reply");
    let assistant = ChatMessage {
        role: reply.role,
        content: reply.content,
        tool_calls: reply.tool_calls,
        tool_call_id: None,
        provider_state: reply.provider_state,
    };
    let restored: ChatMessage =
        serde_json::from_value(serde_json::to_value(assistant).expect("json")).expect("restore");
    let result = ChatMessage {
        role: "tool".into(),
        content: Some("{}".into()),
        tool_calls: vec![],
        tool_call_id: Some("call_test".into()),
        provider_state: None,
    };
    let request = serde_json::to_value(
        responses::build("m", "a", &[user("hi"), restored, result], &tools(), None)
            .expect("request"),
    )
    .expect("json");
    assert_eq!(request["input"].as_array().expect("input").len(), 4);
    assert_eq!(request["input"][3]["call_id"], "call_test");
}
#[test]
fn history_rejects_account_switch_and_orphan_results() {
    let (reply, _) =
        responses::completed(&response(vec![text_output()]), "a", "m", &[]).expect("reply");
    let assistant = ChatMessage {
        role: reply.role,
        content: reply.content,
        tool_calls: reply.tool_calls,
        tool_call_id: None,
        provider_state: reply.provider_state,
    };
    assert!(responses::build("m", "b", &[assistant], &[], None).is_err());
    let mut orphan = user("result");
    orphan.role = "tool".into();
    orphan.tool_call_id = Some("missing".into());
    assert!(responses::build("m", "a", &[orphan], &[], None).is_err());
}
#[test]
fn legacy_message_loads_without_provider_state() {
    let message: ChatMessage =
        serde_json::from_value(json!({"role":"assistant","content":"legacy"})).expect("legacy");
    assert!(message.provider_state.is_none());
}
#[test]
fn sse_handles_byte_split_utf8_crlf_and_heartbeat() {
    let mut decoder = sse::Decoder::default();
    let wire = format!(
        ": ping\r\ndata: {}\r\n\r\n",
        json!({"type":"response.output_text.delta","delta":"日本語"})
    );
    let mut events = vec![];
    for byte in wire.bytes() {
        events.extend(decoder.push(&[byte]).expect("frame"));
    }
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["delta"], "日本語");
}
#[test]
fn sse_handles_multiline_data() {
    let mut decoder = sse::Decoder::default();
    let events = decoder
        .push(b"data: {\ndata: \"type\":\"test\"}\n\n")
        .expect("frame");
    assert_eq!(events[0]["type"], "test");
}
#[test]
fn sse_rejects_oversized_frame() {
    assert!(
        sse::Decoder::default()
            .push(&vec![b'x'; 4 * 1024 * 1024 + 1])
            .is_err()
    );
}
#[test]
fn quota_error_is_nonretryable_and_redacted() {
    let error = ProviderError::from_body(
        Some(429),
        &json!({"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"test-access-secret"}}),
        Some("req_test".into()),
    );
    assert!(!error.retryable);
    assert!(!error.to_string().contains("secret"));
}
#[tokio::test]
async fn scope_denial_stops_before_network() {
    let (_temp, store) = temp_store();
    let mut registry = registry();
    registry.accounts[0]
        .tokens
        .as_mut()
        .expect("tokens")
        .scopes
        .clear();
    store.save(&registry).expect("save");
    let handle = auth::AuthHandle::selected(store).expect("selected");
    assert!(
        handle
            .bearer(&CancellationToken::new())
            .await
            .expect_err("denial")
            .to_string()
            .contains("not granted")
    );
}
#[tokio::test]
async fn unexpired_token_is_read_from_store_on_each_request() {
    let (_temp, store) = temp_store();
    store.save(&registry()).expect("save");
    let handle = auth::AuthHandle::selected(store.clone()).expect("auth");
    assert_eq!(
        handle
            .bearer(&CancellationToken::new())
            .await
            .expect("token"),
        "test-access-secret"
    );
    let mut registry = registry();
    registry.accounts[0]
        .tokens
        .as_mut()
        .expect("tokens")
        .access_token = "replacement".into();
    store.save(&registry).expect("save");
    assert_eq!(
        handle
            .bearer(&CancellationToken::new())
            .await
            .expect("token"),
        "replacement"
    );
}

async fn mock_client(
    body: String,
) -> (
    httptest::Server,
    tempfile::TempDir,
    crate::llm::OpenAIClient,
) {
    use httptest::{Expectation, matchers::*, responders::*};
    let server = httptest::ServerBuilder::new()
        .bind_addr(([127, 0, 0, 1], 0).into())
        .run()
        .expect("server");
    server.expect(
        Expectation::matching(all_of![
            request::method_path("POST", "/v1/responses"),
            request::headers(contains(("authorization", "Bearer test-access-secret")))
        ])
        .times(1)
        .respond_with(
            status_code(200)
                .append_header("content-type", "text/event-stream")
                .body(body),
        ),
    );
    let (temp, store) = temp_store();
    store.save(&registry()).expect("save");
    let mut handle = auth::AuthHandle::selected(store).expect("auth");
    handle.resource = server.url_str("/v1");
    let mut client =
        crate::llm::OpenAIClient::new("https://api.openai.com/v1", "").expect("client");
    client.subscription = Some(handle);
    (server, temp, client)
}

#[tokio::test]
async fn subscription_refusal_and_malformed_mutation_share_preflight() -> Result<()> {
    let tool_defs = vec![crate::tools::write::tool_def()];
    for refused in [false, true] {
        let call = json!({"type":"function_call","id":"fc","call_id":"call","namespace":"dgc","name":"fs_write","arguments":"{\"path\":\"/fixture/keep.txt\"}","status":"completed"});
        let mut output = vec![call];
        if refused {
            output.push(json!({"type":"message","id":"refusal","role":"assistant","status":"completed","content":[{"type":"refusal","refusal":"declined"}]}));
        }
        let wire = format!(
            "data: {}\n\n",
            json!({"type":"response.completed","response":response(output)})
        );
        let (_server, _credentials, client) = mock_client(wire).await;
        let error = crate::llm::tool_execution::requests::chat_tools_once(
            &client,
            "test-model",
            &[user("fixture")],
            &tool_defs,
            None,
            crate::config::ReasoningMode::Off,
            Some(CancellationToken::new()),
            None,
        )
        .await
        .expect_err("invalid mutation must not reach dispatch");
        assert_eq!(
            error.downcast_ref::<crate::llm::LlmErrorKind>(),
            Some(&if refused {
                crate::llm::LlmErrorKind::Incomplete
            } else {
                crate::llm::LlmErrorKind::Client
            })
        );
        assert_eq!(
            client.get_total_tokens_used(),
            16,
            "provider-reported generation remains accounted"
        );
    }
    Ok(())
}

#[tokio::test]
async fn subscription_context_overflow_keeps_raw_error_and_common_recovery_type() -> Result<()> {
    let wire = "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"context_length_exceeded\",\"message\":\"fixture overflow\"}}}\n\n".to_string();
    let (_server, _credentials, client) = mock_client(wire).await;
    let error = crate::llm::tool_execution::requests::chat_tools_once(
        &client,
        "test-model",
        &[user("fixture")],
        &[],
        None,
        crate::config::ReasoningMode::Off,
        Some(CancellationToken::new()),
        None,
    )
    .await
    .expect_err("overflow");
    assert_eq!(
        error.downcast_ref::<crate::llm::LlmErrorKind>(),
        Some(&crate::llm::LlmErrorKind::ContextLengthExceeded)
    );
    assert_eq!(
        error
            .downcast_ref::<ProviderError>()
            .expect("raw provider error")
            .code,
        "context_length_exceeded"
    );
    Ok(())
}
#[tokio::test]
async fn tools_path_uses_responses_with_no_api_key() -> Result<()> {
    let wire = format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":response(vec![call_output()])})
    );
    let (_server, _temp, client) = mock_client(wire).await;
    let reply = crate::llm::tool_execution::requests::chat_tools_once(
        &client,
        "test-model",
        &[user("search")],
        &tools(),
        None,
        crate::config::ReasoningMode::Off,
        None,
        None,
    )
    .await?;
    assert_eq!(reply.tool_calls[0].id.as_deref(), Some("call_test"));
    assert_eq!(client.get_total_tokens_used(), 16);
    Ok(())
}
#[tokio::test]
async fn failed_after_delta_never_returns_callable_tools() {
    let wire = format!(
        "data: {}\n\ndata: {}\n\n",
        json!({"type":"response.function_call_arguments.delta","delta":"{}"}),
        json!({"type":"response.failed","response":{"error":{"code":"subscription_sharing_usage_limit_exceeded"}}})
    );
    let (_server, _temp, client) = mock_client(wire).await;
    let error = crate::llm::tool_execution::requests::chat_tools_once(
        &client,
        "test-model",
        &[user("search")],
        &tools(),
        None,
        crate::config::ReasoningMode::Off,
        None,
        None,
    )
    .await
    .expect_err("quota failure");
    assert!(error.downcast_ref::<ProviderError>().is_some());
    assert_eq!(client.get_total_tokens_used(), 0);
}
#[tokio::test]
async fn eof_without_completed_is_failure() {
    let (_server, _temp, client) = mock_client(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n".into(),
    )
    .await;
    assert!(
        client
            .chat_once("test-model", vec![user("hi")], None)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn ordinary_chat_uses_the_same_sse_completion_gate() {
    let wire = format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":response(vec![text_output()])})
    );
    let (_server, _temp, client) = mock_client(wire).await;
    assert_eq!(
        client
            .chat_once("test-model", vec![user("hi")], None)
            .await
            .expect("reply")
            .content,
        "こんにちは"
    );
}
#[tokio::test]
async fn cancelled_request_never_contacts_server() {
    let (_temp, store) = temp_store();
    store.save(&registry()).expect("save");
    let mut client =
        crate::llm::OpenAIClient::new("https://api.openai.com/v1", "").expect("client");
    client.subscription = Some(auth::AuthHandle::selected(store).expect("auth"));
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(
        client
            .chat_once("test-model", vec![user("hi")], Some(cancel))
            .await
            .is_err()
    );
}

fn signed_token(mut claims: Value) -> String {
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    if claims.get("exp").is_none() {
        claims["exp"] = json!(auth::now() + 600);
    }
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-key".into());
    encode(
        &header,
        &claims,
        &EncodingKey::from_rsa_der(include_bytes!("fixtures/test-only-rsa.der")),
    )
    .expect("sign synthetic token")
}
fn claims() -> Value {
    json!({"sub":"synthetic-subject","iss":auth::ISSUER,"aud":"test-client","nonce":"test-nonce"})
}
fn jwks() -> jsonwebtoken::jwk::JwkSet {
    serde_json::from_str(include_str!("fixtures/test-only-jwks.json")).expect("test keys")
}
#[test]
fn oidc_accepts_verified_identity() {
    let claims = auth::validate_id(
        &signed_token(claims()),
        &jwks(),
        "test-client",
        "test-nonce",
    )
    .expect("valid ID token");
    assert_eq!(claims.sub, "synthetic-subject");
}
#[test]
fn oidc_rejects_wrong_issuer_audience_nonce_and_expiry() {
    for (field, value) in [
        ("iss", json!("https://attacker.invalid")),
        ("aud", json!("other-client")),
        ("nonce", json!("wrong")),
        ("exp", json!(auth::now() - 600)),
        ("sub", json!("")),
    ] {
        let mut c = claims();
        c[field] = value;
        assert!(
            auth::validate_id(&signed_token(c), &jwks(), "test-client", "test-nonce").is_err(),
            "{field}"
        );
    }
}
#[test]
fn oidc_rejects_forged_signature_and_unsigned_token() {
    let token = signed_token(claims());
    let mut parts: Vec<_> = token.split('.').map(str::to_owned).collect();
    parts[2].replace_range(..4, "AAAA");
    assert!(auth::validate_id(&parts.join("."), &jwks(), "test-client", "test-nonce").is_err());
    assert!(
        auth::validate_id(
            "eyJhbGciOiJub25lIn0.e30.",
            &jwks(),
            "test-client",
            "test-nonce"
        )
        .is_err()
    );
}

#[tokio::test]
async fn rotating_refresh_is_single_flight_even_for_independent_handles() {
    use httptest::{Expectation, matchers::*, responders::*};
    let server = httptest::ServerBuilder::new()
        .bind_addr(([127, 0, 0, 1], 0).into())
        .run()
        .expect("server");
    server.expect(Expectation::matching(request::method_path("POST","/api/accounts/oauth/token")).times(1).respond_with(json_encoded(json!({"access_token":"new-test-access","refresh_token":"new-test-refresh","token_type":"Bearer","expires_in":3600}))));
    let (_temp, store) = temp_store();
    let mut data = registry();
    data.accounts[0].tokens.as_mut().expect("tokens").expires_at = auth::now() - 1;
    store.save(&data).expect("save");
    let mut first = auth::AuthHandle::selected(store.clone()).expect("auth");
    first.issuer = server.url_str("");
    let mut second = auth::AuthHandle::selected(store.clone()).expect("auth");
    second.issuer = server.url_str("");
    let cancel = CancellationToken::new();
    let (a, b) = tokio::join!(first.bearer(&cancel), second.bearer(&cancel));
    assert_eq!(a.expect("first"), "new-test-access");
    assert_eq!(b.expect("second"), "new-test-access");
    let saved = store.load().expect("load");
    assert_eq!(
        saved.accounts[0]
            .tokens
            .as_ref()
            .expect("tokens")
            .refresh_token
            .as_deref(),
        Some("new-test-refresh")
    );
}
#[tokio::test]
async fn terminal_refresh_failure_is_not_replayed() {
    use httptest::{Expectation, matchers::*, responders::*};
    let server = httptest::ServerBuilder::new()
        .bind_addr(([127, 0, 0, 1], 0).into())
        .run()
        .expect("server");
    server.expect(
        Expectation::matching(request::method_path("POST", "/api/accounts/oauth/token"))
            .times(1)
            .respond_with(status_code(400).body("{\"error\":\"invalid_grant\"}")),
    );
    let (_temp, store) = temp_store();
    let mut data = registry();
    data.accounts[0].tokens.as_mut().expect("tokens").expires_at = auth::now() - 1;
    store.save(&data).expect("save");
    let mut handle = auth::AuthHandle::selected(store).expect("auth");
    handle.issuer = server.url_str("");
    let cancel = CancellationToken::new();
    assert!(handle.bearer(&cancel).await.is_err());
    assert!(
        handle
            .bearer(&cancel)
            .await
            .expect_err("signed out")
            .to_string()
            .contains("signed out")
    );
}
#[test]
fn governor_does_not_tokenize_opaque_ciphertext() {
    let mut assistant = user("projection");
    assistant.role = "assistant".into();
    assistant.provider_state = Some(ProviderState {
        version: 1,
        account: "a".into(),
        model: "m".into(),
        output: vec![
            json!({"type":"reasoning","id":"rs","encrypted_content":"a".repeat(50000),"summary":[]}),
        ],
    });
    let governor = crate::llm::context_budget::ContextBudgetGovernor::new(Default::default());
    let first = governor
        .measure_subscription("a", "m", &[assistant.clone()], &[], 0)
        .expect("measure");
    assistant.provider_state.as_mut().expect("state").output[0]["encrypted_content"] =
        json!("short");
    let second = governor
        .measure_subscription("a", "m", &[assistant], &[], 0)
        .expect("measure");
    assert_eq!(first.total_json_bytes, second.total_json_bytes);
}
#[test]
fn provider_binding_survives_session_save_and_rejects_switch() {
    let temp = tempfile::tempdir().expect("temp");
    let store = crate::session::SessionStore::new(temp.path().join("sessions")).expect("store");
    let mut manager = crate::session::SessionManager::with_store(store);
    manager.create_session(None).expect("session");
    manager
        .bind_inference("openai-chatgpt:account-a:m".into())
        .expect("bind");
    let id = manager.current_session_id().expect("id");
    manager.load_session(&id).expect("resume");
    assert!(
        manager
            .bind_inference("openai-chatgpt:account-b:m".into())
            .is_err()
    );
    manager
        .bind_inference("openai-chatgpt:account-a:m".into())
        .expect("same");
}

async fn mock_login(
    store: CredentialStore,
    selected: Option<&str>,
    subject: &str,
    grant_plan: bool,
    enable_plan: bool,
    declined: bool,
) -> Result<String> {
    use httptest::{Expectation, matchers::*, responders::*};
    let server = httptest::ServerBuilder::new()
        .bind_addr(([127, 0, 0, 1], 0).into())
        .run()
        .expect("server");
    let base = server.url_str("").trim_end_matches('/').to_owned();
    let discovery = json!({"issuer":auth::ISSUER, "jwks_uri":format!("{base}/jwks"), "revocation_endpoint":format!("{base}/revoke")});
    let old = store
        .load()?
        .accounts
        .into_iter()
        .find(|a| Some(a.label.as_str()) == selected);
    let expected_client = old
        .as_ref()
        .map(|a| a.client_id.as_str())
        .unwrap_or("dynamic_agent_client")
        .to_owned();
    let issued = old
        .as_ref()
        .map(|a| a.client_id.as_str())
        .unwrap_or("oaiapp_new")
        .to_owned();
    let service = auth::AuthService {
        store,
        http: auth::http_client()?,
        auth_base: base,
        clock: auth::now,
    };
    let server = &server;
    service.login(selected, enable_plan, |url| {
        let expected_client = expected_client.clone();
        let issued = issued.clone();
        let old = old.clone();
        let discovery = discovery.clone();
        async move {
        let params: std::collections::HashMap<_, _> = url.query_pairs().map(|(k,v)| (k.into_owned(), v.into_owned())).collect();
        assert_eq!(params["client_id"], expected_client);
        assert_eq!(params.get("prompt").map(String::as_str), enable_plan.then_some("consent"));
        assert_eq!(params.contains_key("agent_name_hint"), old.is_none());
        assert!(params["ext_agent_host_id"].starts_with("urn:uuid:"));
        assert_eq!(params["code_challenge_method"], "S256");
        let mut callback = url::Url::parse(&params["redirect_uri"])?;
        callback.query_pairs_mut().append_pair("state", &params["state"]);
        if declined {
            callback.query_pairs_mut().append_pair("error", "access_denied");
        } else {
            callback.query_pairs_mut().append_pair("code", "test-code");
            if old.is_none() { callback.query_pairs_mut().append_pair("client_id", &issued); }
            let id = signed_token(json!({"iss":auth::ISSUER,"aud":issued,"sub":subject,"nonce":params["nonce"]}));
            server.expect(Expectation::matching(all_of![
                request::method_path("POST", "/api/accounts/oauth/token"),
                request::body(url_decoded(contains(("client_id", issued.clone())))),
                request::body(url_decoded(contains(("redirect_uri", params["redirect_uri"].clone())))),
            ]).times(1).respond_with(json_encoded(json!({"access_token":"test-new-access", "refresh_token":"test-new-refresh", "id_token":id,"token_type":"Bearer","expires_in":3600,"scope":if grant_plan {"openid chatgpt.tokens.use.direct"} else {"openid"}}))));
            server.expect(Expectation::matching(request::method_path("GET", "/.well-known/openid-configuration")).times(1).respond_with(json_encoded(discovery)));
            server.expect(Expectation::matching(request::method_path("GET", "/jwks")).times(1).respond_with(json_encoded(jwks())));
        }
        tokio::spawn(async move { reqwest::get(callback).await.expect("callback request"); });
        Ok(())
    }}, &CancellationToken::new()).await
}

#[tokio::test]
async fn login_registration_reauth_and_explicit_consent_preserve_identity() -> Result<()> {
    let (_temp, store) = temp_store();
    let label = mock_login(store.clone(), None, "new-sub", false, false, false).await?;
    let first = store.load()?;
    assert_eq!(first.accounts[0].client_id, "oaiapp_new");
    assert_eq!(first.accounts[0].subject, "new-sub");
    assert_eq!(
        first.accounts[0].tokens.as_ref().expect("tokens").scopes,
        vec!["openid"]
    );
    assert!(
        auth::AuthHandle::selected(store.clone())?
            .bearer(&CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(
        mock_login(store.clone(), Some(&label), "new-sub", true, true, false).await?,
        label
    );
    let second = store.load()?;
    assert_eq!(first.host_id, second.host_id);
    assert_eq!(second.accounts.len(), 1);
    assert_eq!(
        auth::AuthHandle::selected(store)?
            .bearer(&CancellationToken::new())
            .await?,
        "test-new-access"
    );
    Ok(())
}

#[tokio::test]
async fn login_rejects_account_mixup_without_replacing_credentials() -> Result<()> {
    let (_temp, store) = temp_store();
    store.save(&registry())?;
    let before = serde_json::to_value(store.load()?)?;
    assert!(
        mock_login(
            store.clone(),
            Some("test-account"),
            "other-sub",
            true,
            false,
            false
        )
        .await
        .is_err()
    );
    assert_eq!(serde_json::to_value(store.load()?)?, before);
    Ok(())
}

#[tokio::test]
async fn declined_login_does_not_exchange_code_or_modify_active_account() -> Result<()> {
    let (_temp, store) = temp_store();
    store.save(&registry())?;
    let before = serde_json::to_value(store.load()?)?;
    assert!(
        mock_login(store.clone(), None, "unused", true, false, true)
            .await
            .is_err()
    );
    assert_eq!(serde_json::to_value(store.load()?)?, before);
    Ok(())
}

fn fixture_fs(root: &std::path::Path) -> (crate::config::AppConfig, crate::tools::FsTools) {
    let cfg = crate::config::AppConfig {
        provider: ProviderKind::OpenaiChatgpt,
        model: "test-model".into(),
        project_root: root.to_path_buf(),
        tool_routing: crate::config::ToolRoutingConfig {
            mode: crate::config::ToolRoutingMode::Eager,
            search_result_limit: 5,
        },
        ..Default::default()
    };
    let fs = crate::tools::FsTools::new(
        std::sync::Arc::new(tokio::sync::RwLock::new(None)),
        std::sync::Arc::new(cfg.clone()),
    );
    (cfg, fs)
}

#[tokio::test]
async fn completed_multiple_tools_persist_and_resume_with_raw_reasoning() -> Result<()> {
    use httptest::{Expectation, matchers::*, responders::*};
    let temp = tempfile::tempdir()?;
    let read_arguments =
        serde_json::to_string(&json!({"path":temp.path().join("fixture.txt"),"mode":"full"}))?;
    let reasoning = json!({"type":"reasoning", "id":"rs_tools", "summary":[], "encrypted_content":"opaque-test-history"});
    let call = |id: &str| json!({"type":"function_call","id":format!("fc_{id}"),"call_id":id,"namespace":"dgc","name":"fs_read","arguments":read_arguments,"status":"completed"});
    let first = format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":response(vec![reasoning.clone(), call("read_1"), call("read_2")])})
    );
    let (_server, _credentials, client) = mock_client(first).await;
    let final_wire = format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":response(vec![text_output()])})
    );
    _server.expect(
        Expectation::matching(all_of![
            request::method_path("POST", "/v1/responses"),
            request::body(matches("function_call_output")),
            request::body(matches("opaque-test-history"))
        ])
        .times(1)
        .respond_with(status_code(200).body(final_wire)),
    );
    std::fs::write(temp.path().join("fixture.txt"), "fixture content")?;
    let (cfg, fs) = fixture_fs(temp.path());
    let (history, reply) = crate::llm::run_agent_loop(
        &client,
        "test-model",
        &fs,
        vec![user("read fixture twice")],
        None,
        None,
        &cfg,
        None,
        crate::provenance::ProvenanceAttribution::none(),
    )
    .await?;
    assert_eq!(reply.content, "こんにちは");
    assert_eq!(history.iter().filter(|m| m.role == "tool").count(), 2);
    assert!(history.iter().filter(|m| m.role == "tool").all(|m| {
        m.content
            .as_ref()
            .is_some_and(|text| text.contains("fixture content"))
    }));
    assert_eq!(
        history
            .iter()
            .filter_map(|m| m.provider_state.as_ref())
            .next()
            .expect("state")
            .output[0],
        reasoning
    );
    let store = crate::session::SessionStore::new(temp.path().join("sessions"))?;
    let mut manager = crate::session::SessionManager::with_store(store.clone());
    manager.create_session(None)?;
    manager.bind_inference("openai-chatgpt:test-account:test-model".into())?;
    manager.update_current_session_with_history(&history)?;
    let id = manager.current_session_id().expect("session");
    let mut resumed = crate::session::SessionManager::with_store(store);
    resumed.load_session(&id)?;
    let loaded = resumed.current_session.as_ref().expect("resumed");
    let restored: Vec<ChatMessage> =
        serde_json::from_value(serde_json::to_value(&loaded.conversation)?)?;
    let input = serde_json::to_value(responses::build(
        "test-model",
        "test-account",
        &restored,
        &[],
        None,
    )?)?;
    assert_eq!(
        input["input"]
            .as_array()
            .expect("input")
            .iter()
            .filter(|i| i["type"] == "function_call_output")
            .count(),
        2
    );
    assert!(
        resumed
            .bind_inference("openai-chatgpt:other:test-model".into())
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn failed_stream_cannot_mutate_workspace_in_either_agent_path() -> Result<()> {
    for streaming in [false, true] {
        let wire = format!(
            "data: {}\n\ndata: {}\n\n",
            json!({"type":"response.output_item.done", "item":{"type":"function_call","name":"fs_write","namespace":"dgc","call_id":"write","arguments":"{\"path\":\"must-not-exist.txt\",\"content\":\"bad\"}"}}),
            json!({"type":"response.failed","response":{"error":{"code":"subscription_sharing_usage_limit_exceeded"}}})
        );
        let (_server, temp, client) = mock_client(wire).await;
        let (cfg, fs) = fixture_fs(temp.path());
        if streaming {
            assert!(
                crate::llm::tool_execution::run_agent_streaming_once(
                    &client,
                    "test-model",
                    &fs,
                    vec![user("write file")],
                    None,
                    None
                )
                .await
                .is_err()
            );
        } else {
            assert!(
                crate::llm::run_agent_loop(
                    &client,
                    "test-model",
                    &fs,
                    vec![user("write file")],
                    None,
                    None,
                    &cfg,
                    None,
                    crate::provenance::ProvenanceAttribution::none()
                )
                .await
                .is_err()
            );
        }
        assert!(!temp.path().join("must-not-exist.txt").exists());
        assert_eq!(client.get_total_tokens_used(), 0);
    }
    Ok(())
}

#[tokio::test]
async fn logout_retries_revocation_then_clears_tokens_preserving_registration() -> Result<()> {
    use httptest::{Expectation, matchers::*, responders::*};
    let server = httptest::ServerBuilder::new()
        .bind_addr(([127, 0, 0, 1], 0).into())
        .run()?;
    let base = server.url_str("").trim_end_matches('/').to_owned();
    server.expect(Expectation::matching(request::method_path("GET", "/.well-known/openid-configuration")).times(1).respond_with(json_encoded(json!({"issuer":auth::ISSUER,"jwks_uri":format!("{base}/jwks"),"revocation_endpoint":format!("{base}/revoke")}))));
    server.expect(
        Expectation::matching(request::method_path("POST", "/revoke"))
            .times(2)
            .respond_with(cycle![status_code(503), status_code(200)]),
    );
    let (_temp, store) = temp_store();
    store.save(&registry())?;
    let handle = auth::AuthHandle::selected(store.clone())?;
    let service = auth::AuthService {
        store: store.clone(),
        http: auth::http_client()?,
        auth_base: base,
        clock: auth::now,
    };
    assert!(service.logout(None, &CancellationToken::new()).await?);
    let saved = store.load()?;
    assert_eq!(saved.accounts[0].client_id, "test-client");
    assert_eq!(saved.host_id, "urn:uuid:test-host");
    assert!(saved.accounts[0].tokens.is_none());
    assert!(handle.bearer(&CancellationToken::new()).await.is_err());
    Ok(())
}

#[tokio::test]
async fn compaction_keeps_unseen_output_block_exact_and_offloads_only_seen_results() -> Result<()> {
    let wire = format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":response(vec![text_output()])})
    );
    let (_server, temp, client) = mock_client(wire).await;
    let (cfg, fs) = fixture_fs(temp.path());
    let (reply, _) = responses::completed(
        &response(vec![
            json!({"type":"reasoning","id":"rs","encrypted_content":"opaque-protected","summary":[]}),
            call_output(),
        ]),
        "test-account",
        "test-model",
        &tools(),
    )?;
    let protected = ChatMessage {
        role: "assistant".into(),
        content: reply.content,
        tool_calls: reply.tool_calls,
        tool_call_id: None,
        provider_state: reply.provider_state,
    };
    let mut messages = vec![];
    for i in 0..6 {
        messages.push(user(&format!("old exchange {i}")));
    }
    messages.push(protected.clone());
    let mut history =
        crate::llm::tool_execution::history::HistoryManager::new(client, messages, None, fs, cfg);
    history.push_tool_result(Some("call_test".into()), "large tool result ".repeat(5000));
    let before = serde_json::to_value(&history.as_slice()[6..])?;
    assert!(history.compact_for_budget_pressure().await?);
    let start = history
        .protected_suffix_start_for_unseen()
        .expect("protected suffix");
    assert_eq!(serde_json::to_value(&history.as_slice()[start..])?, before);
    assert_eq!(history.unseen_count(), 1);
    history.mark_sent_tool_results_seen();
    history.offload_stale_tool_results_for_pressure(0);
    assert_eq!(
        history.as_slice()[start]
            .provider_state
            .as_ref()
            .expect("state")
            .output,
        protected.provider_state.expect("state").output
    );
    Ok(())
}

#[tokio::test]
async fn model_catalog_uses_oauth_and_preserves_visible_server_order() -> Result<()> {
    use httptest::{Expectation, matchers::*, responders::*};
    let server = httptest::ServerBuilder::new()
        .bind_addr(([127, 0, 0, 1], 0).into())
        .run()?;
    server.expect(Expectation::matching(all_of![request::method_path("GET", "/v1/models"), request::headers(contains(("authorization", "Bearer test-access-secret")))]).times(1).respond_with(json_encoded(json!({"models":[{"slug":"z-first","display_name":"First","visibility":"list"},{"slug":"hidden","visibility":"hidden"},{"slug":"a-last","display_name":"Last","visibility":"list"}]}))));
    let (_temp, store) = temp_store();
    store.save(&registry())?;
    let mut handle = auth::AuthHandle::selected(store)?;
    handle.resource = server.url_str("/v1");
    let models = models::fetch(&handle, &CancellationToken::new()).await?;
    assert_eq!(
        models.iter().map(|m| m.slug.as_str()).collect::<Vec<_>>(),
        ["z-first", "a-last"]
    );
    assert!(models::catalog(json!({"data":[{"id":"api-key-model"}]})).is_err());
    Ok(())
}

#[tokio::test]
async fn subagent_uses_subscription_without_api_key() -> Result<()> {
    let wire = format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":response(vec![text_output()])})
    );
    let (_server, temp, client) = mock_client(wire).await;
    let (_cfg, fs) = fixture_fs(temp.path());
    let runtime =
        crate::llm::tool_runtime::ToolRuntime::build(&fs, Some(client.clone()), "test-model", None)
            .await?;
    let result = crate::llm::tool_execution::run_subagent(
        &client,
        "test-model",
        &runtime,
        "inspect",
        "describe project",
        None,
        temp.path().to_str().expect("project path"),
    )
    .await?;
    assert_eq!(result.summary, "こんにちは");
    assert_eq!(result.tool_calls, 0);
    assert_eq!(client.get_total_tokens_used(), 16);
    Ok(())
}

#[tokio::test]
async fn expired_code_restarts_once_with_issued_client_and_fresh_oauth_state() -> Result<()> {
    use httptest::{Expectation, matchers::*, responders::*};
    let server = httptest::ServerBuilder::new()
        .bind_addr(([127, 0, 0, 1], 0).into())
        .run()?;
    server.expect(
        Expectation::matching(request::method_path("POST", "/api/accounts/oauth/token"))
            .times(2)
            .respond_with(status_code(400).body("{\"error\":\"invalid_grant\"}")),
    );
    let (_temp, store) = temp_store();
    let service = auth::AuthService {
        store: store.clone(),
        http: auth::http_client()?,
        auth_base: server.url_str("").trim_end_matches('/').into(),
        clock: auth::now,
    };
    let mut previous: Option<std::collections::HashMap<String, String>> = None;
    let mut attempts = 0;
    let result = service
        .login(
            None,
            false,
            |url| {
                let params: std::collections::HashMap<String, String> = url
                    .query_pairs()
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect();
                assert_eq!(
                    params["client_id"],
                    if attempts == 0 {
                        "dynamic_agent_client"
                    } else {
                        "oaiapp_pending"
                    }
                );
                if let Some(old) = &previous {
                    for field in ["state", "nonce", "code_challenge"] {
                        assert_ne!(params[field], old[field]);
                    }
                }
                let mut callback = url::Url::parse(&params["redirect_uri"]).expect("redirect");
                callback
                    .query_pairs_mut()
                    .append_pair("state", &params["state"])
                    .append_pair("code", &format!("code-{attempts}"));
                if attempts == 0 {
                    callback
                        .query_pairs_mut()
                        .append_pair("client_id", "oaiapp_pending");
                }
                attempts += 1;
                previous = Some(params);
                async move {
                    tokio::spawn(async move {
                        reqwest::get(callback).await.expect("callback");
                    });
                    Ok(())
                }
            },
            &CancellationToken::new(),
        )
        .await;
    assert!(result.is_err());
    assert_eq!(attempts, 2);
    assert!(store.load()?.accounts.is_empty());
    Ok(())
}

#[tokio::test]
async fn logout_racing_refresh_cannot_resurrect_credentials() -> Result<()> {
    use httptest::{Expectation, matchers::*, responders::*};
    let server = httptest::ServerBuilder::new()
        .bind_addr(([127, 0, 0, 1], 0).into())
        .run()?;
    let base = server.url_str("").trim_end_matches('/').to_owned();
    server.expect(Expectation::matching(request::method_path("POST", "/api/accounts/oauth/token")).times(..=1).respond_with(json_encoded(json!({"access_token":"new-test-access","refresh_token":"new-test-refresh","token_type":"Bearer","expires_in":3600}))));
    server.expect(Expectation::matching(request::method_path("GET", "/.well-known/openid-configuration")).times(1).respond_with(json_encoded(json!({"issuer":auth::ISSUER,"jwks_uri":format!("{base}/jwks"),"revocation_endpoint":format!("{base}/revoke")}))));
    server.expect(
        Expectation::matching(request::method_path("POST", "/revoke"))
            .times(1)
            .respond_with(status_code(200)),
    );
    let (_temp, store) = temp_store();
    let mut data = registry();
    data.accounts[0].tokens.as_mut().expect("tokens").expires_at = auth::now() - 1;
    store.save(&data)?;
    let mut handle = auth::AuthHandle::selected(store.clone())?;
    handle.issuer = base.clone();
    let service = auth::AuthService {
        store: store.clone(),
        http: auth::http_client()?,
        auth_base: base,
        clock: auth::now,
    };
    let cancel = CancellationToken::new();
    let (_renewal, logout) = tokio::join!(handle.bearer(&cancel), service.logout(None, &cancel));
    assert!(logout?);
    assert!(store.load()?.accounts[0].tokens.is_none());
    assert!(handle.bearer(&cancel).await.is_err());
    Ok(())
}

#[tokio::test]
async fn failed_followup_preserves_unseen_tool_results_in_durable_session() -> Result<()> {
    use httptest::{Expectation, matchers::*, responders::*};
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("read.txt"), "retained result")?;
    let call = json!({"type":"function_call","id":"fc_retained","call_id":"retained","namespace":"dgc","name":"fs_read","arguments":serde_json::to_string(&json!({"path":temp.path().join("read.txt"),"mode":"full"}))?,"status":"completed"});
    let first_output = vec![
        json!({"type":"reasoning","id":"rs_retained","encrypted_content":"opaque-retained","summary":[]}),
        call,
    ];
    let wire = format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":response(first_output.clone())})
    );
    let (server, _credentials, client) = mock_client(wire).await;
    server.expect(Expectation::matching(all_of![request::method_path("POST", "/v1/responses"), request::body(matches("function_call_output"))]).times(1).respond_with(status_code(200).body("data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"subscription_sharing_usage_limit_exceeded\"}}}\n\n")));
    let store = crate::session::SessionStore::new(temp.path().join("sessions"))?;
    let mut manager = crate::session::SessionManager::with_store(store.clone());
    manager.create_session(None)?;
    let id = manager.current_session_id().expect("id");
    let manager = std::sync::Arc::new(std::sync::Mutex::new(manager));
    let (cfg, fs) = fixture_fs(temp.path());
    let fs = fs.with_session_manager(manager);
    assert!(
        crate::llm::run_agent_loop(
            &client,
            "test-model",
            &fs,
            vec![user("read file")],
            None,
            None,
            &cfg,
            None,
            crate::provenance::ProvenanceAttribution::none()
        )
        .await
        .is_err()
    );
    let saved = store.load(&id)?;
    let messages: Vec<ChatMessage> =
        serde_json::from_value(serde_json::to_value(&saved.conversation)?)?;
    assert_eq!(
        messages
            .iter()
            .find_map(|m| m.provider_state.as_ref())
            .expect("state")
            .output,
        first_output
    );
    assert!(messages.iter().any(|m| {
        m.role == "tool"
            && m.content
                .as_ref()
                .is_some_and(|s| s.contains("retained result"))
    }));
    assert!(saved.unseen_tool_results.contains("retained"));
    assert!(responses::build("test-model", "test-account", &messages, &[], None).is_ok());
    Ok(())
}

#[tokio::test]
async fn interrupted_batch_checkpoint_retains_output_and_marks_unknown_calls() -> Result<()> {
    let (temp, store) = temp_store();
    store.save(&registry())?;
    let mut client = crate::llm::OpenAIClient::new(auth::RESOURCE, "")?;
    client.subscription = Some(auth::AuthHandle::selected(store)?);
    let session_store = crate::session::SessionStore::new(temp.path().join("sessions"))?;
    let mut manager = crate::session::SessionManager::with_store(session_store.clone());
    manager.create_session(None)?;
    let id = manager.current_session_id().expect("session");
    let (cfg, fs) = fixture_fs(temp.path());
    let fs = fs.with_session_manager(std::sync::Arc::new(std::sync::Mutex::new(manager)));
    let mut second = call_output();
    second["call_id"] = json!("second_call");
    second["id"] = json!("fc_second");
    let (reply, _) = responses::completed(
        &response(vec![
            json!({"type":"reasoning","id":"rs","encrypted_content":"never-debug-this-ciphertext","summary":[]}),
            call_output(),
            second,
        ]),
        "test-account",
        "test-model",
        &tools(),
    )?;
    let assistant = ChatMessage {
        provider_state: reply.provider_state,
        role: "assistant".into(),
        content: reply.content,
        tool_calls: reply.tool_calls,
        tool_call_id: None,
    };
    let mut history = crate::llm::tool_execution::history::HistoryManager::new(
        client,
        vec![user("inspect"), assistant],
        None,
        fs,
        cfg,
    );
    history.push_tool_result(Some("call_test".into()), "completed result".into());
    drop(history);
    let saved = session_store.load(&id)?;
    let messages: Vec<ChatMessage> =
        serde_json::from_value(serde_json::to_value(saved.conversation)?)?;
    assert!(messages.iter().any(|m| {
        m.tool_call_id.as_deref() == Some("second_call")
            && m.content
                .as_ref()
                .is_some_and(|s| s.contains("outcome unknown"))
    }));
    assert!(saved.unseen_tool_results.contains("call_test"));
    assert!(saved.unseen_tool_results.contains("second_call"));
    let request = responses::build("test-model", "test-account", &messages, &[], None)?;
    assert!(!format!("{request:?}").contains("never-debug-this-ciphertext"));
    Ok(())
}

#[test]
fn completed_rejects_unfinished_items_and_non_assistant_messages() {
    for status in ["in_progress", "incomplete", "failed"] {
        let mut call = call_output();
        call["status"] = json!(status);
        assert!(responses::completed(&response(vec![call]), "a", "m", &tools()).is_err());
    }
    let mut message = text_output();
    message["role"] = json!("developer");
    assert!(responses::completed(&response(vec![message.clone()]), "a", "m", &[]).is_err());
    let history = ChatMessage {
        role: "assistant".into(),
        content: None,
        tool_calls: vec![],
        tool_call_id: None,
        provider_state: Some(ProviderState {
            version: 1,
            account: "a".into(),
            model: "m".into(),
            output: vec![message],
        }),
    };
    assert!(responses::build("m", "a", &[history], &[], None).is_err());
}

#[tokio::test]
async fn inference_uses_configured_deadline_instead_of_oauth_timeout() -> Result<()> {
    use httptest::{Expectation, matchers::*, responders::*};
    use std::time::Duration;
    let server = httptest::ServerBuilder::new()
        .bind_addr(([127, 0, 0, 1], 0).into())
        .run()?;
    let wire = format!(
        "data: {}\n\n",
        json!({"type":"response.completed", "response":response(vec![text_output()])})
    );
    server.expect(
        Expectation::matching(request::method_path("POST", "/v1/responses"))
            .times(1)
            .respond_with(delay_and_then(
                Duration::from_millis(150),
                status_code(200).body(wire),
            )),
    );
    let (_temp, store) = temp_store();
    store.save(&registry())?;
    let mut handle = auth::AuthHandle::selected(store)?;
    handle.http = reqwest::Client::builder()
        .timeout(Duration::from_millis(30))
        .build()?;
    handle.resource = server.url_str("/v1");
    let mut client = crate::llm::OpenAIClient::new("https://api.openai.com/v1", "")?;
    client.llm_cfg.request_timeout_ms = 2000;
    client.subscription = Some(handle);
    assert_eq!(
        client
            .chat_once("test-model", vec![user("hi")], None)
            .await?
            .content,
        "こんにちは"
    );
    Ok(())
}

#[test]
fn oidc_accepts_es256_jwk_and_rejects_wrong_nonce() {
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    let mut value = claims();
    value["exp"] = json!(auth::now() + 600);
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some("test-ec-key".into());
    let token = encode(
        &header,
        &value,
        &EncodingKey::from_ec_der(include_bytes!("fixtures/test-only-ec.der")),
    )
    .expect("synthetic ES256 signature");
    let keys = serde_json::from_str(include_str!("fixtures/test-only-ec-jwks.json"))
        .expect("synthetic EC JWK");
    assert_eq!(
        auth::validate_id(&token, &keys, "test-client", "test-nonce")
            .expect("verified ES256 identity")
            .sub,
        "synthetic-subject"
    );
    assert!(auth::validate_id(&token, &keys, "test-client", "wrong-nonce").is_err());
}
