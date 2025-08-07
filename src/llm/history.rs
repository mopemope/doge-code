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

    pub fn append_system_once(&mut self) {
        if self.system_added {
            return;
        }
        if let Some(sys) = self.system_prompt.clone() {
            self.messages.insert(
                0,
                ChatMessage {
                    role: "system".into(),
                    content: sys,
                },
            );
            self.system_added = true;
        }
    }

    #[allow(dead_code)]
    pub fn append_user(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMessage {
            role: "user".into(),
            content: content.into(),
        });
        self.trim_to_max();
    }

    #[allow(dead_code)]
    pub fn append_assistant(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMessage {
            role: "assistant".into(),
            content: content.into(),
        });
        self.trim_to_max();
    }

    #[allow(dead_code)]
    pub fn build_messages(&self) -> Vec<ChatMessage> {
        self.messages.clone()
    }

    fn trim_to_max(&mut self) {
        // Keep total character count under max_chars, preserving system message at index 0 if present.
        let mut total: usize = self.messages.iter().map(|m| m.content.len()).sum();
        while total > self.max_chars && self.messages.len() > 1 {
            // remove the oldest non-system (index 1)
            let removed = self.messages.remove(1);
            total -= removed.content.len();
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
