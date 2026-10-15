use crate::tui::state::TuiApp; // import TuiApp
use tracing::debug;

impl TuiApp {
    /// Start a provider response, retaining any interrupted partial response.
    pub(crate) fn begin_llm_response(&mut self) {
        self.finalize_and_append_llm_response("");
        self.last_llm_response_content = None;
    }

    /// Replace exactly one entry, never truncating warnings or tool messages
    /// that may have arrived while the provider was streaming.
    fn replace_response_entry(&mut self, index: usize, content: String) {
        let entry = crate::tui::state::LogEntry::Markdown(crate::features::safe_display::text(
            &content,
            self.cfg.as_ref().and_then(|cfg| cfg.api_key.as_deref()),
        ));
        let height = if self.log_width > 0 {
            self.calculate_entry_height(&entry, self.log_width)
        } else {
            1
        };
        let previous = self.log_heights[index];
        self.log[index] = entry;
        self.log_heights[index] = height;
        if !self.scroll_state.auto_scroll {
            self.scroll_state.offset = self
                .scroll_state
                .offset
                .saturating_add(height)
                .saturating_sub(previous);
        }
        self.dirty = true;
    }

    pub fn append_stream_token_structured(&mut self, s: &str) {
        let clean = s.to_string();
        if clean.is_empty() {
            return;
        }
        if self.current_stream_start.is_none() {
            self.last_llm_response_content = None;
        }
        self.llm_parsing_buffer.push_str(&clean);
        let size = crate::llm::telemetry::text_size(&clean);
        debug!(
            appended_chars = size.chars,
            appended_bytes = size.bytes,
            "received response fragment"
        );
        let content = self.llm_parsing_buffer.clone();
        if let Some(index) = self.current_stream_start {
            self.replace_response_entry(index, content);
        } else if !crate::features::safe_display::text(
            &content,
            self.cfg.as_ref().and_then(|cfg| cfg.api_key.as_deref()),
        )
        .trim()
        .is_empty()
        {
            self.push_markdown_response(&content);
            self.current_stream_start = self.log.len().checked_sub(1);
        }
    }

    /// Route ordinary inbox text without duplicating a completed answer.
    pub(crate) fn append_inbox_message(&mut self, message: &str) -> bool {
        let message = crate::features::safe_display::text(
            message,
            self.cfg.as_ref().and_then(|cfg| cfg.api_key.as_deref()),
        );
        if self.current_stream_start.is_some()
            && crate::features::safe_display::text(
                &self.llm_parsing_buffer,
                self.cfg.as_ref().and_then(|cfg| cfg.api_key.as_deref()),
            ) == message
        {
            self.finalize_and_append_llm_response(&message);
            return true;
        }
        if self.last_llm_response_content.as_deref() != Some(&message) {
            self.push_log(message);
        }
        false
    }

    /// Empty terminal content preserves the buffered partial answer on error,
    /// cancellation or EOF. An explicit final answer replaces only its block.
    pub fn finalize_and_append_llm_response(&mut self, content: &str) {
        let content = crate::features::safe_display::text(
            content,
            self.cfg.as_ref().and_then(|cfg| cfg.api_key.as_deref()),
        );
        if let Some(index) = self.current_stream_start.take() {
            let final_content = if content.is_empty() {
                std::mem::take(&mut self.llm_parsing_buffer)
            } else {
                content
            };
            let final_content = crate::features::safe_display::text(
                &final_content,
                self.cfg.as_ref().and_then(|cfg| cfg.api_key.as_deref()),
            );
            self.replace_response_entry(index, final_content.clone());
            self.last_llm_response_content = Some(final_content);
        } else if !content.trim().is_empty()
            && self.last_llm_response_content.as_deref() != Some(&content)
        {
            self.push_markdown_response(&content);
            self.last_llm_response_content = Some(content);
        }
        self.llm_parsing_buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> TuiApp {
        TuiApp::new("test", None, "default").expect("test app")
    }

    #[test]
    fn tui_telemetry_hides_secret_but_buffer_keeps_content() {
        let secret = "SECRET_MODEL_OUTPUT_cd34";
        let mut app = test_app();
        app.append_stream_token_structured(secret);
        // UI buffer preserves content for display.
        assert!(app.llm_parsing_buffer.contains(secret));
        assert_eq!(
            app.log.last(),
            Some(&crate::tui::state::LogEntry::Markdown(secret.into()))
        );
        // Telemetry metadata carries only sizes.
        let size = crate::llm::telemetry::text_size(secret);
        let rendered = format!("{size:?}");
        assert!(!rendered.contains(secret));
        assert_eq!(size.chars, secret.chars().count());
        assert_eq!(size.bytes, secret.len());
    }
}
