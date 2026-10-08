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
        responses::build("m", "a", &[system, user("untrusted")], &[], None, None).expect("request"),
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
        responses::build(
            "m",
            "a",
            &[user("hi"), restored, result],
            &tools(),
            None,
            None,
        )
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
    assert!(responses::build("m", "b", &[assistant], &[], None, None).is_err());
    let mut orphan = user("result");
    orphan.role = "tool".into();
    orphan.tool_call_id = Some("missing".into());
    assert!(responses::build("m", "a", &[orphan], &[], None, None).is_err());
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
                crate::llm::LlmErrorKind::InvalidToolArguments
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
        additional_tool_names: Vec::new(),
    });
    let governor = crate::llm::context_budget::ContextBudgetGovernor::new(Default::default());
    let first = governor
        .measure_subscription("a", "m", &[assistant.clone()], &[], 0, None)
        .expect("measure");
    assistant.provider_state.as_mut().expect("state").output[0]["encrypted_content"] =
        json!("short");
    let second = governor
        .measure_subscription("a", "m", &[assistant], &[], 0, None)
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
        .bind_inference("openai:account-a:m".into())
        .expect("bind");
    let id = manager.current_session_id().expect("id");
    manager.load_session(&id).expect("resume");
    assert!(manager.bind_inference("openai:account-b:m".into()).is_err());
    manager
        .bind_inference("openai:account-a:m".into())
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
    mock_login_with_browser_failure(
        store,
        selected,
        subject,
        grant_plan,
        enable_plan,
        declined,
        false,
    )
    .await
}

async fn mock_login_with_browser_failure(
    store: CredentialStore,
    selected: Option<&str>,
    subject: &str,
    grant_plan: bool,
    enable_plan: bool,
    declined: bool,
    browser_failure: bool,
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
        let url = if browser_failure {
            let temp = tempfile::tempdir()?;
            let outcome = auth::launch_browser(&temp.path().join("missing-browser"), &url, std::time::Duration::from_secs(1), &CancellationToken::new()).await?;
            assert_eq!(outcome, auth::BrowserLaunch::Unavailable);
            let message = auth::browser_instructions(&url, Some(outcome));
            let manual = message.lines().find(|line| line.starts_with("http://")).expect("manual URL");
            url::Url::parse(manual)?
        } else { url };
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
        provider: ProviderKind::Openai,
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
    let run = crate::llm::run_agent_loop(
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
    let history = run.messages;
    let reply = run.final_message;
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
    manager.bind_inference("openai:test-account:test-model".into())?;
    manager.update_current_session_with_history(&history)?;
    let id = manager.current_session_id().expect("session");
    drop(manager); // Resume after the previous owner exits.
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
            .bind_inference("openai:other:test-model".into())
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
    assert!(responses::build("test-model", "test-account", &messages, &[], None, None).is_ok());
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
    let request = responses::build("test-model", "test-account", &messages, &[], None, None)?;
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
            additional_tool_names: Vec::new(),
        }),
    };
    assert!(responses::build("m", "a", &[history], &[], None, None).is_err());
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

// --- Responses native server-side compaction v1 ---

fn compaction_item(id: &str) -> Value {
    json!({"type":"compaction","id":id,"encrypted_content":"opaque-test"})
}

#[test]
fn native_request_carries_context_management_without_legacy_fields() {
    let request = serde_json::to_value(
        responses::build("m", "a", &[user("hi")], &tools(), None, Some(102_400)).expect("request"),
    )
    .expect("json");
    assert_eq!(request["store"], false);
    assert_eq!(request["stream"], true);
    assert_eq!(request["context_management"][0]["type"], "compaction");
    assert_eq!(
        request["context_management"][0]["compact_threshold"],
        102_400
    );
    for field in [
        "previous_response_id",
        "conversation",
        "background",
        "truncation",
    ] {
        assert!(
            request.get(field).is_none(),
            "forbidden field {field} must not be sent"
        );
    }
}

#[test]
fn native_request_without_threshold_omits_context_management() {
    let request = serde_json::to_value(
        responses::build("m", "a", &[user("hi")], &tools(), None, None).expect("request"),
    )
    .expect("json");
    assert!(request.get("context_management").is_none());
    assert_eq!(request["store"], false);
    assert_eq!(request["stream"], true);
}

#[test]
fn native_threshold_minimum_is_enforced_without_clamp() {
    assert!(
        responses::validate_compact_threshold(999).is_err(),
        "999 must be rejected, never clamped"
    );
    assert!(responses::validate_compact_threshold(1000).is_ok());
    assert!(responses::validate_compact_threshold(102_400).is_ok());
    assert!(responses::build("m", "a", &[user("hi")], &[], None, Some(999)).is_err());
    assert!(responses::build("m", "a", &[user("hi")], &[], None, Some(1000)).is_ok());
    // Client fixture override also rejects below-minimum values.
    assert!(
        crate::llm::OpenAIClient::new("https://example.invalid", "k")
            .expect("client")
            .with_responses_compact_threshold(Some(999))
            .is_err()
    );
    let enabled = crate::llm::OpenAIClient::new("https://example.invalid", "k")
        .expect("client")
        .with_responses_compact_threshold(Some(1000))
        .expect("threshold");
    assert!(enabled.native_responses_compaction_enabled());
    assert_eq!(enabled.responses_compact_threshold(), Some(1000));
    let plain = crate::llm::OpenAIClient::new("https://example.invalid", "k").expect("client");
    assert!(!plain.native_responses_compaction_enabled());
    assert_eq!(plain.responses_compact_threshold(), None);
}

#[test]
fn native_compaction_item_is_accepted_and_preserved() {
    let (reply, _) = responses::completed(
        &response(vec![compaction_item("cmp_test"), text_output()]),
        "a",
        "m",
        &[],
    )
    .expect("reply");
    let state = reply.provider_state.expect("state");
    assert_eq!(state.version, 1, "serialization version must not change");
    assert!(state.contains_compaction());
    assert_eq!(state.latest_compaction_index(), Some(0));
    assert_eq!(state.output[0]["encrypted_content"], "opaque-test");
    // Visible text extraction is unaffected by the opaque item.
    assert_eq!(reply.content.as_deref(), Some("こんにちは"));
}

#[test]
fn native_compaction_item_validation_is_strict_but_opaque() {
    // Empty ciphertext is rejected.
    assert!(
        responses::completed(
            &response(vec![
                json!({"type":"compaction","id":"x","encrypted_content":""})
            ]),
            "a",
            "m",
            &[],
        )
        .is_err()
    );
    // Missing ciphertext is rejected.
    assert!(
        responses::completed(
            &response(vec![json!({"type":"compaction","id":"x"})]),
            "a",
            "m",
            &[],
        )
        .is_err()
    );
    // Non-string id is rejected without inspecting ciphertext.
    assert!(
        responses::completed(
            &response(vec![
                json!({"type":"compaction","id":1,"encrypted_content":"opaque"})
            ]),
            "a",
            "m",
            &[],
        )
        .is_err()
    );
}

