use crate::llm::types::ChatMessage;

/// Durable conversation buffer: a canonical ordered message container.
///
/// This type owns no token budgeting, no system-prompt injection, and no
/// lossy trimming. Context reduction (token estimates, stale-result
/// offloading, compaction) belongs to `HistoryManager` / the Context Budget
/// Governor / the Observation Store (`src/llm/context_budget.rs`,
/// `src/llm/tool_execution/history.rs`, `src/llm/observation.rs`,
/// `src/llm/compact_history.rs`).
///
/// Invariants:
/// - `push`/`append`/`replace` never delete, reorder, or summarize messages.
/// - Tool-call invocations and their `tool` results are kept exactly as
///   pushed; this container never splits the protocol pairing.
/// - Messages carrying `provider_state` (ChatGPT subscription / Responses
///   opaque items, encrypted reasoning) are preserved byte-identically.
#[derive(Debug, Clone, Default)]
pub struct ChatHistory {
    messages: Vec<ChatMessage>,
}

impl ChatHistory {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    pub fn from_messages(messages: Vec<ChatMessage>) -> Self {
        Self { messages }
    }

    /// Borrow-free snapshot of the canonical ordered conversation.
    pub fn snapshot(&self) -> Vec<ChatMessage> {
        self.messages.clone()
    }

    /// Replace the entire conversation with the canonical result.
    /// Never deletes selectively: the caller passes the full new truth
    /// (e.g. the history returned by `run_agent_loop`, which may be shorter
    /// after compaction).
    pub fn replace(&mut self, messages: Vec<ChatMessage>) {
        self.messages = messages;
    }

    pub fn push_user(&mut self, content: impl Into<String>) {
        self.push_message(ChatMessage {
            reasoning: Default::default(),
            provider_state: None,
            role: "user".into(),
            content: Some(content.into()),
            tool_calls: vec![],
            tool_call_id: None,
        });
    }

    /// Append one message exactly as given. No trimming, no reordering,
    /// no protocol-pair adjustment.
    pub fn push_message(&mut self, msg: ChatMessage) {
        self.messages.push(msg);
    }

    pub fn append_user(&mut self, content: impl Into<String>) {
        self.push_user(content);
    }

    pub fn append_assistant(&mut self, content: impl Into<String>) {
        self.push_message(ChatMessage {
            reasoning: Default::default(),
            provider_state: None,
            role: "assistant".into(),
            content: Some(content.into()),
            tool_calls: vec![],
            tool_call_id: None,
        });
    }

    /// Appends a generic message (e.g. tool output) exactly as given.
    pub fn append_message(&mut self, msg: ChatMessage) {
        self.push_message(msg);
    }

    pub fn build_messages(&self) -> Vec<ChatMessage> {
        self.snapshot()
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }

    /// Overwrites the current history with the provided messages, exactly.
    /// This is useful for transferring context between executors.
    pub fn overwrite_messages(&mut self, messages: Vec<ChatMessage>) {
        self.replace(messages);
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

/// Project the request-independent durable conversation from a canonical
/// history: everything except request-scoped `role == "system"` messages
/// (default system prompt, shell-output context, plan injection,
/// `RuntimeContext` overlay, loop interventions).
///
/// This is the single choke point for durable persistence. Callers (TUI
/// success path, `exec` success path, subscription checkpoint, session
/// persistence) must not implement their own system-message filtering.
/// Ordering, `provider_state`, `tool_calls`, and `tool_call_id` are preserved
/// exactly; only `system` roles are dropped.
pub fn durable_conversation_messages(
    messages: impl IntoIterator<Item = ChatMessage>,
) -> Vec<ChatMessage> {
    messages
        .into_iter()
        .filter(|msg| msg.role != "system")
        .collect()
}

/// Validate each assistant invocation and its contiguous tool-result block.
/// IDs are local to a batch: reuse in a later complete invocation is allowed.
/// Only a final interrupted block may lack results in the durable projection.
pub(crate) fn validate_tool_blocks(
    messages: &[ChatMessage],
    allow_pending_tail: bool,
) -> anyhow::Result<Vec<String>> {
    use std::collections::BTreeSet;
    let mut index = 0;
    while index < messages.len() {
        let message = &messages[index];
        anyhow::ensure!(
            message.role != "tool",
            "invalid history: orphan tool result at message {index}"
        );
        if message.tool_calls.is_empty() {
            index += 1;
            continue;
        }
        anyhow::ensure!(
            message.role == "assistant",
            "invalid history: tool calls outside assistant at message {index}"
        );
        let mut pending = BTreeSet::new();
        for call in &message.tool_calls {
            let id = call
                .id
                .as_deref()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("invalid history: missing tool-call ID at message {index}")
                })?;
            anyhow::ensure!(
                pending.insert(id),
                "invalid history: duplicate tool-call ID at message {index}"
            );
        }
        index += 1;
        while index < messages.len() && messages[index].role == "tool" {
            let result = &messages[index];
            anyhow::ensure!(
                result.tool_calls.is_empty(),
                "invalid history: tool result contains calls at message {index}"
            );
            let id = result.tool_call_id.as_deref().ok_or_else(|| {
                anyhow::anyhow!("invalid history: missing result ID at message {index}")
            })?;
            anyhow::ensure!(
                pending.remove(id),
                "invalid history: duplicate or mismatched result ID at message {index}"
            );
            index += 1;
        }
        if !pending.is_empty() {
            anyhow::ensure!(
                allow_pending_tail && index == messages.len(),
                "invalid history: incomplete tool-result block before message {index}; original session retained, inspect it before retrying"
            );
            return Ok(message
                .tool_calls
                .iter()
                .filter_map(|call| call.id.as_ref())
                .filter(|id| pending.contains(id.as_str()))
                .cloned()
                .collect());
        }
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_call_msg(id: &str) -> ChatMessage {
        ChatMessage {
            reasoning: Default::default(),
            provider_state: None,
            role: "assistant".into(),
            content: None,
            tool_calls: vec![crate::llm::types::ToolCall {
                id: Some(id.into()),
                r#type: "function".into(),
                function: crate::llm::types::ToolCallFunction {
                    name: "fs_read".into(),
                    arguments: "{}".into(),
                },
            }],
            tool_call_id: None,
        }
    }

    fn tool_result_msg(id: &str, content: String) -> ChatMessage {
        ChatMessage {
            reasoning: Default::default(),
            provider_state: None,
            role: "tool".into(),
            content: Some(content),
            tool_calls: vec![],
            tool_call_id: Some(id.into()),
        }
    }

    #[test]
    fn tool_blocks_reject_interleaving_and_ambiguous_ids_without_reordering() {
        let invocation = tool_call_msg("a");
        let user = ChatMessage {
            role: "user".into(),
            content: Some("user instruction".into()),
            tool_calls: vec![],
            tool_call_id: None,
            reasoning: Default::default(),
            provider_state: None,
        };
        for bad in [
            vec![
                invocation.clone(),
                user.clone(),
                tool_result_msg("a", "real".into()),
            ],
            vec![invocation.clone(), tool_result_msg("b", "wrong".into())],
            vec![
                invocation.clone(),
                tool_result_msg("a", "real".into()),
                tool_result_msg("a", "duplicate".into()),
            ],
            vec![tool_result_msg("a", "orphan".into())],
        ] {
            let before = serde_json::to_value(&bad).unwrap();
            assert!(validate_tool_blocks(&bad, false).is_err());
            assert!(validate_tool_blocks(&bad, true).is_err());
            assert_eq!(serde_json::to_value(&bad).unwrap(), before);
        }
        let mut duplicate = invocation.clone();
        duplicate.tool_calls.push(duplicate.tool_calls[0].clone());
        assert!(validate_tool_blocks(&[duplicate], true).is_err());
        let mut missing = invocation.clone();
        missing.tool_calls[0].id = None;
        assert!(validate_tool_blocks(&[missing], true).is_err());
        let mut opaque = invocation.clone();
        opaque.provider_state = Some(crate::features::openai_subscription::ProviderState {
            version: 1,
            account: "fixture".into(),
            model: "mock".into(),
            output: vec![serde_json::json!({"encrypted_content":"opaque"})],
            additional_tool_names: Vec::new(),
        });
        let valid = vec![
            opaque,
            tool_result_msg("a", "first".into()),
            user,
            invocation.clone(),
            tool_result_msg("a", "second".into()),
        ];
        assert!(validate_tool_blocks(&valid, false).is_ok());
        assert_eq!(
            validate_tool_blocks(&[invocation], true).unwrap(),
            vec!["a"]
        );
    }

    /// P0 regression: the durable conversation buffer must never split a
    /// tool-call invocation from its tool result. Appending a very large
    /// tool result must keep both sides of the pair intact.
    #[test]
    fn durable_pair_survives_large_tool_result() {
        let mut h = ChatHistory::new();
        h.push_user("do work");
        h.append_message(tool_call_msg("call-1"));
        h.append_message(tool_result_msg("call-1", "A".repeat(20_000)));
        h.append_assistant("done");
        h.append_user("next");

        let invocation = h.build_messages().iter().any(|m| {
            m.role == "assistant"
                && m.tool_calls
                    .iter()
                    .any(|tc| tc.id.as_deref() == Some("call-1"))
        });
        let result = h
            .build_messages()
            .iter()
            .any(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some("call-1"));
        assert!(
            invocation && result,
            "tool-call invocation and its result must stay paired"
        );
    }

    #[test]
    fn parallel_tool_calls_stay_paired_and_ordered() {
        let mut h = ChatHistory::new();
        h.push_user("batch");
        h.append_message(ChatMessage {
            reasoning: Default::default(),
            provider_state: None,
            role: "assistant".into(),
            content: None,
            tool_calls: vec![
                crate::llm::types::ToolCall {
                    id: Some("call-a".into()),
                    r#type: "function".into(),
                    function: crate::llm::types::ToolCallFunction {
                        name: "fs_read".into(),
                        arguments: "{\"path\":\"a\"}".into(),
                    },
                },
                crate::llm::types::ToolCall {
                    id: Some("call-b".into()),
                    r#type: "function".into(),
                    function: crate::llm::types::ToolCallFunction {
                        name: "fs_read".into(),
                        arguments: "{\"path\":\"b\"}".into(),
                    },
                },
            ],
            tool_call_id: None,
        });
        h.append_message(tool_result_msg("call-a", "content-a".into()));
        h.append_message(tool_result_msg("call-b", "content-b".into()));

        let ids: Vec<_> = h
            .snapshot()
            .iter()
            .filter(|m| m.role == "tool")
            .filter_map(|m| m.tool_call_id.clone())
            .collect();
        assert_eq!(ids, vec!["call-a".to_string(), "call-b".to_string()]);
    }

    #[test]
    fn provider_state_messages_preserved_exactly() {
        let state = crate::features::openai_subscription::ProviderState {
            version: 1,
            account: "test-account".into(),
            model: "test-model".into(),
            output: vec![serde_json::json!({"type": "reasoning", "id": "rs_1"})],
            additional_tool_names: Vec::new(),
        };
        let mut h = ChatHistory::new();
        h.push_message(ChatMessage {
            reasoning: Default::default(),
            provider_state: Some(state),
            role: "assistant".into(),
            content: Some("answer".into()),
            tool_calls: vec![],
            tool_call_id: None,
        });
        let snapshot = h.snapshot();
        assert_eq!(snapshot.len(), 1);
        let round_tripped: ChatMessage =
            serde_json::from_value(serde_json::to_value(&snapshot[0]).expect("serialize"))
                .expect("deserialize");
        assert_eq!(
            round_tripped.provider_state.as_ref().expect("state").output,
            snapshot[0].provider_state.as_ref().expect("state").output
        );
    }

    #[test]
    fn replace_accepts_shorter_canonical_history() {
        let mut h = ChatHistory::new();
        for i in 0..10 {
            h.push_user(format!("message {i}"));
        }
        // Compaction may return fewer messages in a new order; the canonical
        // result replaces the buffer as-is, never via index/count deltas.
        let compacted = vec![h.snapshot()[9].clone()];
        h.replace(compacted.clone());
        assert_eq!(h.len(), 1);
        assert_eq!(
            h.snapshot()[0].content.as_deref(),
            compacted[0].content.as_deref()
        );
    }

    #[test]
    fn durable_filter_strips_system_only() {
        let messages = vec![
            ChatMessage {
                reasoning: Default::default(),
                provider_state: None,
                role: "system".into(),
                content: Some("default prompt".into()),
                tool_calls: vec![],
                tool_call_id: None,
            },
            ChatMessage {
                reasoning: Default::default(),
                provider_state: None,
                role: "user".into(),
                content: Some("hi".into()),
                tool_calls: vec![],
                tool_call_id: None,
            },
            tool_call_msg("call-1"),
            tool_result_msg("call-1", "output".into()),
            ChatMessage {
                reasoning: Default::default(),
                provider_state: None,
                role: "system".into(),
                content: Some("loop intervention".into()),
                tool_calls: vec![],
                tool_call_id: None,
            },
        ];
        let durable = durable_conversation_messages(messages);
        assert_eq!(durable.len(), 3);
        assert!(durable.iter().all(|m| m.role != "system"));
        assert_eq!(durable[0].role, "user");
        assert_eq!(durable[2].tool_call_id.as_deref(), Some("call-1"));
    }

    #[test]
    fn container_basics() {
        let mut h = ChatHistory::from_messages(vec![]);
        assert!(h.is_empty());
        assert_eq!(h.len(), 0);
        h.push_user("hello");
        assert!(!h.is_empty());
        assert_eq!(h.len(), 1);
        h.clear();
        assert!(h.is_empty());
    }
}
