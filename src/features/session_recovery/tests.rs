use super::*;
use crate::llm::types::{ChatMessage, ToolCall, ToolCallFunction};

fn message(role: &str, content: &str) -> ChatMessage {
    ChatMessage {
        role: role.into(),
        content: Some(content.into()),
        reasoning: Default::default(),
        provider_state: None,
        tool_calls: vec![],
        tool_call_id: None,
    }
}
fn fixture() -> (tempfile::TempDir, SessionStore, SessionData) {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path().join(".doge/sessions")).unwrap();
    let mut data = SessionData::new();
    data.inference_binding = Some("PRIVATE-ACCOUNT-BINDING".into());
    data.replace_conversation_messages(&[message("user", "PRIVATE-CONVERSATION")])
        .unwrap();
    store.save(&data).unwrap();
    (root, store, data)
}
#[test]
fn saved_diagnosis_is_read_only_and_does_not_disclose_payloads() {
    let (root, store, data) = fixture();
    let checkpoint = store.session_dir(&data.meta.id).join("session.json");
    let before = std::fs::read(&checkpoint).unwrap();
    let report = inspect(root.path(), &data.meta.id).unwrap();
    assert_eq!(report.route, RecoveryRoute::InspectThenResume);
    assert_eq!(report.view, "saved_checkpoint");
    assert_eq!(report.pending_tail_calls, Some(0));
    let output = format!(
        "{}{}",
        render(&report),
        serde_json::to_string(&report).unwrap()
    );
    assert!(
        !output.contains("PRIVATE-ACCOUNT-BINDING") && !output.contains("PRIVATE-CONVERSATION")
    );
    assert_eq!(std::fs::read(checkpoint).unwrap(), before);
    assert!(!store.session_dir(&data.meta.id).join("provenance").exists());
    assert!(!root.path().join(".doge/logs").exists());
}
#[test]
fn gap_or_unreadable_history_recommends_preservation_and_fresh_checks() {
    let (root, store, mut data) = fixture();
    data.mark_provenance_failure();
    store.save(&data).unwrap();
    let dir = store.session_dir(&data.meta.id);
    let before = std::fs::read(dir.join("session.json")).unwrap();
    let report = inspect(root.path(), &data.meta.id).unwrap();
    assert_eq!(report.route, RecoveryRoute::FreshSessionRequired);
    assert_eq!(report.recorded_write_failures, 1);
    let text = render(&report);
    for required in [
        "outside the managed session store",
        "retention",
        "/verify refresh <explicit-paths>",
        "Do not copy old history",
        "cannot repair the gap",
    ] {
        assert!(text.contains(required));
    }
    assert_eq!(std::fs::read(dir.join("session.json")).unwrap(), before);
    data.provenance_incomplete = false;
    store.save(&data).unwrap();
    let events = dir.join("provenance/v8/events");
    std::fs::create_dir_all(&events).unwrap();
    std::fs::write(events.join("bad.json"), "PRIVATE-MALFORMED-PROVENANCE").unwrap();
    let report = inspect(root.path(), &data.meta.id).unwrap();
    assert_eq!(report.provenance_status, "history_unavailable");
    assert_eq!(report.route, RecoveryRoute::FreshSessionRequired);
    assert!(!render(&report).contains("PRIVATE-MALFORMED"));
}
#[test]
fn pending_tail_is_not_repaired_and_ambiguous_history_is_not_reordered() {
    let (_root, store, mut data) = fixture();
    let mut call = message("assistant", "");
    call.tool_calls = vec![ToolCall {
        id: Some("pending".into()),
        r#type: "function".into(),
        function: ToolCallFunction {
            name: "edit".into(),
            arguments: "PRIVATE-ARGUMENTS".into(),
        },
    }];
    data.replace_conversation_messages(&[message("user", "task"), call.clone()])
        .unwrap();
    let before = serde_json::to_value(&data.conversation).unwrap();
    let report = diagnose(
        &data,
        &store.session_dir(&data.meta.id),
        true,
        "live_checkpoint",
    );
    assert_eq!(report.conversation_status, "pending_tail");
    assert_eq!(report.pending_tail_calls, Some(1));
    assert_eq!(report.route, RecoveryRoute::FreshSessionRequired);
    // Normal CLI/loop preflight rejects an unresolved tool invocation;
    // diagnosis must not promise that adding an instruction can resume it.
    assert!(
        crate::llm::validate_tool_blocks(&data.conversation_messages().unwrap(), false).is_err()
    );
    assert!(render(&report).contains("Inspect actual files"));
    assert!(render(&report).contains("export is not resumable"));
    assert_eq!(serde_json::to_value(&data.conversation).unwrap(), before);
    data.replace_conversation_messages(&[call, message("user", "interleaved")])
        .unwrap();
    assert_eq!(
        diagnose(
            &data,
            &store.session_dir(&data.meta.id),
            false,
            "live_checkpoint"
        )
        .route,
        RecoveryRoute::FreshSessionRequired
    );
}
#[test]
fn missing_store_is_not_created_and_bad_checkpoint_does_not_leak_payload() {
    let root = tempfile::tempdir().unwrap();
    assert!(inspect(root.path(), "missing").is_err());
    assert!(!root.path().join(".doge").exists());
    let (root, store, data) = fixture();
    std::fs::write(
        store.session_dir(&data.meta.id).join("session.json"),
        "PRIVATE-BROKEN-CHECKPOINT",
    )
    .unwrap();
    let error = inspect(root.path(), &data.meta.id).unwrap_err();
    assert!(!format!("{error:#}").contains("PRIVATE-BROKEN"));
    assert_eq!(
        std::fs::read_to_string(store.session_dir(&data.meta.id).join("session.json")).unwrap(),
        "PRIVATE-BROKEN-CHECKPOINT"
    );
}
#[test]
fn explicit_session_diagnosis_does_not_parse_unrelated_bad_checkpoint() {
    let (root, store, data) = fixture();
    let bad = store.session_dir("unrelated");
    std::fs::create_dir(&bad).unwrap();
    std::fs::write(bad.join("session.json"), "not JSON").unwrap();
    assert!(inspect(root.path(), &data.meta.id).is_ok());
}
