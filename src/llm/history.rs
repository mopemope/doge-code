use crate::llm::types::ChatMessage;
use tracing::debug;

#[derive(Debug, Clone)]
pub struct ChatHistory {
    messages: Vec<ChatMessage>,
    max_tokens: usize,
    system_added: bool,
    system_prompt: Option<String>,
}

impl ChatHistory {
    pub fn new(max_tokens: usize, system_prompt: Option<String>) -> Self {
        Self {
            messages: Vec::new(),
            max_tokens,
            system_added: false,
            system_prompt,
        }
    }

    pub fn append_system_once(&mut self) {
        if self.system_added {
            return;
        }
        if let Some(sys) = self.system_prompt.clone() {
            self.messages.insert(
                0,
                ChatMessage {
                    role: "system".into(),
                    content: Some(sys),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
            );
            self.system_added = true;
        }
    }

    #[allow(dead_code)]
    pub fn append_user(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMessage {
            role: "user".into(),
            content: Some(content.into()),
            tool_calls: vec![],
            tool_call_id: None,
        });
        self.smart_trim();
    }

    #[allow(dead_code)]
    pub fn append_assistant(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMessage {
            role: "assistant".into(),
            content: Some(content.into()),
            tool_calls: vec![],
            tool_call_id: None,
        });
        self.smart_trim();
    }

    /// Appends a generic message (e.g. tool output)
    pub fn append_message(&mut self, msg: ChatMessage) {
        if msg.role == "system" {
            // Only add system prompt if not already set, or replace it?
            // For restoration, we might want to ensure it's at index 0.
            // append_system_once handles index 0.
            self.append_system_once();
            // Note: msg.content might differ from stored system_prompt.
            // If we are restoring history, we should trust the message.
            // But append_system_once uses self.system_prompt.
            // Let's just push generic if it's not the initial system prompt?
            // Or better: ensure system prompt is always index 0.
            if !self.system_added {
                self.messages.insert(0, msg);
                self.system_added = true;
            }
        } else {
            self.messages.push(msg);
        }
        self.smart_trim();
    }

    #[allow(dead_code)]
    pub fn build_messages(&self) -> Vec<ChatMessage> {
        self.messages.clone()
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.system_added = false;
    }

    /// Overwrites the current history with the provided messages.
    /// This is useful for transferring context between executors.
    pub fn overwrite_messages(&mut self, messages: Vec<ChatMessage>) {
        self.messages = messages;
        // Check if system prompt is present in the beginning
        if let Some(first) = self.messages.first()
            && first.role == "system"
        {
            self.system_added = true;
        }
        self.smart_trim();
    }

    /// Estimates tokens for a message including content and tool calls.
    /// Uses a heuristic of 4 chars per token.
    fn estimate_tokens(msg: &ChatMessage) -> usize {
        let mut count = 0;
        if let Some(c) = &msg.content
            && !c.is_empty()
        {
            count += c.len().div_ceil(4);
        }

        // Add tokens for tool calls overhead
        for tool in &msg.tool_calls {
            count += tool.function.name.len().div_ceil(4);
            count += tool.function.arguments.len().div_ceil(4);
            // Extra constant overhead for JSON structure
            count += 10;
        }

        // Add tokens for tool_call_id (if present, usually in tool response)
        if let Some(id) = &msg.tool_call_id {
            count += id.len().div_ceil(4);
        }

        // Base message overhead
        count + 3
    }

    /// Trims messages to stay within max_tokens using a smart eviction strategy.
    /// Priority for retention:
    /// 1. System Prompt (Index 0) - Always kept
    /// 2. Last 3 messages (Recent context) - Pinned
    /// 3. User/Assistant conversation - Standard priority
    /// 4. Tool outputs (role="tool") - First to go
    fn smart_trim(&mut self) {
        let mut total_tokens: usize = self.messages.iter().map(Self::estimate_tokens).sum();

        if total_tokens <= self.max_tokens {
            return;
        }

        debug!("Trimming history: {} > {}", total_tokens, self.max_tokens);

        while total_tokens > self.max_tokens && self.messages.len() > 1 {
            // Find the best candidate to remove.
            // We never remove the system message (index 0).
            // We try to preserve the last few messages.

            let len = self.messages.len();
            // Define protected window (e.g., last 2 messages)
            let protected_start = len.saturating_sub(2);

            let mut best_index = None;
            let mut lowest_score = i32::MAX;

            // Scan candidates between index 1 and protected_start
            // If protected_start <= 1, we just remove index 1 (FIFO fallback)
            let scan_end = if protected_start > 1 {
                protected_start
            } else {
                1
            };

            // If we have very few messages, we might be forced to eat into "protected" if specific checks fail,
            // but loop condition handles empty details.
            if len <= 2 {
                // Only system + 1 message left? Or system + user?
                // We shouldn't remove system. If len=2 (sys, msg), we might have to remove msg if it's too huge.
                best_index = Some(1);
            } else {
                // Scoring loop: Lower score = Higher chance of eviction
                for i in 1..scan_end {
                    let msg = &self.messages[i];
                    let score = match msg.role.as_str() {
                        "tool" => 10,                                    // Ephemeral tool output
                        "assistant" if !msg.tool_calls.is_empty() => 20, // Tool call invocation
                        "assistant" => 50,                               // Normal answer
                        "user" => 60,                                    // User instruction
                        _ => 40,                                         // Other?
                    };

                    if score < lowest_score {
                        lowest_score = score;
                        best_index = Some(i);
                    }
                }

                // If scanned range was empty or no candidate found (shouldn't happen),
                // fallback to FIFO (index 1)
                if best_index.is_none() {
                    best_index = Some(1);
                }
            }

            if let Some(idx) = best_index {
                let removed = self.messages.remove(idx);
                let freed = Self::estimate_tokens(&removed);
                debug!(
                    "Evicted message at index {} (role: {}), freed {} tokens",
                    idx, removed.role, freed
                );
                total_tokens = total_tokens.saturating_sub(freed);
            } else {
                break; // Should not happen
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_by_smart_priority() {
        // Setup: Max 100 tokens.
        // 1. System (kept)
        // 2. User "Task"
        // 3. Assistant "Tool Call"
        // 4. Tool "Huge Output" (Target for eviction)
        // 5. Assistant "Result"
        let mut h = ChatHistory::new(50, Some("sys".into())); // Small limit to force trim
        h.append_system_once();

        let tool_call_msg = ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: vec![crate::llm::types::ToolCall {
                id: Some("call_1".into()),
                r#type: "function".into(),
                function: crate::llm::types::ToolCallFunction {
                    name: "ls".into(),
                    arguments: "{}".into(),
                },
            }],
            tool_call_id: None,
        };

        // Manual push to control roles
        h.messages.push(ChatMessage {
            role: "system".into(),
            content: Some("sys".into()),
            tool_calls: vec![],
            tool_call_id: None,
        });
        h.messages.push(ChatMessage {
            role: "user".into(),
            content: Some("Do work".into()),
            tool_calls: vec![],
            tool_call_id: None,
        });
        h.messages.push(tool_call_msg);

        // Big tool output
        h.messages.push(ChatMessage {
            role: "tool".into(),
            content: Some("A".repeat(200)), // ~50 tokens alone
            tool_calls: vec![],
            tool_call_id: Some("call_1".into()),
        });

        // Add padding messages to ensure the tool message is not in the protected last 2
        h.messages.push(ChatMessage {
            role: "assistant".into(),
            content: Some("ok".into()),
            tool_calls: vec![],
            tool_call_id: None,
        });
        h.messages.push(ChatMessage {
            role: "user".into(),
            content: Some("next".into()),
            tool_calls: vec![],
            tool_call_id: None,
        });

        // Trigger trim
        h.smart_trim();

        // Expectation: The "tool" message (role: tool) should be evicted first.
        let roles: Vec<&str> = h.messages.iter().map(|m| m.role.as_str()).collect();
        assert!(roles.contains(&"system"));
        assert!(roles.contains(&"user")); // User intent preserved
        assert!(!roles.contains(&"tool")); // Tool output evicted
    }

    #[test]
    fn keeps_system_first() {
        let mut h = ChatHistory::new(10, Some("sys".into()));
        h.append_system_once();
        h.append_user("12345678");
        h.append_assistant("abcdefgh");
        let msgs = h.build_messages();
        assert_eq!(msgs.first().unwrap().role, "system");
    }

    #[test]
    fn estimate_includes_tool_calls() {
        let msg = ChatMessage {
            role: "assistant".into(),
            content: Some("Thinking...".into()),
            tool_calls: vec![crate::llm::types::ToolCall {
                id: Some("123".into()),
                r#type: "function".into(),
                function: crate::llm::types::ToolCallFunction {
                    name: "test_tool".into(),              // 9 chars
                    arguments: "{\"key\":\"val\"}".into(), // 13 chars
                },
            }],
            tool_call_id: None,
        };
        let tokens = ChatHistory::estimate_tokens(&msg);
        // Content: "Thinking..." (11) -> 3
        // Tool: "test_tool" (9) -> 3, args (13) -> 4, overhead 10 -> 17
        // Base overhead: 3
        // Total ~23
        assert!(tokens > 10);
    }
}
