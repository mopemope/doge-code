use crate::tui::state::{LlmResponseSegment, TuiApp}; // import TuiApp and LlmResponseSegment
use regex::Regex;

// Implement LLM response handling logic for TuiApp
impl TuiApp {
    // New: structured handling for LLM streaming tokens (simple version)
    // Tokens are accumulated in a buffer and parsed in bulk when the stream completes.
    // #[allow(dead_code)]
    pub fn append_stream_token_structured(&mut self, s: &str) {
        // Append received token to the parsing buffer
        self.llm_parsing_buffer.push_str(s);
    }

    // Keep existing method (with left margin) temporarily; replaced by the new implementation
    #[allow(dead_code)]
    pub fn append_stream_token(&mut self, s: &str) {
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

        // The first part is appended to the existing last line
        if let Some(last) = self.log.last_mut() {
            last.push_str(parts[0]);
        }

        // Parts from the second onward are added as new lines (with margin)
        for seg in parts.iter().skip(1) {
            // Adjust condition if you want to skip empty lines; here we keep margin even for empty lines
            let line_with_margin = format!("{margin_str}{seg}");
            self.log.push(line_with_margin);
        }
    }

    // New: parse llm_parsing_buffer into structured segments and append to self.log
    pub(crate) fn finalize_and_append_llm_response(&mut self) {
        // Consume the parsing buffer
        let buffer = std::mem::take(&mut self.llm_parsing_buffer);
        if buffer.is_empty() {
            // If buffer is empty, also clear current_llm_response and exit
            self.current_llm_response = None;
            return;
        }

        // Take ownership of current_llm_response and build a list of segments to process
        // This releases the borrow on self.current_llm_response immediately
        let mut response_segments = self.current_llm_response.take().unwrap_or_default();

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

        // At this point, response_segments contains all structured segments
        // current_llm_response is None (taken above), so mutable borrows of other parts of self are safe

        // Convert response_segments (Vec<LlmResponseSegment>) into flat String lines and append to self.log
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

        // current_llm_response remains None after processing (taken earlier)
    }
}