#[test]
fn native_canonical_output_uses_latest_boundary_only() {
    let output = vec![
        json!({"type":"reasoning","id":"rs-old","encrypted_content":"old","summary":[]}),
        json!({"type":"message","id":"msg-old","role":"assistant","status":"completed","content":[{"type":"output_text","text":"old"}]}),
        json!({"type":"compaction","id":"cmp-1","encrypted_content":"opaque-1"}),
        json!({"type":"reasoning","id":"rs-mid","encrypted_content":"mid","summary":[]}),
        json!({"type":"compaction","id":"cmp-2","encrypted_content":"opaque-2"}),
        json!({"type":"message","id":"msg-new","role":"assistant","status":"completed","content":[{"type":"output_text","text":"new"}]}),
    ];
    assert_eq!(
        responses::latest_compaction_index(&output),
        Some(4),
        "latest compaction item wins, never the first"
    );
    let canonical = responses::canonical_output(&output);
    assert_eq!(canonical.len(), 2);
    assert_eq!(canonical[0]["id"], "cmp-2");
    assert_eq!(canonical[1]["id"], "msg-new");
    let (reply, _) = responses::completed(&response(output), "a", "m", &[]).expect("reply");
    let state = reply.provider_state.expect("state");
    assert_eq!(state.output.len(), 2);
    assert_eq!(state.output[0]["id"], "cmp-2");
    assert_eq!(state.output[1]["id"], "msg-new");
    // Pre-boundary text must not leak into visible content.
    assert_eq!(reply.content.as_deref(), Some("new"));
}

#[test]
fn native_compaction_with_tool_call_preserves_pairing() {
    let call = call_output();
    let (reply, _) = responses::completed(
        &response(vec![
            compaction_item("cmp_tools"),
            json!({"type":"reasoning","id":"rs","encrypted_content":"opaque","summary":[]}),
            call.clone(),
        ]),
        "a",
        "m",
        &tools(),
    )
    .expect("reply");
    assert_eq!(reply.tool_calls.len(), 1);
    assert_eq!(reply.tool_calls[0].id.as_deref(), Some("call_test"));
    let state = reply.provider_state.expect("state");
    assert_eq!(state.output.len(), 3);
    assert_eq!(state.output[0]["type"], "compaction");
    assert_eq!(state.output[2]["call_id"], "call_test");
    assert!(state.contains_compaction());
}

#[test]
fn native_pre_boundary_tool_calls_never_dispatch() {
    // A stale function call before the boundary must not dispatch; only the
    // post-boundary call is returned.
    let mut stale = call_output();
    stale["call_id"] = json!("stale_call");
    stale["id"] = json!("fc_stale");
    let output = vec![stale, compaction_item("cmp-boundary"), call_output()];
    let (reply, _) = responses::completed(&response(output), "a", "m", &tools()).expect("reply");
    assert_eq!(reply.tool_calls.len(), 1);
    assert_eq!(reply.tool_calls[0].id.as_deref(), Some("call_test"));
    let state = reply.provider_state.expect("state");
    assert!(
        state
            .output
            .iter()
            .all(|item| item["call_id"] != "stale_call"),
        "pre-boundary calls must not persist"
    );
}

