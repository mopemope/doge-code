use crate::llm::types::ChatMessage;

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
        self.trim_to_max();
    }

    #[allow(dead_code)]
    pub fn append_assistant(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMessage {
            role: "assistant".into(),
            content: Some(content.into()),
            tool_calls: vec![],
            tool_call_id: None,
        });
        self.trim_to_max();
    }

    #[allow(dead_code)]
    pub fn build_messages(&self) -> Vec<ChatMessage> {
        self.messages.clone()
    }

    fn estimate_tokens(content: &str) -> usize {
        // Approximate: 1 token ~= 4 chars
        if content.is_empty() {
            0
        } else {
            content.len().div_ceil(4)
        }
    }

    fn trim_to_max(&mut self) {
        // Keep total estimated token count under max_tokens, preserving system message at index 0 if present.
        let mut total_tokens: usize = self
            .messages
            .iter()
            .map(|m| Self::estimate_tokens(m.content.as_deref().unwrap_or("")))
            .sum();

        while total_tokens > self.max_tokens && !self.messages.is_empty() {
            let has_system = self
                .messages
                .first()
                .map(|m| m.role == "system")
                .unwrap_or(false);

            let remove_index = if has_system {
                if self.messages.len() > 1 {
                    1
                } else {
                    // Only system message left. Stop trimming to preserve system prompt.
                    break;
                }
            } else {
                0
            };

            let removed = self.messages.remove(remove_index);
            total_tokens -= Self::estimate_tokens(removed.content.as_deref().unwrap_or(""));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_by_tokens() {
        // max_tokens = 10. "12345678" is 8 chars -> 2 tokens.
        // We want to test that it holds enough messages.
        // "1234" -> 1 token.
        let mut h = ChatHistory::new(3, None); // Max 3 tokens
        h.append_user("1234"); // 1 token
        h.append_assistant("5678"); // 1 token
        h.append_user("abcd"); // 1 token
        // Total 3 tokens. Should fit.
        assert_eq!(h.build_messages().len(), 3);

        h.append_user("efgh"); // 1 token. Total 4 > 3. Should remove first (index 0 as no system).
        assert_eq!(h.build_messages().len(), 3);
        assert_eq!(h.build_messages()[0].content.as_deref(), Some("5678"));
    }

    #[test]
    fn keeps_system_first() {
        let mut h = ChatHistory::new(10, Some("sys".into())); // sys -> 1 token
        h.append_system_once();
        h.append_user("12345678"); // 2 tokens
        h.append_assistant("abcdefgh"); // 2 tokens
        // Total 5 tokens.
        let msgs = h.build_messages();
        assert_eq!(msgs.first().unwrap().role, "system");
    }
}
