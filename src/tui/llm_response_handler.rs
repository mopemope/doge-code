use crate::tui::state::TuiApp; // import TuiApp
use regex::Regex;
use tracing::debug; // import tracing

// Implement LLM response handling logic for TuiApp
impl TuiApp {
    // New: structured handling for LLM streaming tokens (immediate log addition)
    pub fn append_stream_token_structured(&mut self, s: &str) {
        // Sanitize incoming token to avoid terminal-control sequences that can break raw mode
        fn sanitize_for_display(input: &str) -> String {
            // Remove common ANSI CSI sequences
            let csi_re = Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").unwrap();
            // Remove OSC sequences: ESC ] ... BEL or ESC \
            let osc_re = Regex::new(r"\x1b\].*?(?:\x07|\x1b\\)").unwrap();

            let mut s = csi_re.replace_all(input, "").to_string();
            s = osc_re.replace_all(&s, "").to_string();

            // Remove other control chars except newline and tab
            s.chars()
                .filter(|&c| !c.is_control() || c == '\n' || c == '\t')
                .collect()
        }

        let clean = sanitize_for_display(s);

        // Append received (sanitized) token to the parsing buffer for final processing
        self.llm_parsing_buffer.push_str(&clean);
        debug!(appended_content = %clean, "Appended token to llm_parsing_buffer");

        // Accumulate content in last_llm_response_content for duplicate checking
        let accumulated_content = match &self.last_llm_response_content {
            Some(existing) => format!("{}{}", existing, clean),
            None => clean.clone(),
        };
        self.last_llm_response_content = Some(accumulated_content);

        // For immediate display during streaming, add the sanitized token with margin
        // This will be replaced by structured content when streaming completes
        if !clean.trim().is_empty() {
            for line in clean.lines() {
                let line_with_margin = format!("  {}", line); // 2-space margin for streaming content
                self.push_log(line_with_margin);
            }
        }
    }

    // Keep existing method (with left margin) temporarily; replaced by the new implementation
    #[allow(dead_code)]
    pub fn append_stream_token(&mut self, s: &str) {
        // Clear the last LLM response content as streaming has started/resumed
        self.last_llm_response_content = None;
        // Append streaming token with left margin
        self.append_stream_token_with_margin(s, 2); // 2-space margin
    }

    // Internal helper to append tokens with a left margin (deprecated - use push_log instead)
    #[allow(dead_code)]
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

    // Finalize LLM response (simplified - content already added during streaming)
    pub fn finalize_and_append_llm_response(&mut self, content: &str) {
        debug!("finalize_and_append_llm_response called");

        // Check if LLM response is already being displayed to prevent duplicates
        if self.is_llm_response_active {
            debug!(
                "LLM response is already active, skipping duplicate finalize_and_append_llm_response call."
            );
            return;
        }
        self.is_llm_response_active = true;
        debug!("Set is_llm_response_active to true");

        // Clear the parsing buffer as streaming is complete
        self.llm_parsing_buffer.clear();

        // Check if content is already displayed (duplicate check)
        let should_add_content = match &self.last_llm_response_content {
            Some(existing) => {
                // If the existing content is not the same as the new content, add it
                // This handles both streaming and non-streaming cases
                existing != content
            }
            None => {
                // If there's no existing content, add the new content
                !content.is_empty()
            }
        };

        if should_add_content {
            debug!(provided_content = %content, "Adding content");

            // Parse content for code blocks and add with proper formatting
            let re = Regex::new(r"(?s)```(\w*)\n(.*?)```").unwrap();
            let mut last_end = 0;
            let captures: Vec<_> = re.captures_iter(content).collect();

            if !captures.is_empty() {
                for cap in captures {
                    debug!("Found codeblock cap: {:?}", cap);
                    let whole_match = cap.get(0).unwrap();
                    let lang = cap.get(1).map_or("", |m| m.as_str());
                    let code_content = cap.get(2).map_or("", |m| m.as_str());

                    // Add text before the code block
                    if whole_match.start() > last_end {
                        let text_content = &content[last_end..whole_match.start()];
                        if !text_content.is_empty() {
                            for line in text_content.lines() {
                                let line_with_margin = format!("  {}", line);
                                self.push_log(line_with_margin);
                            }
                        }
                    }

                    // Add the code block
                    self.push_log(format!("  ```{}", lang));
                    for line in code_content.lines() {
                        let line_with_margin = format!("    {}", line);
                        self.push_log(line_with_margin);
                    }
                    self.push_log("  ```".to_string());

                    last_end = whole_match.end();
                }

                // Add any text after the last code block
                if last_end < content.len() {
                    let text_content = &content[last_end..];
                    if !text_content.is_empty() {
                        for line in text_content.lines() {
                            let line_with_margin = format!("  {}", line);
                            self.push_log(line_with_margin);
                        }
                    }
                }
            } else {
                // If no code blocks, treat as plain text
                for line in content.lines() {
                    let line_with_margin = format!("  {}", line);
                    debug!("line_with_margin: {}", line_with_margin);
                    self.push_log(line_with_margin);
                }
            }

            // Update last_llm_response_content to the new content
            self.last_llm_response_content = Some(content.to_string());
        } else {
            debug!("Skipping content addition due to duplicate check");
        }

        // Reset the flag after the response has been fully added
        self.is_llm_response_active = false;
        debug!("Set is_llm_response_active to false");
    }
}