#[test]
fn native_round_trip_request_replays_compaction_byte_identical() {
    let call = call_output();
    let (reply, _) = responses::completed(
        &response(vec![compaction_item("cmp_rt"), call.clone()]),
        "test-account",
        "test-model",
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
    // Byte-equivalent session persistence.
    let restored: ChatMessage =
        serde_json::from_value(serde_json::to_value(&assistant).expect("json")).expect("restore");
    assert_eq!(
        serde_json::to_value(&restored).expect("json"),
        serde_json::to_value(&assistant).expect("json")
    );
    let state = restored.provider_state.as_ref().expect("state");
    assert_eq!(state.account, "test-account");
    assert_eq!(state.model, "test-model");
    assert_eq!(state.output[0]["encrypted_content"], "opaque-test");
    let result = ChatMessage {
        role: "tool".into(),
        content: Some("{}".into()),
        tool_calls: vec![],
        tool_call_id: Some("call_test".into()),
        provider_state: None,
    };
    let next = ChatMessage {
        role: "user".into(),
        content: Some("next instruction".into()),
        tool_calls: vec![],
        tool_call_id: None,
        provider_state: None,
    };
    let request = serde_json::to_value(
        responses::build(
            "test-model",
            "test-account",
            &[restored, result, next],
            &tools(),
            None,
            Some(102_400),
        )
        .expect("request"),
    )
    .expect("json");
    let input = request["input"].as_array().expect("input");
    // Compaction item raw, function call raw, paired output, new user message.
    assert_eq!(input[0]["type"], "compaction");
    assert_eq!(input[0]["encrypted_content"], "opaque-test");
    assert_eq!(input[1]["type"], "function_call");
    assert_eq!(input[1]["call_id"], "call_test");
    assert_eq!(input[2]["type"], "function_call_output");
    assert_eq!(input[2]["call_id"], "call_test");
    assert_eq!(input[3]["role"], "user");
    assert_eq!(input[3]["content"], "next instruction");
    assert_eq!(
        request["context_management"][0]["compact_threshold"],
        102_400
    );
}

#[test]
fn native_compacted_session_resume_replays_only_compact_state() {
    let temp = tempfile::tempdir().expect("temp");
    let store = crate::session::SessionStore::new(temp.path().join("sessions")).expect("store");
    let mut manager = crate::session::SessionManager::with_store(store.clone());
    manager.create_session(None).expect("session");
    manager
        .bind_inference("openai:test-account:test-model".into())
        .expect("bind");
    // Canonical pruned history: only the compaction assistant + follow-ups.
    let (reply, _) = responses::completed(
        &response(vec![compaction_item("cmp-resume"), text_output()]),
        "test-account",
        "test-model",
        &[],
    )
    .expect("reply");
    let compacted = ChatMessage {
        role: reply.role,
        content: reply.content,
        tool_calls: reply.tool_calls,
        tool_call_id: None,
        provider_state: reply.provider_state,
    };
    let followup = user("continue from compact state");
    manager
        .update_current_session_with_history(&[compacted.clone(), followup.clone()])
        .expect("save");
    let id = manager.current_session_id().expect("id");
    drop(manager);
    let mut resumed = crate::session::SessionManager::with_store(store);
    resumed.load_session(&id).expect("resume");
    let loaded = resumed.current_session.as_ref().expect("resumed");
    let messages: Vec<ChatMessage> =
        serde_json::from_value(serde_json::to_value(&loaded.conversation).expect("json"))
            .expect("messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0].provider_state.as_ref().expect("state").output[0]["encrypted_content"],
        "opaque-test"
    );
    let request = serde_json::to_value(
        responses::build(
            "test-model",
            "test-account",
            &messages,
            &[],
            None,
            Some(102_400),
        )
        .expect("request"),
    )
    .expect("json");
    let input = request["input"].as_array().expect("input");
    // No pre-compaction transcript: compaction + new user prompt only.
    assert_eq!(input.len(), 3);
    assert_eq!(input[0]["type"], "compaction");
    assert_eq!(input[1]["type"], "message");
    assert_eq!(input[2]["role"], "user");
}

async fn mock_native_client(
    body: String,
    threshold: u32,
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
    // Assert the actual wire body carries server-side compaction alongside
    // the preserved store/stream contract.
    server.expect(
        Expectation::matching(all_of![
            request::method_path("POST", "/v1/responses"),
            request::headers(contains(("authorization", "Bearer test-access-secret"))),
            request::body(matches("context_management")),
            request::body(matches("\"type\":\"compaction\"")),
            request::body(matches(format!("\"compact_threshold\":{threshold}"))),
            request::body(matches("\"store\":false")),
            request::body(matches("\"stream\":true")),
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
    let client = client
        .with_responses_compact_threshold(Some(threshold))
        .expect("native threshold");
    (server, temp, client)
}

#[tokio::test]
async fn responses_usage_diagnostics_hide_provider_values_in_errors_and_logs() {
    use tracing::instrument::WithSubscriber;
    const SECRET: &str = "SENTINEL_RESPONSES_USAGE_SECRET_20261005";
    let mut payload = response(vec![text_output()]);
    payload["usage"]["input_tokens"] = json!(SECRET);
    let mut leaks = Vec::new();
    let mut diagnostics = Vec::new();
    for native in [false, true] {
        let mut payload = payload.clone();
        if native {
            payload["output"]
                .as_array_mut()
                .unwrap()
                .insert(0, compaction_item("cmp-fixture"));
        }
        let wire = format!(
            "data: {}\n\n",
            json!({"type":"response.completed","response":payload})
        );
        let (_server, _temp, client) = if native {
            mock_native_client(wire, 102_400).await
        } else {
            mock_client(wire).await
        };
        let capture = crate::test_support::DiagnosticCapture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(move || writer.clone())
            .finish();
        let error = crate::llm::tool_execution::requests::chat_tools_once(
            &client,
            "test-model",
            &[user("fixture")],
            &[],
            None,
            crate::config::ReasoningMode::Off,
            None,
            None,
        )
        .with_subscriber(subscriber)
        .await
        .unwrap_err();
        let rendered = format!("{error:#}\n{error:?}");
        let logs = capture.text();
        if rendered.contains(SECRET) {
            leaks.push(format!("native={native} error"));
        }
        if logs.contains(SECRET) {
            leaks.push(format!("native={native} log"));
        }
        assert_eq!(client.usage_snapshot().usage_records, 0);
        assert_eq!(client.usage_snapshot().total_tokens, 0);
        diagnostics.push((rendered, logs));
    }
    let legacy_error = responses::completed(&payload, "account", "test-model", &[]).unwrap_err();
    let legacy_error = format!("{legacy_error:#}\n{legacy_error:?}");
    if legacy_error.contains(SECRET) {
        leaks.push("legacy completed error".into());
    }
    assert!(
        leaks.is_empty(),
        "provider usage sentinel leaked through: {leaks:?}"
    );
    assert!(
        legacy_error.contains("data")
            && legacy_error.contains("line")
            && legacy_error.contains("column")
    );
    for (error, logs) in diagnostics {
        for diagnostic in [error, logs] {
            assert!(
                diagnostic.contains("data")
                    && diagnostic.contains("line")
                    && diagnostic.contains("column")
            );
        }
    }
}

#[tokio::test]
async fn native_inference_sends_context_management_on_wire() -> Result<()> {
    let wire = format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":response(vec![text_output()])})
    );
    let (_server, _temp, client) = mock_native_client(wire, 102_400).await;
    let reply = crate::llm::tool_execution::requests::chat_tools_once(
        &client,
        "test-model",
        &[user("hi")],
        &[],
        None,
        crate::config::ReasoningMode::Off,
        None,
        None,
    )
    .await?;
    assert_eq!(reply.content.as_deref(), Some("こんにちは"));
    assert_eq!(client.get_total_tokens_used(), 16);
    // The httptest expectation above already asserted the wire body carried
    // `context_management` with the resolved threshold; a mismatch would
    // have failed the request.
    Ok(())
}

#[tokio::test]
async fn attempt_budget_responses_scoped_hook_gates_send_and_preserves_usage() {
    for native in [false, true] {
        let wire = format!(
            "data: {}\n\n",
            json!({"type":"response.completed","response":response(vec![text_output()])})
        );
        let (_server, _temp, parent) = if native {
            mock_native_client(wire, 102_400).await
        } else {
            mock_client(wire).await
        };
        let policy = std::sync::Arc::new(crate::test_support::SingleAttemptPolicy::default());
        let client = parent.with_request_attempt_policy(policy.clone());
        for denied in [false, true] {
            let result = crate::llm::tool_execution::requests::chat_tools_once(
                &client,
                "test-model",
                &[user("fixture")],
                &[],
                None,
                crate::config::ReasoningMode::Off,
                None,
                None,
            )
            .await;
            if denied {
                assert!(
                    result
                        .unwrap_err()
                        .downcast_ref::<crate::test_support::FixtureAttemptDenied>()
                        .is_some()
                );
            } else {
                result.unwrap();
            }
        }
        assert_eq!(parent.usage_snapshot().attempts, 1);
        assert_eq!(parent.usage_snapshot().usage_records, 1);
        assert_eq!(parent.usage_snapshot().total_tokens, 16);
        assert_eq!(
            policy.reported.load(std::sync::atomic::Ordering::SeqCst),
            16
        );
    }
}

#[tokio::test]
async fn usage_calibration_native_metadata_and_compaction_boundary_are_response_local() -> Result<()>
{
    use crate::llm::context_budget::{ContextBudgetGovernor, RequestFootprint};
    for (compacted, prompt) in [
        (false, Some(12)),
        (false, None),
        (false, Some(0)),
        (true, Some(12)),
        (true, None),
    ] {
        let mut output = vec![text_output()];
        if compacted {
            output.insert(0, compaction_item("usage-boundary"));
        }
        let mut body = response(output);
        match prompt {
            None => {
                body.as_object_mut().expect("body").remove("usage");
            }
            Some(0) => {
                body["usage"] = json!({"input_tokens":0,"output_tokens":0,"total_tokens":0});
            }
            _ => {}
        }
        let wire = format!(
            "data: {}\n\n",
            json!({"type":"response.completed","response":body})
        );
        let (_server, _temp, client) = mock_native_client(wire, 102_400).await;
        client.set_prompt_tokens(999_999);
        let result =
            crate::llm::tool_execution::requests::chat_tools_once_with_activation_and_usage(
                &client,
                "test-model",
                &[user("hi")],
                &[],
                &[],
                None,
                crate::config::ReasoningMode::Off,
                None,
                None,
            )
            .await?;
        assert_eq!(result.prompt_tokens, prompt);
        let fp = RequestFootprint::new(10_000, 0, 0);
        let mut gov = ContextBudgetGovernor::new(Default::default());
        gov.observe_actual(fp, 7);
        client.set_prompt_tokens(888_888);
        let native = result
            .message
            .provider_state
            .as_ref()
            .is_some_and(|s| s.contains_compaction());
        assert_eq!(native, compacted);
        gov.observe_response_usage(Some(fp), result.prompt_tokens, native);
        if compacted {
            assert!(gov.previous_usage().is_none());
        } else {
            assert_eq!(
                gov.previous_usage().expect("sample").actual_prompt_tokens,
                if prompt == Some(12) { 12 } else { 7 }
            );
        }
        let usage = client.usage_snapshot();
        assert_eq!(usage.attempts, 1);
        assert_eq!(usage.usage_records, u64::from(prompt.is_some()));
        assert_eq!(usage.total_tokens, if prompt == Some(12) { 16 } else { 0 });
    }
    Ok(())
}

#[tokio::test]
async fn native_single_response_is_single_budget_charge() -> Result<()> {
    // Main-agent budget: one provider response with compaction is exactly
    // one usage record, never a second internal charge.
    let wire = format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":response(vec![compaction_item("cmp-budget"), text_output()])})
    );
    let (_server, _temp, client) = mock_native_client(wire, 102_400).await;
    let before = client.usage_snapshot();
    let reply = crate::llm::tool_execution::requests::chat_tools_once(
        &client,
        "test-model",
        &[user("hi")],
        &[],
        None,
        crate::config::ReasoningMode::Off,
        None,
        None,
    )
    .await?;
    assert!(
        reply
            .provider_state
            .as_ref()
            .expect("state")
            .contains_compaction()
    );
    let after = client.usage_snapshot();
    assert_eq!(after.attempts, before.attempts + 1);
    assert_eq!(after.usage_records, before.usage_records + 1);
    assert_eq!(after.total_tokens, before.total_tokens + 16);
    assert_eq!(client.get_total_tokens_used(), 16);
    Ok(())
}

#[tokio::test]
async fn native_compaction_with_tools_keeps_batch_pairing() -> Result<()> {
    let wire = format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":response(vec![compaction_item("cmp-batch"), call_output()])})
    );
    let (_server, _temp, client) = mock_native_client(wire, 102_400).await;
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
    assert_eq!(reply.tool_calls.len(), 1);
    assert_eq!(reply.tool_calls[0].id.as_deref(), Some("call_test"));
    assert!(
        reply
            .provider_state
            .as_ref()
            .expect("state")
            .contains_compaction()
    );
    // Pair the synthetic result and verify the next projection is valid.
    let assistant = ChatMessage {
        role: reply.role,
        content: reply.content,
        tool_calls: reply.tool_calls,
        tool_call_id: None,
        provider_state: reply.provider_state,
    };
    let result = ChatMessage {
        role: "tool".into(),
        content: Some("{}".into()),
        tool_calls: vec![],
        tool_call_id: Some("call_test".into()),
        provider_state: None,
    };
    let request = serde_json::to_value(
        responses::build(
            "test-model",
            "test-account",
            &[user("hi"), assistant, result],
            &tools(),
            None,
            Some(102_400),
        )
        .expect("request"),
    )
    .expect("json");
    let input = request["input"].as_array().expect("input");
    assert_eq!(input[1]["type"], "compaction");
    assert_eq!(input[2]["call_id"], "call_test");
    assert_eq!(input[3]["call_id"], "call_test");
    Ok(())
}

#[tokio::test]
async fn native_cancellation_stays_cancelled() {
    let (_temp, store) = temp_store();
    store.save(&registry()).expect("save");
    let mut client =
        crate::llm::OpenAIClient::new("https://api.openai.com/v1", "").expect("client");
    client.subscription = Some(auth::AuthHandle::selected(store).expect("auth"));
    let client = client
        .with_responses_compact_threshold(Some(102_400))
        .expect("threshold");
    let cancel = CancellationToken::new();
    cancel.cancel();
    let err = client
        .chat_once("test-model", vec![user("hi")], Some(cancel))
        .await
        .expect_err("cancelled");
    assert_eq!(
        err.downcast_ref::<crate::llm::LlmErrorKind>(),
        Some(&crate::llm::LlmErrorKind::Cancelled)
    );
}

#[tokio::test]
async fn native_provider_errors_keep_retry_semantics() {
    // Quota, auth, and stream-incomplete errors are unchanged by native mode.
    let quota = format!(
        "data: {}\n\n",
        json!({"type":"response.failed","response":{"error":{"code":"subscription_sharing_usage_limit_exceeded"}}})
    );
    let (_server, _temp, client) = mock_native_client(quota, 102_400).await;
    let err = crate::llm::tool_execution::requests::chat_tools_once(
        &client,
        "test-model",
        &[user("hi")],
        &[],
        None,
        crate::config::ReasoningMode::Off,
        None,
        None,
    )
    .await
    .expect_err("quota");
    assert!(err.downcast_ref::<ProviderError>().is_some());
    assert_eq!(client.get_total_tokens_used(), 0);
}

#[tokio::test]
async fn native_overflow_does_not_fallback_to_local_compactor() -> Result<()> {
    use httptest::{Expectation, matchers::*, responders::*};
    let temp = tempfile::tempdir()?;
    let (cred_temp, store) = temp_store();
    store.save(&registry())?;
    let mut handle = auth::AuthHandle::selected(store)?;
    // Overflow on first attempt; no second attempt may occur (no retry).
    let server = httptest::ServerBuilder::new()
        .bind_addr(([127, 0, 0, 1], 0).into())
        .run()
        .expect("server");
    server.expect(
        Expectation::matching(request::method_path("POST", "/v1/responses"))
            .times(1)
            .respond_with(
                status_code(200)
                    .append_header("content-type", "text/event-stream")
                    .body("data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"context_length_exceeded\"}}}\n\n"),
            ),
    );
    handle.resource = server.url_str("/v1");
    let mut client =
        crate::llm::OpenAIClient::new("https://api.openai.com/v1", "").expect("client");
    client.subscription = Some(handle);
    let client = client
        .with_responses_compact_threshold(Some(102_400))
        .expect("threshold");
    let (cfg, fs) = fixture_fs(temp.path());
    let err = crate::llm::run_agent_loop(
        &client,
        "test-model",
        &fs,
        vec![user("overflow fixture")],
        None,
        None,
        &cfg,
        None,
        crate::provenance::ProvenanceAttribution::none(),
    )
    .await
    .expect_err("overflow must fail");
    let text = format!("{err:?}");
    assert!(
        text.contains("Responses server-side compaction was enabled"),
        "overflow must carry the native guidance, got: {text}"
    );
    assert_eq!(
        err.downcast_ref::<crate::llm::LlmErrorKind>(),
        Some(&crate::llm::LlmErrorKind::ContextLengthExceeded),
        "typed overflow must survive double context, got: {text}"
    );
    drop((cred_temp, server));
    Ok(())
}

// --- Responses append-only tool activation (v1) ---

fn fixture_tool_def(name: &str) -> ToolDef {
    ToolDef {
        kind: "function".into(),
        function: crate::llm::types::ToolFunctionDef {
            name: name.into(),
            description: format!("{name} helper"),
            parameters: serde_json::json!({"type":"object","properties":{}}),
            strict: Some(false),
        },
    }
}

fn append_only_base() -> Vec<ToolDef> {
    vec![
        fixture_tool_def("fs_read"),
        crate::tools::tool_search::tool_def(),
    ]
}

fn append_only_active_with(names: &[&str]) -> Vec<ToolDef> {
    let mut defs = append_only_base();
    for name in names {
        defs.push(fixture_tool_def(name));
    }
    defs.sort_by(|a, b| a.function.name.cmp(&b.function.name));
    defs
}

fn tool_search_call(id: &str) -> ChatMessage {
    ChatMessage {
        provider_state: None,
        role: "assistant".into(),
        content: None,
        tool_calls: vec![crate::llm::types::ToolCall {
            id: Some(id.into()),
            r#type: "function".into(),
            function: crate::llm::types::ToolCallFunction {
                name: "tool_search".into(),
                arguments: "{\"query\":\"edit\"}".into(),
            },
        }],
        tool_call_id: None,
    }
}

fn tool_result(id: &str, content: &str) -> ChatMessage {
    ChatMessage {
        provider_state: None,
        role: "tool".into(),
        content: Some(content.into()),
        tool_calls: vec![],
        tool_call_id: Some(id.into()),
    }
}

fn activation_marker(names: Vec<&str>) -> ChatMessage {
    let sorted: Vec<String> = {
        let mut v: Vec<String> = names.into_iter().map(str::to_string).collect();
        v.sort();
        v
    };
    ChatMessage {
        provider_state: Some(ProviderState::activation("a".into(), "m".into(), sorted)),
        role: "developer".into(),
        content: None,
        tool_calls: vec![],
        tool_call_id: None,
    }
}

#[test]
fn append_only_first_request_has_stable_namespace_only() {
    let base = append_only_base();
    let active = append_only_base();
    let req = responses::build_with_activation("m", "a", &[user("hi")], &base, &active, None, None)
        .expect("build");
    let v = serde_json::to_value(&req).expect("json");
    assert_eq!(v["store"], false);
    assert_eq!(v["stream"], true);
    assert!(v.get("previous_response_id").is_none());
    assert!(v.get("prompt_cache_key").is_none());
    assert!(v.get("prompt_cache_options").is_none());
    // Top-level is the initial namespace only.
    assert_eq!(v["tools"].as_array().expect("tools").len(), 1);
    assert_eq!(v["tools"][0]["type"], "namespace");
    assert_eq!(v["tools"][0]["name"], "dgc");
    // No additional_tools input on first request.
    let input = v["input"].as_array().expect("input");
    assert!(input.iter().all(|i| i["type"] != "additional_tools"));
    // No Responses-native tool_search.
    let raw = serde_json::to_string(&v).expect("raw");
    assert!(
        !raw.contains("\"type\":\"tool_search\""),
        "native tool_search must never emit"
    );
    // Local function tool_search remains as type function.
    assert!(raw.contains("tool_search"));
}

#[test]
fn append_only_post_activation_keeps_top_level_stable() {
    let base = append_only_base();
    let active = append_only_active_with(&["edit"]);
    let history = vec![
        user("do edit"),
        tool_search_call("call_search"),
        tool_result("call_search", "{\"ok\":true}"),
        activation_marker(vec!["edit"]),
    ];
    let first = serde_json::to_value(
        responses::build_with_activation("m", "a", &[user("hi")], &base, &base, None, None)
            .expect("first"),
    )
    .expect("json");
    let second = serde_json::to_value(
        responses::build_with_activation("m", "a", &history, &base, &active, None, None)
            .expect("second"),
    )
    .expect("json");
    assert_eq!(
        first["tools"], second["tools"],
        "top-level tools must be byte-equivalent"
    );
    let input = second["input"].as_array().expect("input").clone();
    let pos_call = input
        .iter()
        .position(|i| i.get("call_id") == Some(&json!("call_search")))
        .expect("call");
    let pos_marker = input
        .iter()
        .position(|i| i["type"] == "additional_tools")
        .expect("marker");
    assert!(
        pos_marker > pos_call,
        "additional_tools must follow its tool_search result"
    );
    let marker = &input[pos_marker];
    assert_eq!(marker["role"], "developer");
    assert_eq!(marker["tools"].as_array().expect("tools").len(), 1);
    assert_eq!(marker["tools"][0]["name"], "edit");
    assert_eq!(marker["tools"][0]["type"], "function");
}

#[test]
fn append_only_exact_ordering_call_output_additional() {
    let base = append_only_base();
    let active = append_only_active_with(&["edit"]);
    let history = vec![
        tool_search_call("c1"),
        tool_result("c1", "{}"),
        activation_marker(vec!["edit"]),
    ];
    let req = serde_json::to_value(
        responses::build_with_activation("m", "a", &history, &base, &active, None, None)
            .expect("req"),
    )
    .expect("json");
    let input = req["input"].as_array().expect("input");
    assert_eq!(input.len(), 3);
    assert_eq!(input[0]["type"], "function_call");
    assert_eq!(input[1]["type"], "function_call_output");
    assert_eq!(input[2]["type"], "additional_tools");
}

#[test]
fn append_only_third_request_does_not_duplicate() {
    let base = append_only_base();
    let active = append_only_active_with(&["edit"]);
    let history = vec![
        user("hi"),
        tool_search_call("c1"),
        tool_result("c1", "{}"),
        activation_marker(vec!["edit"]),
        user("follow up"),
    ];
    let req = serde_json::to_value(
        responses::build_with_activation("m", "a", &history, &base, &active, None, None)
            .expect("req"),
    )
    .expect("json");
    let count = req["input"]
        .as_array()
        .expect("input")
        .iter()
        .filter(|i| i["type"] == "additional_tools")
        .count();
    assert_eq!(count, 1, "same tool must not be re-appended");
}

#[test]
fn append_only_multiple_rounds_preserve_time_order() {
    let base = append_only_base();
    let active = append_only_active_with(&["apply_patch", "edit"]);
    let history = vec![
        tool_search_call("c1"),
        tool_result("c1", "{}"),
        activation_marker(vec!["edit"]),
        tool_search_call("c2"),
        tool_result("c2", "{}"),
        activation_marker(vec!["apply_patch"]),
    ];
    let req = serde_json::to_value(
        responses::build_with_activation("m", "a", &history, &base, &active, None, None)
            .expect("req"),
    )
    .expect("json");
    let markers: Vec<_> = req["input"]
        .as_array()
        .expect("input")
        .iter()
        .filter(|i| i["type"] == "additional_tools")
        .collect();
    assert_eq!(markers.len(), 2);
    assert_eq!(markers[0]["tools"][0]["name"], "edit");
    assert_eq!(markers[1]["tools"][0]["name"], "apply_patch");
}

#[test]
fn append_only_duplicate_marker_rejected() {
    let base = append_only_base();
    let active = append_only_active_with(&["edit"]);
    let history = vec![
        activation_marker(vec!["edit"]),
        activation_marker(vec!["edit"]),
    ];
    assert!(
        responses::build_with_activation("m", "a", &history, &base, &active, None, None).is_err()
    );
}

#[test]
fn append_only_unknown_tool_rejected() {
    let base = append_only_base();
    let active = append_only_base();
    let history = vec![activation_marker(vec!["unknown-tool"])];
    assert!(
        responses::build_with_activation("m", "a", &history, &base, &active, None, None).is_err()
    );
}

#[test]
fn append_only_inactive_tampering_rejected() {
    // `edit` is known to the catalog shape here but not in the passed active
    // set: session-file tampering must fail closed.
    let base = append_only_base();
    let active = append_only_base();
    let history = vec![activation_marker(vec!["edit"])];
    assert!(
        responses::build_with_activation("m", "a", &history, &base, &active, None, None).is_err()
    );
}

#[test]
fn append_only_legacy_repair_inserts_before_first_use() {
    // Old session: edit called without any provider marker.
    let base = append_only_base();
    let active = append_only_active_with(&["edit"]);
    let history = vec![
        tool_search_call("c1"),
        tool_result("c1", "{}"),
        ChatMessage {
            provider_state: None,
            role: "assistant".into(),
            content: None,
            tool_calls: vec![crate::llm::types::ToolCall {
                id: Some("c2".into()),
                r#type: "function".into(),
                function: crate::llm::types::ToolCallFunction {
                    name: "edit".into(),
                    arguments: "{}".into(),
                },
            }],
            tool_call_id: None,
        },
    ];
    let req = serde_json::to_value(
        responses::build_with_activation("m", "a", &history, &base, &active, None, None)
            .expect("repair"),
    )
    .expect("json");
    let input = req["input"].as_array().expect("input");
    let marker_pos = input
        .iter()
        .position(|i| i["type"] == "additional_tools")
        .expect("repair marker");
    let edit_pos = input
        .iter()
        .position(|i| i.get("name") == Some(&json!("edit")))
        .expect("edit call");
    assert!(marker_pos < edit_pos);
}

#[test]
fn append_only_completed_validates_namespaces() {
    let base = append_only_base();
    let active = append_only_active_with(&["edit"]);
    // Base tool with dgc namespace: accept.
    let ok_base = response(vec![
        json!({"type":"function_call","id":"fc1","call_id":"c1","namespace":"dgc","name":"fs_read","arguments":"{}","status":"completed"}),
    ]);
    assert!(responses::completed_with_activation(&ok_base, "a", "m", &base, &active).is_ok());
    // Appended tool unnamespaced: accept.
    let ok_add = response(vec![
        json!({"type":"function_call","id":"fc2","call_id":"c2","name":"edit","arguments":"{}","status":"completed"}),
    ]);
    assert!(responses::completed_with_activation(&ok_add, "a", "m", &base, &active).is_ok());
    // Unknown unnamespaced: reject.
    let bad_unknown = response(vec![
        json!({"type":"function_call","id":"fc3","call_id":"c3","name":"nope","arguments":"{}","status":"completed"}),
    ]);
    assert!(responses::completed_with_activation(&bad_unknown, "a", "m", &base, &active).is_err());
    // Foreign namespace: reject.
    let bad_ns = response(vec![
        json!({"type":"function_call","id":"fc4","call_id":"c4","namespace":"foreign","name":"edit","arguments":"{}","status":"completed"}),
    ]);
    assert!(responses::completed_with_activation(&bad_ns, "a", "m", &base, &active).is_err());
    // Base tool unnamespaced would be wrong (must be dgc): our strict path
    // treats unnamespaced fs_read as unknown additional -> reject.
    let bad_base_unnamespaced = response(vec![
        json!({"type":"function_call","id":"fc5","call_id":"c5","name":"fs_read","arguments":"{}","status":"completed"}),
    ]);
    assert!(
        responses::completed_with_activation(&bad_base_unnamespaced, "a", "m", &base, &active)
            .is_err()
    );
}

#[test]
fn append_only_provider_state_role_validation() {
    let assistant =
        ProviderState::assistant("a".into(), "m".into(), vec![json!({"type":"message"})]);
    assert!(assistant.validate_role_binding("assistant").is_ok());
    assert!(assistant.validate_role_binding("developer").is_err());
    let marker = ProviderState::activation("a".into(), "m".into(), vec!["edit".into()]);
    assert!(marker.validate_role_binding("developer").is_ok());
    assert!(marker.validate_role_binding("assistant").is_err());
    assert!(marker.validate_role_binding("tool").is_err());
    // Mixed state rejected.
    let mut mixed =
        ProviderState::assistant("a".into(), "m".into(), vec![json!({"type":"message"})]);
    mixed.additional_tool_names = vec!["edit".into()];
    assert!(mixed.validate_role_binding("assistant").is_err());
    // Debug is content-free.
    let rendered = format!("{marker:?}");
    assert!(rendered.contains("additional_tool_count"));
    assert!(!rendered.contains("edit"));
}

#[test]
fn append_only_native_tool_search_never_emitted() {
    let base = append_only_base();
    let active = append_only_active_with(&["edit"]);
    let history = vec![activation_marker(vec!["edit"])];
    let req = serde_json::to_value(
        responses::build_with_activation("m", "a", &history, &base, &active, None, None)
            .expect("req"),
    )
    .expect("json");
    let raw = serde_json::to_string(&req).expect("raw");
    assert!(!raw.contains("\"type\":\"tool_search\""));
}

#[test]
fn append_only_resume_activation_only_session() {
    // Session: tool_search activated apply_patch, but apply_patch not yet called.
    // Resume must: catalog active, input has additional, top-level stable.
    use crate::config::{ToolRoutingConfig, ToolRoutingMode};
    use crate::llm::{ToolCatalog, ToolCatalogEntry, ToolSource};
    let defs = vec![fixture_tool_def("fs_read"), fixture_tool_def("apply_patch")];
    let entries: Vec<ToolCatalogEntry> = defs
        .into_iter()
        .map(|def| {
            let text = crate::llm::build_searchable_text(&def, &ToolSource::Builtin);
            ToolCatalogEntry {
                definition: def,
                source: ToolSource::Builtin,
                searchable_text: text,
            }
        })
        .collect();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    rt.block_on(async {
        let catalog = ToolCatalog::from_entries(
            entries,
            &ToolRoutingConfig {
                mode: ToolRoutingMode::Deferred,
                search_result_limit: 5,
            },
        );
        // Simulate persisted sidecar with apply_patch (activation-only).
        let persisted: std::collections::BTreeSet<String> =
            ["apply_patch".into()].into_iter().collect();
        catalog.activate(&persisted.iter().cloned().collect::<Vec<_>>()).await;
        assert!(catalog.is_active("apply_patch").await);
        let base = catalog.initial_active_tool_defs();
        let active = catalog.active_tool_defs().await;
        // Top-level stable: base does not contain apply_patch.
        assert!(!base.iter().any(|d| d.function.name == "apply_patch"));
        assert!(active.iter().any(|d| d.function.name == "apply_patch"));
        // Build resume request with marker.
        let history = vec![
            user("do apply_patch"),
            tool_search_call("c1"),
            tool_result("c1", "{}"),
            activation_marker(vec!["apply_patch"]),
        ];
        let req = serde_json::to_value(
            responses::build_with_activation("m", "a", &history, &base, &active, None, None)
                .expect("resume build"),
        )
        .expect("json");
        assert!(req["input"]
            .as_array()
            .expect("input")
            .iter()
            .any(|i| i["type"] == "additional_tools"));
        // Edit callable as unnamespaced additional.
        let call = response(vec![json!({"type":"function_call","id":"fc","call_id":"c2","name":"apply_patch","arguments":"{}","status":"completed"})]);
        assert!(responses::completed_with_activation(&call, "a", "m", &base, &active).is_ok());
    });
}

#[test]
fn append_only_post_compaction_rebase_shape() {
    // Compacted history lost old marker, but sidecar retains edit.
    // Rebase must re-append without changing top-level.
    let base = append_only_base();
    let active = append_only_active_with(&["edit"]);
    // Post-compaction history: only authority + new user (marker pruned).
    let compacted_history = vec![user("follow up after compaction")];
    let first = serde_json::to_value(
        responses::build_with_activation("m", "a", &compacted_history, &base, &base, None, None)
            .expect("pre-rebase"),
    )
    .expect("json");
    // Rebase marker (simulating agent_loop rebase from sidecar).
    let rebased_history = vec![
        user("follow up after compaction"),
        activation_marker(vec!["edit"]),
    ];
    let second = serde_json::to_value(
        responses::build_with_activation("m", "a", &rebased_history, &base, &active, None, None)
            .expect("post-rebase"),
    )
    .expect("json");
    assert_eq!(
        first["tools"], second["tools"],
        "top-level must not change across rebase"
    );
    assert!(
        second["input"]
            .as_array()
            .expect("input")
            .iter()
            .any(|i| i["type"] == "additional_tools")
    );
}

async fn retry_after_fixture(
    hint: Option<&str>,
    attempts: usize,
    recover: bool,
) -> (
    httptest::Server,
    tempfile::TempDir,
    crate::llm::OpenAIClient,
) {
    use httptest::{Expectation, matchers::*, responders::*};
    let server = httptest::ServerBuilder::new()
        .bind_addr(([127, 0, 0, 1], 0).into())
        .run()
        .expect("fixture");
    let mut failed = status_code(503);
    if let Some(hint) = hint {
        failed = failed.append_header("retry-after", hint);
    }
    let failed = failed.body(r#"{"error":{"code":"server_error"}}"#);
    let wire = format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":response(vec![text_output()])})
    );
    let expectation =
        Expectation::matching(request::method_path("POST", "/v1/responses")).times(attempts);
    if recover {
        server.expect(expectation.respond_with(cycle![failed,
            status_code(200).append_header("content-type", "text/event-stream").body(wire)]));
    } else {
        server.expect(expectation.respond_with(failed));
    }
    let (temp, store) = temp_store();
    store
        .save(&registry())
        .expect("save synthetic fixture credentials");
    let mut handle = auth::AuthHandle::selected(store).expect("fixture auth");
    handle.resource = server.url_str("/v1");
    let mut client = crate::llm::OpenAIClient::new("https://api.openai.com/v1", "").unwrap();
    client.subscription = Some(handle);
    client.llm_cfg.retry_base_ms = 0;
    client.llm_cfg.retry_jitter_ms = 0;
    client.llm_cfg.request_timeout_ms = 500;
    (server, temp, client)
}

#[tokio::test]
async fn responses_retry_after_excessive_hint_declines_original_error_without_resend() {
    for hint in ["301", "600", "18446744073709551615"] {
        let (_server, _temp, client) = retry_after_fixture(Some(hint), 1, false).await;
        let error = client
            .chat_once("test-model", vec![user("hi")], None)
            .await
            .unwrap_err();
        let provider = error
            .downcast_ref::<ProviderError>()
            .expect("original provider error");
        assert_eq!(provider.status, Some(503));
        assert_eq!(provider.code, "server_error");
        assert_eq!(client.usage_snapshot().attempts, 1);
        assert_eq!(client.usage_snapshot().usage_records, 0);
    }
}

#[tokio::test]
async fn responses_retry_after_disabled_uses_local_backoff() {
    let (_server, _temp, mut client) = retry_after_fixture(Some("600"), 2, true).await;
    client.llm_cfg.respect_retry_after = false;
    assert_eq!(
        client
            .chat_once("test-model", vec![user("hi")], None)
            .await
            .unwrap()
            .content
            .as_str(),
        "こんにちは"
    );
    assert_eq!(client.usage_snapshot().attempts, 2);
    assert_eq!(client.usage_snapshot().usage_records, 1);
}

#[tokio::test]
async fn responses_retry_after_zero_and_invalid_hints_use_bounded_retry() {
    for hint in [
        Some("0"),
        None,
        Some("invalid"),
        Some("-1"),
        Some("Wed, 21 Oct 2015 07:28:00 GMT"),
    ] {
        let (_server, _temp, client) = retry_after_fixture(hint, 2, true).await;
        client
            .chat_once("test-model", vec![user("hi")], None)
            .await
            .unwrap();
        assert_eq!(client.usage_snapshot().attempts, 2);
        assert_eq!(client.usage_snapshot().usage_records, 1);
    }
}

#[tokio::test]
async fn responses_retry_after_retry_count_boundaries_preserve_original_error() {
    for max_retries in [0, 1, 3, usize::MAX] {
        let attempts = max_retries.min(3) + 1;
        let (_server, _temp, mut client) = retry_after_fixture(Some("0"), attempts, false).await;
        client.llm_cfg.max_retries = max_retries;
        let error = client
            .chat_once("test-model", vec![user("hi")], None)
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<ProviderError>().is_some());
        assert_eq!(client.usage_snapshot().attempts, attempts as u64);
        assert_eq!(client.usage_snapshot().usage_records, 0);
    }
}

