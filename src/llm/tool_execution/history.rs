use crate::llm::types::ChatMessage;
use crate::llm::{OpenAIClient, compact_conversation_history};
use anyhow::{Result, anyhow};
use tracing::{error, info, warn};

pub struct HistoryManager {
    messages: Vec<ChatMessage>,
    client: OpenAIClient,
    ui_tx: Option<std::sync::mpsc::Sender<String>>,
    fs_tools: crate::tools::FsTools,
    config: crate::config::AppConfig,
}

impl HistoryManager {
    pub fn new(
        client: OpenAIClient,
        messages: Vec<ChatMessage>,
        ui_tx: Option<std::sync::mpsc::Sender<String>>,
        fs_tools: crate::tools::FsTools,
        config: crate::config::AppConfig,
    ) -> Self {
        Self {
            messages,
            client,
            ui_tx,
            fs_tools,
            config,
        }
    }

    /// Add a message to the history
    pub fn push(&mut self, message: ChatMessage) {
        self.messages.push(message);
    }

    /// Insert a message at a specific index
    pub fn insert(&mut self, index: usize, message: ChatMessage) {
        self.messages.insert(index, message);
    }

    pub fn last(&self) -> Option<&ChatMessage> {
        self.messages.last()
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }

    pub fn iter(&self) -> std::slice::Iter<'_, ChatMessage> {
        self.messages.iter()
    }

    pub fn as_slice(&self) -> &[ChatMessage] {
        self.messages.as_slice()
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub async fn inject_context(&mut self) -> Result<()> {
        // 1. Context Manager (File Access)
        let cm = self.fs_tools.context_manager.read().await;
        let context_prompt = cm.get_context_prompt().await;

        // 2. Memory Search (Smart Context)
        // Extract the user's goal from the last user message
        let user_goal = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .and_then(|m| m.content.clone());

        let memory_context = if let Some(goal) = user_goal {
            match self.fs_tools.search_memory(Some(goal.clone()), None).await {
                Ok(result) => {
                    if result.contains("No matching memories found")
                        || result.contains("No memories found")
                    {
                        None
                    } else {
                        Some(result)
                    }
                }
                Err(e) => {
                    warn!("Failed to search memory: {}", e);
                    None
                }
            }
        } else {
            None
        };

        let mut combined_context = String::new();
        if !context_prompt.is_empty() {
            combined_context.push_str(&context_prompt);
            combined_context.push_str("\n\n");
        }

        if let Some(mem) = memory_context {
            combined_context.push_str("<RelevantMemory>\n");
            combined_context.push_str(&mem);
            combined_context.push_str("\n</RelevantMemory>");
        }

        if !combined_context.is_empty() {
            let context_msg = ChatMessage {
                role: "system".into(),
                content: Some(combined_context),
                tool_calls: vec![],
                tool_call_id: None,
            };
            // Insert before the last message if it's a User message to provide immediate context
            if !self.messages.is_empty()
                && self
                    .messages
                    .last()
                    .map(|m| m.role == "user")
                    .unwrap_or(false)
            {
                let idx = self.messages.len() - 1;
                self.messages.insert(idx, context_msg);
            } else {
                self.messages.push(context_msg);
            }
        }
        Ok(())
    }

    /// Clear stale tool results to free context space before compaction is
    /// needed (mirrors "context editing": old tool outputs are replaced with a
    /// short placeholder while the conversation flow is preserved).
    ///
    /// Only runs once the prompt token usage crosses `threshold * ratio`, and
    /// never touches the most recent `keep_recent` tool messages.
    pub fn clear_stale_tool_results(
        messages: &mut [ChatMessage],
        prompt_tokens: u32,
        threshold: u32,
        ratio: f32,
        keep_recent: usize,
    ) -> usize {
        if threshold == 0 || (prompt_tokens as f32) < threshold as f32 * ratio {
            return 0;
        }

        // Collect indices of tool messages.
        let tool_indices: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == "tool" && m.content.is_some())
            .map(|(i, _)| i)
            .collect();

        if tool_indices.len() <= keep_recent {
            return 0;
        }

        let cutoff = tool_indices.len() - keep_recent;
        let mut cleared = 0usize;
        for &idx in &tool_indices[..cutoff] {
            let msg = &mut messages[idx];
            let already_cleared = msg
                .content
                .as_deref()
                .is_some_and(|c| c.starts_with("[cleared tool result"));
            if already_cleared {
                continue;
            }
            msg.content = Some("[cleared tool result: earlier tool output removed to free context; re-run the tool if needed]".to_string());
            cleared += 1;
        }
        cleared
    }

    /// Check if proactive compaction is needed and perform it if so
    pub async fn check_and_compact_proactive(&mut self) -> Result<bool> {
        // Free stale tool results first (context editing) before considering
        // a full compaction.
        let cleared = Self::clear_stale_tool_results(
            self.messages.as_mut_slice(),
            self.client.get_prompt_tokens_used(),
            self.config.get_effective_compaction_limit(),
            0.6,
            3,
        );
        if cleared > 0 {
            info!(cleared, "Cleared stale tool results to free context");
            if let Some(tx) = &self.ui_tx {
                let _ = tx.send(format!(
                    "::status:waiting:Cleared {cleared} stale tool result(s) to free context..."
                ));
            }
        }

        let last_prompt_tokens = self.client.get_prompt_tokens_used();
        let effective_limit = self.config.get_effective_compaction_limit();

        // Only compact if we are over the limit AND we have enough history to meaningful compact
        if last_prompt_tokens > effective_limit && self.messages.len() > 2 {
            warn!(
                current_tokens = last_prompt_tokens,
                limit = effective_limit,
                "Proactive compaction triggered"
            );

            if let Some(tx) = &self.ui_tx {
                let _ = tx.send(
                    "::status:compacting:Context limits approaching, summarizing history..."
                        .to_string(),
                );
            }

            self.perform_compaction().await
        } else {
            Ok(false)
        }
    }

    /// Reactive compaction when context length is exceeded
    pub async fn compact_reactive(&mut self) -> Result<bool> {
        warn!("Context length exceeded in agent loop. Attempting to compact history.");

        if let Some(tx) = &self.ui_tx {
            let _ = tx.send(
                "::status:compacting:Context limits reached, summarizing history...".to_string(),
            );
        }

        self.perform_compaction().await
    }

    pub fn into_messages(self) -> Vec<ChatMessage> {
        self.messages
    }

    async fn perform_compaction(&mut self) -> Result<bool> {
        let params = crate::llm::compact_history::CompactParams {
            client: self.client.clone(),
            model: self.config.model.clone(),
            fs_tools: self.fs_tools.clone(),
            history: self.messages.clone(),
            cfg: self.config.clone(),
        };

        match compact_conversation_history(params).await {
            Ok(compact_result) => {
                if compact_result.metadata.success {
                    info!("History compaction successful.");

                    // Preserve System Prompt AND Loop Intervention messages
                    self.messages = Self::merge_compacted_history(
                        &self.messages,
                        compact_result.compacted_message,
                    );

                    if let Some(tx) = &self.ui_tx {
                        let _ = tx
                            .send("::status:waiting:History compacted. Continuing...".to_string());
                    }
                    Ok(true)
                } else {
                    let err_msg = format!(
                        "Compaction failed: {:?}",
                        compact_result.metadata.error_message
                    );
                    error!("{}", err_msg);
                    // Return error but as a result, not breaking execution if possible
                    // Actually, if compaction fails, we can't do much. returning false or error
                    Err(anyhow!(err_msg))
                }
            }
            Err(e) => {
                error!("Compaction error: {}", e);
                Err(e)
            }
        }
    }

    /// Helper to merge existing system messages with the compacted state and
    /// a short tail of recent messages.
    ///
    /// Keeps:
    /// - pruned system messages (deduped, bounded) so the system prompt and
    ///   recent interventions survive,
    /// - the compacted summary,
    /// - the most recent non-system messages (within a character budget) so
    ///   the model can continue mid-task without re-discovering state.
    fn merge_compacted_history(
        original: &[ChatMessage],
        compacted: ChatMessage,
    ) -> Vec<ChatMessage> {
        const TAIL_BUDGET_CHARS: usize = 8_000;
        const MAX_SYSTEM_MESSAGES: usize = 8;
        const MAX_SYSTEM_MESSAGE_CHARS: usize = 4_000;

        let mut new_history: Vec<ChatMessage> = Self::prune_system_messages(
            original.iter().filter(|m| m.role == "system"),
            MAX_SYSTEM_MESSAGES,
            MAX_SYSTEM_MESSAGE_CHARS,
        );
        new_history.push(compacted);
        new_history.extend(Self::tail_messages(original, TAIL_BUDGET_CHARS));
        new_history
    }

    /// Dedupe system messages (by normalized prefix) and cap their count and
    /// size so loop warnings cannot accumulate unbounded across compactions.
    fn prune_system_messages<'a, I>(
        messages: I,
        max_count: usize,
        max_chars: usize,
    ) -> Vec<ChatMessage>
    where
        I: Iterator<Item = &'a ChatMessage>,
    {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut kept = Vec::new();
        let mut overflow = 0usize;
        for msg in messages {
            let content = msg.content.as_deref().unwrap_or("");
            let key: String = content
                .chars()
                .take(80)
                .map(|c| if c.is_whitespace() { ' ' } else { c })
                .collect();
            if !seen.insert(key) {
                // Skip repeated interventions with the same content prefix.
                continue;
            }
            if kept.len() >= max_count {
                overflow += 1;
                continue;
            }
            let mut msg = msg.clone();
            if content.chars().count() > max_chars {
                let mut cut = max_chars;
                while cut > 0 && !content.is_char_boundary(cut) {
                    cut -= 1;
                }
                msg.content = Some(format!("{}\n[system message truncated]", &content[..cut]));
            }
            kept.push(msg);
        }
        if overflow > 0 {
            tracing::debug!(
                dropped = overflow,
                "dropped overflow system messages during compaction"
            );
        }
        kept
    }

    /// Take the most recent non-system messages whose combined size fits the
    /// budget, capped to the last few conversation units. Tool-call pairing is
    /// preserved: an assistant message with tool calls and its tool responses
    /// are kept or dropped as a unit.
    fn tail_messages(original: &[ChatMessage], budget: usize) -> Vec<ChatMessage> {
        const MAX_TAIL_GROUPS: usize = 3;

        let msg_len = |m: &ChatMessage| -> usize {
            let mut len = m.content.as_deref().map(str::len).unwrap_or(0);
            for tc in &m.tool_calls {
                len += tc.function.name.len() + tc.function.arguments.len();
            }
            len
        };

        // Group each assistant tool-call message with the tool responses that
        // immediately follow it so we never cut a pair in half.
        let mut groups: Vec<Vec<&ChatMessage>> = Vec::new();
        for msg in original.iter().filter(|m| m.role != "system") {
            match msg.role.as_str() {
                "assistant" if !msg.tool_calls.is_empty() => groups.push(vec![msg]),
                "tool" => {
                    if let Some(group) = groups.last_mut()
                        && group.first().is_some_and(|head| {
                            head.role == "assistant" && !head.tool_calls.is_empty()
                        })
                    {
                        group.push(msg);
                    } else {
                        groups.push(vec![msg]);
                    }
                }
                _ => groups.push(vec![msg]),
            }
        }

        // Only consider the most recent few units.
        let window_start = groups.len().saturating_sub(MAX_TAIL_GROUPS);

        let mut total = 0usize;
        let mut take_from = groups.len();
        for (idx, group) in groups.iter().enumerate().rev() {
            let group_len: usize = group.iter().map(|m| msg_len(m)).sum();
            if idx < window_start {
                break;
            }
            if total + group_len > budget {
                break;
            }
            total += group_len;
            take_from = idx;
        }

        if take_from >= groups.len() {
            return Vec::new();
        }
        groups[take_from..]
            .iter()
            .flat_map(|group| group.iter().map(|m| (*m).clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: Some(content.to_string()),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    fn make_tool_msg(tool_call_id: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: "tool".to_string(),
            content: Some(content.to_string()),
            tool_calls: vec![],
            tool_call_id: Some(tool_call_id.to_string()),
        }
    }

    fn make_assistant_with_tool_calls(call_id: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            content: Some(content.to_string()),
            tool_calls: vec![crate::llm::types::ToolCall {
                id: Some(call_id.to_string()),
                r#type: "function".to_string(),
                function: crate::llm::types::ToolCallFunction {
                    name: "fs_read".to_string(),
                    arguments: "{}".to_string(),
                },
            }],
            tool_call_id: None,
        }
    }

    #[test]
    fn test_merge_compacted_history_keeps_system_summary_and_tail() {
        let original = vec![
            make_msg("system", "System Prompt"),
            make_msg("user", "User 1"),
            make_msg("assistant", "Assistant 1"),
            make_msg("system", "Loop Warning"),
            make_msg("user", "User 2"),
        ];

        let compacted = make_msg("user", "Summary");

        let result = HistoryManager::merge_compacted_history(&original, compacted);

        assert_eq!(result.len(), 6);
        assert_eq!(result[0].role, "system");
        assert_eq!(result[0].content.as_deref(), Some("System Prompt"));
        assert_eq!(result[1].role, "system");
        assert_eq!(result[1].content.as_deref(), Some("Loop Warning"));
        assert_eq!(result[2].role, "user");
        assert_eq!(result[2].content.as_deref(), Some("Summary"));
        // Recent tail preserved after the summary.
        assert_eq!(result[3].content.as_deref(), Some("User 1"));
        assert_eq!(result[4].content.as_deref(), Some("Assistant 1"));
        assert_eq!(result[5].content.as_deref(), Some("User 2"));
    }

    #[test]
    fn test_merge_compacted_history_no_system_messages() {
        let original = vec![
            make_msg("user", "User 1"),
            make_msg("assistant", "Assistant 1"),
        ];

        let compacted = make_msg("user", "Summary");

        let result = HistoryManager::merge_compacted_history(&original, compacted);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].content.as_deref(), Some("Summary"));
        assert_eq!(result[1].content.as_deref(), Some("User 1"));
    }

    #[test]
    fn test_merge_compacted_history_keeps_tool_call_pairs_intact() {
        let original = vec![
            make_msg("system", "System Prompt"),
            make_msg("user", "Task"),
            make_assistant_with_tool_calls("call_1", "reading file"),
            make_tool_msg("call_1", "{\"content\": \"file body\"}"),
            make_msg("assistant", "done"),
        ];

        let compacted = make_msg("user", "Summary");
        let result = HistoryManager::merge_compacted_history(&original, compacted);

        // Summary + tail (user, assistant+tool pair, assistant).
        assert_eq!(result.len(), 6);
        // The tail must not start with an orphan tool message.
        assert_eq!(result[2].role, "user");
        assert_eq!(result[3].role, "assistant");
        assert_eq!(result[4].role, "tool");
        assert_eq!(result[4].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(result[5].role, "assistant");
    }

    #[test]
    fn test_merge_compacted_history_dedupes_repeated_system_warnings() {
        let original: Vec<ChatMessage> = vec![
            make_msg("system", "System Prompt"),
            make_msg("system", "WARNING: stalled progress"),
            make_msg("user", "u1"),
            make_msg("system", "WARNING: stalled progress"),
            make_msg("system", "WARNING: stalled progress"),
        ];
        let compacted = make_msg("user", "Summary");
        let result = HistoryManager::merge_compacted_history(&original, compacted);
        let system_texts: Vec<_> = result
            .iter()
            .filter(|m| m.role == "system")
            .map(|m| m.content.clone().unwrap_or_default())
            .collect();
        assert_eq!(
            system_texts.len(),
            2,
            "dedupe repeated warnings: {system_texts:?}"
        );
    }

    #[test]
    fn test_merge_compacted_history_tail_respects_budget() {
        let big = "x".repeat(8_500);
        let original = vec![
            make_msg("system", "System Prompt"),
            make_msg("user", "old task"),
            make_msg("assistant", big.as_str()),
            make_msg("assistant", "middle"),
            make_msg("user", "recent question"),
        ];
        let compacted = make_msg("user", "Summary");
        let result = HistoryManager::merge_compacted_history(&original, compacted);

        // Tail keeps at most the last 3 units within the 8,000 budget: the
        // 8,500-char assistant message exceeds it, so the tail is
        // ["middle", "recent question"] and older units are dropped.
        let contents: Vec<_> = result
            .iter()
            .map(|m| m.content.clone().unwrap_or_default())
            .collect();
        assert!(contents.contains(&"recent question".to_string()));
        assert!(contents.contains(&"middle".to_string()));
        assert!(!contents.contains(&big));
        assert!(!contents.contains(&"old task".to_string()));
    }

    #[test]
    fn test_prune_system_messages_caps_count_and_keeps_first() {
        let original: Vec<ChatMessage> = (0..20)
            .map(|i| {
                make_msg(
                    "system",
                    &format!("unique-warning-{i} with enough text here!!"),
                )
            })
            .collect();
        let pruned = HistoryManager::prune_system_messages(original.iter(), 8, 4_000);
        assert_eq!(pruned.len(), 8);
    }

    #[test]
    fn test_clear_stale_tool_results_clears_old_but_keeps_recent() {
        let mut messages = vec![
            make_msg("system", "System Prompt"),
            make_msg("user", "task"),
            make_tool_msg("c1", "old result 1"),
            make_tool_msg("c2", "old result 2"),
            make_tool_msg("c3", "old result 3"),
            make_tool_msg("c4", "recent result 4"),
            make_tool_msg("c5", "recent result 5"),
        ];

        let cleared = HistoryManager::clear_stale_tool_results(&mut messages, 800, 1_000, 0.6, 2);
        assert_eq!(cleared, 3);
        assert!(
            messages[2]
                .content
                .as_deref()
                .unwrap()
                .starts_with("[cleared tool result"),
            "old results cleared"
        );
        assert_eq!(messages[5].content.as_deref(), Some("recent result 4"));
        assert_eq!(messages[6].content.as_deref(), Some("recent result 5"));
    }

    #[test]
    fn test_clear_stale_tool_results_skips_below_threshold() {
        let mut messages = vec![
            make_tool_msg("c1", "result"),
            make_tool_msg("c2", "result"),
            make_tool_msg("c3", "result"),
            make_tool_msg("c4", "result"),
        ];
        let cleared = HistoryManager::clear_stale_tool_results(&mut messages, 100, 1_000, 0.6, 2);
        assert_eq!(cleared, 0);
        assert_eq!(messages[0].content.as_deref(), Some("result"));
    }

    #[test]
    fn test_clear_stale_tool_results_idempotent() {
        let mut messages = vec![
            make_tool_msg("c1", "old"),
            make_tool_msg("c2", "old"),
            make_tool_msg("c3", "old"),
            make_tool_msg("c4", "recent"),
        ];
        let first = HistoryManager::clear_stale_tool_results(&mut messages, 900, 1_000, 0.6, 1);
        let second = HistoryManager::clear_stale_tool_results(&mut messages, 900, 1_000, 0.6, 1);
        assert_eq!(first, 3);
        assert_eq!(second, 0, "already-cleared messages are not double-counted");
    }
}
