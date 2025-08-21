use crate::tui::state::{LlmResponseSegment, TuiApp}; // import TuiApp and LlmResponseSegment
use regex::Regex;
use tracing::debug; // tracingをインポート

// Implement LLM response handling logic for TuiApp
impl TuiApp {
    // New: structured handling for LLM streaming tokens (simple version)
    // Tokens are accumulated in a buffer and parsed in bulk when the stream completes.
    // #[allow(dead_code)]
    pub fn append_stream_token_structured(&mut self, s: &str) {
        // Clear the last LLM response content as streaming has started/resumed
        self.last_llm_response_content = None;
        // Append received token to the parsing buffer
        self.llm_parsing_buffer.push_str(s);
        debug!(appended_content = %s, "Appended token to llm_parsing_buffer");
    }

    // Keep existing method (with left margin) temporarily; replaced by the new implementation
    #[allow(dead_code)]
    pub fn append_stream_token(&mut self, s: &str) {
        // Clear the last LLM response content as streaming has started/resumed
        self.last_llm_response_content = None;
        // Append streaming token with left margin
        self.append_stream_token_with_margin(s, 2); // 2-space margin
    }

    // Internal helper to append tokens with a left margin
    fn append_stream_token_with_margin(&mut self, s: &str, margin: usize) {
        let margin_str = " ".repeat(margin); // create a space string for margin

        // Normalize incoming token: split by '\n' and append as multiple logical lines if needed.
        let parts: Vec<&str> = s.split('\n').collect();
        if parts.is_empty() {
            return;
        }

        // Add the first part as a new line (with margin)
        let first_line_with_margin = format!("{margin_str}{}", parts[0]);
        self.log.push(first_line_with_margin);

        // Parts from the second onward are added as new lines (with margin)
        for seg in parts.iter().skip(1) {
            // Adjust condition if you want to skip empty lines; here we keep margin even for empty lines
            let line_with_margin = format!("{margin_str}{seg}");
            self.log.push(line_with_margin);
        }
    }

    // New: parse llm_parsing_buffer into structured segments and append to self.log
    pub(crate) fn finalize_and_append_llm_response(&mut self, content: &str) {
        // Log the last few lines of self.log before modification for debugging
        debug!("self.log before finalize_and_append_llm_response:");
        let num_lines = self.log.len();
        let start = num_lines.saturating_sub(3);
        for i in start..num_lines {
            debug!("  [{}] '{}'", i, self.log[i]);
        }

        // Check if LLM response is already being displayed to prevent duplicates
        if self.is_llm_response_active {
            debug!(
                "LLM response is already active, skipping duplicate finalize_and_append_llm_response call."
            );
            return;
        }
        self.is_llm_response_active = true;
        debug!("Set is_llm_response_active to true");

        // Consume the parsing buffer
        let buffer = std::mem::take(&mut self.llm_parsing_buffer);
        debug!(buffer_content = %buffer, "Finalizing LLM response. Buffer is empty: {}", buffer.is_empty());

        // If buffer is empty, use the provided content
        let buffer = if buffer.is_empty() {
            debug!(target: "tui", provided_content = %content, "Buffer is empty, using provided content. Content is empty: {}", content.is_empty());
            content.to_string()
        } else {
            buffer
        };

        // Convert buffer to response segments
        let mut response_segments = Vec::new();

        // Extract code blocks using regex
        // (?s) makes '.' match newlines (multi-line match)
        // non-greedy match ensures we stop at the first closing ```
        let re = Regex::new(r"(?s)```(\w*)\n(.*?)```").unwrap();
        let mut last_end = 0;

        for cap in re.captures_iter(&buffer) {
            let whole_match = cap.get(0).unwrap();
            let lang = cap.get(1).map_or("", |m| m.as_str());
            let code_content = cap.get(2).map_or("", |m| m.as_str());

            // Add text before the code block as a Text segment
            if whole_match.start() > last_end {
                let text_content = &buffer[last_end..whole_match.start()];
                if !text_content.is_empty() {
                    response_segments.push(LlmResponseSegment::Text {
                        content: text_content.to_string(),
                    });
                }
            }

            // Add the code block as a CodeBlock segment
            response_segments.push(LlmResponseSegment::CodeBlock {
                language: lang.to_string(),
                content: code_content.to_string(),
            });

            last_end = whole_match.end();
        }

        // Add any text after the last code block as a Text segment
        if last_end < buffer.len() {
            let text_content = &buffer[last_end..];
            if !text_content.is_empty() {
                response_segments.push(LlmResponseSegment::Text {
                    content: text_content.to_string(),
                });
            }
        }

        // If there's no code block and the buffer is not empty, treat the whole buffer as text
        if response_segments.is_empty() && !buffer.is_empty() {
            response_segments.push(LlmResponseSegment::Text {
                content: buffer.clone(),
            });
        }

        // Convert response_segments (Vec<LlmResponseSegment>) into flat String lines and append to self.log
        // This needs to be done after adding the start marker so that the rendering logic can detect the content
        for segment in response_segments {
            match segment {
                LlmResponseSegment::Text { content } => {
                    // Split text content by newlines and append to the log using existing margin helper
                    self.append_stream_token_with_margin(&content, 2); // apply margin to text as well
                }
                LlmResponseSegment::CodeBlock { language, content } => {
                    // Add a marker line for the start of the code block
                    self.log.push(format!(" [CodeBlockStart({language})]"));
                    // Append code content split by newlines (with wider margin)
                    self.append_stream_token_with_margin(&content, 4); // wider margin for code blocks
                    // Add a marker line for the end of the code block
                    self.log.push(" [CodeBlockEnd]".to_string());
                }
            }
        }

        // Log the last few lines of self.log after modification for debugging
        debug!("self.log after finalize_and_append_llm_response:");
        let num_lines = self.log.len();
        let start = num_lines.saturating_sub(10);
        for i in start..num_lines {
            debug!("  [{}] '{}'", i, self.log[i]);
        }

        // Store the content to prevent duplicate printing in event_loop
        self.last_llm_response_content = Some(content.to_string());

        // Reset the flag after the response has been fully added
        self.is_llm_response_active = false;
        debug!("Set is_llm_response_active to false");
    }
}