#[tokio::test]
async fn responses_retry_after_long_hint_remains_under_total_deadline() {
    for hint in ["60", "300"] {
        let (_server, _temp, mut client) = retry_after_fixture(Some(hint), 1, false).await;
        client.llm_cfg.request_timeout_ms = 100;
        let error = client
            .chat_once("test-model", vec![user("hi")], None)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("Responses request deadline exceeded"));
        assert_eq!(client.usage_snapshot().attempts, 1);
    }
}

#[tokio::test]
async fn responses_retry_after_wait_is_cancellable_without_resend() {
    use tracing::instrument::WithSubscriber;
    for hint in ["60", "300"] {
        let (_server, _temp, mut client) = retry_after_fixture(Some(hint), 1, false).await;
        client.llm_cfg.request_timeout_ms = 5000;
        let capture = crate::test_support::DiagnosticCapture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(move || writer.clone())
            .finish();
        let token = CancellationToken::new();
        let canceller = token.clone();
        let task = async {
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while !capture.text().contains("Responses retry scheduled") {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("reach actual retry wait");
            // The scheduled delay is the server's exact value, not the old30s cap.
            assert!(capture.text().contains(&format!(
                "retry_delay_ms={}",
                hint.parse::<u64>().unwrap() * 1000
            )));
            canceller.cancel();
        };
        let (result, ()) = tokio::join!(
            client
                .chat_once("test-model", vec![user("hi")], Some(token))
                .with_subscriber(subscriber),
            task,
        );
        let error = result.unwrap_err();
        assert_eq!(
            error.downcast_ref::<crate::llm::LlmErrorKind>(),
            Some(&crate::llm::LlmErrorKind::Cancelled)
        );
        assert_eq!(client.usage_snapshot().attempts, 1);
        assert_eq!(client.usage_snapshot().usage_records, 0);
    }
}

#[tokio::test]
async fn responses_retry_after_delay_policy_does_not_multiply_server_hints() {
    use tracing::instrument::WithSubscriber;
    // Two retries exercise the second-attempt path with a real integer hint.
    let (_server, _temp, mut client) = retry_after_fixture(Some("1"), 3, false).await;
    client.llm_cfg.max_retries = 2;
    client.llm_cfg.request_timeout_ms = 10000;
    let capture = crate::test_support::DiagnosticCapture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(move || writer.clone())
        .finish();
    let error = client
        .chat_once("test-model", vec![user("hi")], None)
        .with_subscriber(subscriber)
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<ProviderError>().is_some());
    assert_eq!(capture.text().matches("retry_delay_ms=1000").count(), 2);
    assert_eq!(client.usage_snapshot().attempts, 3);
}

