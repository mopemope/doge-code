use crate::llm::client::ChatMessage;

#[derive(Debug, Clone)]
pub struct ChatHistory {
    messages: Vec<ChatMessage>,
    max_chars: usize,
    system_added: bool,
    system_prompt: Option<String>,
}

impl ChatHistory {
    pub fn new(max_chars: usize, system_prompt: Option<String>) -> Self {
        Self {
            messages: Vec::new(),
            max_chars,
            system_added: false,
            system_prompt,
        }
    }

    pub fn append_user(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMessage {
            role: "user".into(),
            content: content.into(),
        });
        self.trim_if_needed();
    }

    pub fn append_assistant(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMessage {
            role: "assistant".into(),
            content: content.into(),
        });
        self.trim_if_needed();
    }

    pub fn append_system_once(&mut self) {
        if !self.system_added {
            if let Some(sp) = self.system_prompt.clone() {
                self.messages.insert(
                    0,
                    ChatMessage {
                        role: "system".into(),
                        content: sp,
                    },
                );
            }
            self.system_added = true;
            self.trim_if_needed();
        }
    }

    pub fn build_messages(&self) -> Vec<ChatMessage> {
        self.messages.clone()
    }

    fn trim_if_needed(&mut self) {
        // Rough heuristic: limit by characters across all contents
        let mut total: usize = self.messages.iter().map(|m| m.content.len()).sum();
        while total > self.max_chars && self.messages.len() > 1 {
            // Preserve system message if present at index 0
            let start_idx = if self.system_added { 1 } else { 0 };
            if self.messages.len() > start_idx {
                let removed = self.messages.remove(start_idx);
                total = total.saturating_sub(removed.content.len());
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_by_chars() {
        let mut h = ChatHistory::new(10, None);
        h.append_user("12345");
        h.append_assistant("67890");
        h.append_user("abcde");
        assert!(h.build_messages().len() <= 3);
        let sum: usize = h.build_messages().iter().map(|m| m.content.len()).sum();
        assert!(sum <= 10);
    }

    #[test]
    fn keeps_system_first() {
        let mut h = ChatHistory::new(10, Some("sys".into()));
        h.append_system_once();
        h.append_user("12345");
        h.append_assistant("67890");
        let msgs = h.build_messages();
        assert_eq!(msgs.first().unwrap().role, "system");
    }
}
