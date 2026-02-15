use crate::llm::types::ChatMessage;
use crate::llm::{OpenAIClient, compact_conversation_history};
use anyhow::{Result, anyhow};
use tracing::{error, info, warn};

pub struct HistoryManager {
    messages: Vec<ChatMessage>,
    client: OpenAIClient,
    model: String,
    auto_compact_threshold: u32,
    context_window_size: u32,
    ui_tx: Option<std::sync::mpsc::Sender<String>>,
    fs_tools: crate::tools::FsTools,
    config: crate::config::AppConfig,
}

impl HistoryManager {
    pub fn new(
        client: OpenAIClient,
        model: String,
        messages: Vec<ChatMessage>,
        auto_compact_threshold: u32,
        context_window_size: u32,
        ui_tx: Option<std::sync::mpsc::Sender<String>>,
        fs_tools: crate::tools::FsTools,
        config: crate::config::AppConfig,
    ) -> Self {
        Self {
            messages,
            client,
            model,
            auto_compact_threshold,
            context_window_size,
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

    /// Check if proactive compaction is needed and perform it if so
    pub async fn check_and_compact_proactive(&mut self) -> Result<bool> {
        let last_prompt_tokens = self.client.get_prompt_tokens_used();
        let safety_limit = (self.context_window_size as f64 * 0.9) as u32;
        let effective_limit = std::cmp::min(self.auto_compact_threshold, safety_limit);

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
            model: self.model.clone(),
            fs_tools: self.fs_tools.clone(),
            history: self.messages.clone(),
            cfg: self.config.clone(),
        };

        match compact_conversation_history(params).await {
            Ok(compact_result) => {
                if compact_result.metadata.success {
                    info!("History compaction successful.");

                    // Preserve System Prompt if present
                    let system_prompt = self.messages.iter().find(|m| m.role == "system").cloned();
                    self.messages.clear();
                    if let Some(sys) = system_prompt {
                        self.messages.push(sys);
                    }
                    self.messages.push(compact_result.compacted_message);

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
}