#[tokio::test]
async fn responses_retry_after_local_backoff_uses_one_based_failures() {
    use tracing::instrument::WithSubscriber;
    let (_server, _temp, mut client) = retry_after_fixture(None, 4, false).await;
    client.llm_cfg.retry_base_ms = 10;
    client.llm_cfg.request_timeout_ms = 5000;
    let capture = crate::test_support::DiagnosticCapture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(move || writer.clone())
        .finish();
    let error = client
        .chat_once("test-model", vec![user("hi")], None)
        .with_subscriber(subscriber)
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<ProviderError>().is_some());
    let text = capture.text();
    for (attempt, millis) in [(1, 10), (2, 20), (3, 40)] {
        assert!(
            text.contains(&format!("attempt={attempt} retry_delay_ms={millis}")),
            "{text}"
        );
    }
    assert_eq!(client.usage_snapshot().attempts, 4);
}

#[cfg(unix)]
#[tokio::test]
async fn browser_handoff_keeps_launched_child_alive() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().expect("tempdir");
    let launcher = temp.path().join("fake-browser-launcher");
    let marker = temp.path().join("browser-still-alive");
    std::fs::write(
        &launcher,
        format!(
            "#!/bin/sh\n(sleep 0.2; touch '{}') </dev/null >/dev/null 2>&1 &\nexit 0\n",
            marker.display()
        ),
    )
    .expect("script");
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700))
        .expect("permissions");
    let result = auth::launch_browser(
        &launcher,
        &url::Url::parse("https://example.invalid/auth").expect("url"),
        std::time::Duration::from_secs(2),
        &CancellationToken::new(),
    )
    .await
    .expect("launch");
    assert_eq!(result, auth::BrowserLaunch::Opened);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !marker.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("browser child must outlive its launcher");
}

