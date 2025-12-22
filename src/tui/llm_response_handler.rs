use crate::tui::state::TuiApp; // import TuiApp
use regex::Regex;
use std::sync::OnceLock;
use tracing::debug; // import tracing

static CSI_RE: OnceLock<Regex> = OnceLock::new();
static OSC_RE: OnceLock<Regex> = OnceLock::new();

// Sanitize incoming token/content to avoid terminal-control sequences that can break raw mode.
fn sanitize_for_display(input: &str) -> String {
    // Fast path: no escape sequences, no CR, and no other control chars.
    if !input.contains('\x1b')
        && !input.contains('\r')
        && !input
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return input.to_string();
    }

    let mut s = input.to_string();

    if s.contains('\x1b') {
        let csi_re = CSI_RE
            .get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").expect("valid CSI regex"));
        let osc_re = OSC_RE
            .get_or_init(|| Regex::new(r"\x1b\].*?(?:\x07|\x1b\\)").expect("valid OSC regex"));

        // Remove common ANSI CSI sequences.
        let stripped = csi_re.replace_all(&s, "");
        // Remove OSC sequences: ESC ] ... BEL or ESC \\
        let stripped = osc_re.replace_all(&stripped, "");
        s = stripped.into_owned();
    }

    // Handle carriage returns - convert \r\n to \n and remove standalone \r
    if s.contains('\r') {
        s = s.replace("\r\n", "\n").replace('\r', "");
    }

    // Remove other control chars except newline and tab
    s.chars()
        .filter(|&c| !c.is_control() || c == '\n' || c == '\t')
        .collect()
}

// Implement LLM response handling logic for TuiApp
impl TuiApp {
    // New: structured handling for LLM streaming tokens (immediate log addition)
    pub fn append_stream_token_structured(&mut self, s: &str) {
        let clean = sanitize_for_display(s);

        // Append received (sanitized) token to the parsing buffer for final processing
        self.llm_parsing_buffer.push_str(&clean);
        debug!(appended_content = %clean, "Appended token to llm_parsing_buffer");

        if self.current_stream_start.is_none() {
            self.current_stream_start = Some(self.log.len());
        }

        // Accumulate content in last_llm_response_content for duplicate checking
        if let Some(existing) = &mut self.last_llm_response_content {
            existing.push_str(&clean);
        } else {
            self.last_llm_response_content = Some(clean.clone());
        }

        // For immediate display during streaming, add the sanitized token with margin
        // This will be replaced by structured content when streaming completes
        if !clean.trim().is_empty() {
            // Handle content with newlines by preserving the exact structure
            // But use split('\n') instead of lines() to preserve trailing newlines
            let mut parts = clean.split('\n').peekable();
            while let Some(part) = parts.next() {
                let line_with_margin = format!("  {}", part); // 2-space margin for streaming content
                self.push_log(line_with_margin);
                // Add empty line for all but the last part
                if parts.peek().is_some() {
                    self.push_log("  ".to_string()); // Empty line with margin
                }
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
        let first_line_with_margin = format!("{}{}", margin_str, parts[0]);
        self.push_log(first_line_with_margin);

        // Parts from the second onward are added as new lines (with margin)
        for seg in parts.iter().skip(1) {
            // Adjust condition if you want to skip empty lines; here we keep margin even for empty lines
            let line_with_margin = format!("{}{}", margin_str, seg);
            self.push_log(line_with_margin);
        }
    }

    // Finalize LLM response (simplified - content already added during streaming)
    pub fn finalize_and_append_llm_response(&mut self, content: &str) {
        debug!("finalize_and_append_llm_response called");

        let content = sanitize_for_display(content);

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
                existing != &content
            }
            None => {
                // If there's no existing content, add the new content
                !content.is_empty()
            }
        };

        if should_add_content {
            debug!(provided_content = %content, "Adding content");

            if let Some(start) = self.current_stream_start.take()
                && start <= self.log.len()
            {
                self.log.truncate(start);
            }

            self.push_markdown_response(&content);
            self.last_llm_response_content = Some(content);
        } else {
            debug!("Skipping content addition due to duplicate check");
            self.current_stream_start = None;
        }

        // Reset the flag after the response has been fully added
        self.is_llm_response_active = false;
        debug!("Set is_llm_response_active to false");
    }
}