#[cfg(unix)]
fn fake_browser(temp: &tempfile::TempDir, script: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = temp.path().join("fake-launcher");
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).expect("permissions");
    path
}

#[cfg(unix)]
#[tokio::test]
async fn browser_launch_failure_and_timeout_allow_manual_sign_in() {
    let temp = tempfile::tempdir().expect("tempdir");
    let url = url::Url::parse("https://example.invalid/auth?state=ephemeral").expect("url");
    let cancel = CancellationToken::new();
    let missing = temp.path().join("missing");
    assert_eq!(
        auth::launch_browser(&missing, &url, std::time::Duration::from_secs(1), &cancel)
            .await
            .expect("missing"),
        auth::BrowserLaunch::Unavailable
    );
    let failed = fake_browser(&temp, "echo sensitive-url >&2; exit 3");
    assert_eq!(
        auth::launch_browser(&failed, &url, std::time::Duration::from_secs(1), &cancel)
            .await
            .expect("failed"),
        auth::BrowserLaunch::Failed
    );
    let slow = fake_browser(&temp, "exec sleep 10");
    assert_eq!(
        auth::launch_browser(&slow, &url, std::time::Duration::from_millis(30), &cancel)
            .await
            .expect("timeout"),
        auth::BrowserLaunch::TimedOut
    );
}

#[cfg(unix)]
#[tokio::test]
async fn browser_launch_cancellation_stays_cancelled() {
    let temp = tempfile::tempdir().expect("tempdir");
    let launched = temp.path().join("launched");
    let program = fake_browser(
        &temp,
        &format!("echo $$ > '{}'; exec sleep 10", launched.display()),
    );
    let url = url::Url::parse("https://example.invalid/auth").expect("url");
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = auth::launch_browser(&program, &url, std::time::Duration::from_secs(2), &cancel)
        .await
        .expect_err("cancelled before launch");
    assert!(matches!(
        error.downcast_ref::<crate::llm::LlmErrorKind>(),
        Some(crate::llm::LlmErrorKind::Cancelled)
    ));
    assert!(!launched.exists());
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let marker = launched.clone();
    let task = tokio::spawn(async move {
        let pid: i32 = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let Some(pid) = std::fs::read_to_string(&marker)
                    .ok()
                    .and_then(|text| text.trim().parse().ok())
                {
                    break pid;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("launcher PID");
        // SAFETY: read process-group IDs only; no signal is sent to the test runner.
        assert_eq!(unsafe { libc::getpgid(pid) }, pid);
        assert_ne!(unsafe { libc::getpgrp() }, pid);
        trigger.cancel();
    });
    let error = auth::launch_browser(&program, &url, std::time::Duration::from_secs(2), &cancel)
        .await
        .expect_err("cancelled during launch");
    task.await.expect("canceller");
    assert!(matches!(
        error.downcast_ref::<crate::llm::LlmErrorKind>(),
        Some(crate::llm::LlmErrorKind::Cancelled)
    ));
}

#[test]
fn browser_instructions_preserve_manual_recovery_without_saved_tokens() {
    let url = url::Url::parse("https://example.invalid/auth?state=test-state&code_challenge=test-challenge&id_token_hint=saved-id-secret&redirect_uri=http%3A%2F%2F127.0.0.1%3A12345%2Fauth%2Fcallback").expect("url");
    for outcome in [
        None,
        Some(auth::BrowserLaunch::Opened),
        Some(auth::BrowserLaunch::Unavailable),
        Some(auth::BrowserLaunch::Failed),
        Some(auth::BrowserLaunch::TimedOut),
    ] {
        let message = auth::browser_instructions(&url, outcome);
        assert!(message.contains("state=test-state"));
        assert!(message.contains("code_challenge=test-challenge"));
        assert!(!message.contains("saved-id-secret"));
        assert!(!message.contains("id_token_hint"));
        assert!(message.contains("same machine"));
        assert!(message.contains("5 minutes"));
        assert!(message.contains("Ctrl-C"));
    }
    assert!(
        url.as_str().contains("saved-id-secret"),
        "do not mutate the browser URL"
    );
}

#[tokio::test]
async fn login_callback_timeout_and_cancellation_are_actionable() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let error = auth::wait_for_callback(
        listener,
        "state",
        std::time::Duration::from_millis(20),
        &CancellationToken::new(),
    )
    .await
    .err()
    .expect("timeout");
    assert!(error.to_string().contains("--no-browser"));
    assert!(error.to_string().contains("same machine"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = auth::wait_for_callback(
        listener,
        "state",
        std::time::Duration::from_secs(1),
        &cancel,
    )
    .await
    .err()
    .expect("cancel");
    assert!(matches!(
        error.downcast_ref::<crate::llm::LlmErrorKind>(),
        Some(crate::llm::LlmErrorKind::Cancelled)
    ));
}

#[tokio::test]
async fn failed_browser_launch_can_finish_mock_login_using_manual_url() -> Result<()> {
    let (_temp, store) = temp_store();
    let label = mock_login_with_browser_failure(
        store.clone(),
        None,
        "manual-sub",
        true,
        false,
        false,
        true,
    )
    .await?;
    let saved = store.load()?;
    assert_eq!(saved.active.as_deref(), Some(label.as_str()));
    assert_eq!(saved.accounts[0].subject, "manual-sub");
    Ok(())
}
