use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyModifiers};
use std::time::{Duration, Instant};
use tracing::debug;

use crate::tui::event_handlers::{
    handle_file_search_key, handle_history_search_key, handle_normal_mode_key,
};
use crate::tui::state::{InputMode, Status, TuiApp};

impl TuiApp {
    pub fn event_loop(
        &mut self,
        terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    ) -> Result<()> {
        let mut last_ctrl_c_at: Option<Instant> = None;

        let mut is_streaming = false; // track streaming state
        let mut last_spinner_update = Instant::now(); // Track last spinner update time
        loop {
            // Process instruction queue if ready
            let is_ready = matches!(self.status, Status::Ready | Status::Error);
            if is_ready && let Some(instruction) = self.pending_instructions.pop_front() {
                self.dispatch(&instruction);
                self.dirty = true;
            }

            // Update spinner state for active statuses
            let should_update_spinner = matches!(self.status, Status::Thinking | Status::Running);

            if should_update_spinner && last_spinner_update.elapsed() >= Duration::from_millis(150)
            {
                self.spinner_state = self.spinner_state.wrapping_add(1);
                self.dirty = true;
                last_spinner_update = Instant::now();
            }

            // Drain inbox; mark dirty on any state change
            if let Some(rx) = self.inbox_rx.as_ref() {
                let mut drained = Vec::new();
                while let Ok(msg) = rx.try_recv() {
                    drained.push(msg);
                }

                if !drained.is_empty() {
                    self.last_heartbeat = Some(Instant::now());
                }

                for msg in drained {
                    // Shell command outputs (bin)
                    if let Some(encoded) = msg.strip_prefix("::shell_output_bin:") {
                        use base64::{Engine as _, engine::general_purpose};
                        if let Ok(decoded) = general_purpose::STANDARD.decode(encoded) {
                            let text = String::from_utf8_lossy(&decoded);
                            self.shell_output_buffer.push_str(&text);
                            self.dirty = true;
                        }
                        continue;
                    }
                    // Legacy shell output handling
                    if let Some(output) = msg.strip_prefix("::shell_output:") {
                        for line in output.lines() {
                            self.push_log(line.to_string());
                        }
                        self.dirty = true;
                        continue;
                    }

                    if let Some(payload) = msg.strip_prefix("::file_list_loaded:") {
                        if let Ok(files) = serde_json::from_str::<Vec<String>>(payload)
                            && let Some(state) = &mut self.file_search_state
                        {
                            state.all_files = files;
                            state.loading = false;
                            self.update_file_search();
                        }
                        self.dirty = true;
                        continue;
                    }

                    // Removed DiffReview handling
                    if msg.starts_with("::diff_review:") || msg.starts_with("::diff_output:") {
                        debug!("Ignoring diff review message as feature is disabled.");
                        continue;
                    }

                    // Lint issues handling
                    if let Some(payload) = msg.strip_prefix("::lint_issues:") {
                        // Assuming payload is JSON array of issues
                        // For simplicity, just log that we got issues and dispatch similar to before
                        // but without complex struct parsing if we can avoid it, or just use regex/string manip?
                        // The original code parsed it into `LintIssue`. We need that struct definition which is in another module.
                        // I'll keep the parsing logic if possible, or simplify.
                        // The original used `crate::tui::commands::handlers::slash_commands::lint::LintIssue`.
                        // It is safe to import or refer to it.

                        if let Ok(issues) = serde_json::from_str::<
                            Vec<crate::tui::commands::handlers::slash_commands::lint::LintIssue>,
                        >(payload)
                        {
                            self.push_log(format!(
                                "[lint] Found {} issues. Sending to LLM for analysis...",
                                issues.len()
                            ));
                            self.dirty = true;

                            let mut prompt = String::from(
                                "Please analyze and fix the following lint issues in the codebase:\n\n",
                            );

                            for (i, issue) in issues.iter().enumerate() {
                                prompt.push_str(&format!("Issue {}: {}\n", i + 1, issue.message));
                                if !issue.file_path.is_empty() {
                                    prompt.push_str(&format!("File: {}\n", issue.file_path));
                                }
                                if let Some(line) = issue.line_number {
                                    prompt.push_str(&format!("Line: {}\n", line));
                                }
                                prompt.push_str(&format!("Severity: {}\n", issue.severity));
                                if let Some(code) = &issue.code {
                                    prompt.push_str(&format!("Code: {}\n", code));
                                }
                                prompt.push('\n');
                            }
                            prompt.push_str("Please provide specific code fixes for each issue.");

                            self.push_log(format!("> {}", prompt));
                            self.last_user_input = Some(prompt.clone());
                            self.dispatch(&prompt);
                        } else {
                            self.push_log(format!(
                                "[lint][warn] Failed to parse lint issues: {}",
                                payload
                            ));
                            self.dirty = true;
                        }
                        continue;
                    }

                    if let Some(prompt) = msg.strip_prefix("::lint_command_output_analysis:") {
                        self.push_log("[lint] Sending output to LLM...");
                        self.dirty = true;
                        self.push_log(format!("> {}", prompt));
                        self.last_user_input = Some(prompt.to_string());
                        self.dispatch(prompt);
                        continue;
                    }

                    if let Some(prompt) = msg.strip_prefix("::test_failures_analysis:") {
                        self.push_log("[test] Sending failures to LLM...");
                        self.dirty = true;
                        self.push_log(format!("> {}", prompt));
                        self.last_user_input = Some(prompt.to_string());
                        self.dispatch(prompt);
                        continue;
                    }

                    // Handle status messages
                    if let Some(rest) = msg.strip_prefix("::status:") {
                        if let Some(content) = rest.strip_prefix("done:") {
                            self.finalize_and_append_llm_response(content);
                            is_streaming = false;
                            self.status = Status::Ready; // Was Done
                            self.detailed_status = None;
                            self.dirty = true;

                            if let Some(start_time) = self.processing_start_time.take() {
                                let elapsed = start_time.elapsed();
                                let elapsed_secs = elapsed.as_secs();
                                self.last_elapsed_time = Some(format!(
                                    "{:02}:{:02}:{:02}",
                                    elapsed_secs / 3600,
                                    (elapsed_secs % 3600) / 60,
                                    elapsed_secs % 60
                                ));
                            }
                            continue;
                        }

                        if let Some(content) = rest.strip_prefix("error:") {
                            self.finalize_and_append_llm_response(content);
                            is_streaming = false;
                            self.status = Status::Error;
                            self.detailed_status = None;
                            self.dirty = true;
                            self.processing_start_time = None;
                            continue;
                        }

                        if let Some(msg_body) = rest.strip_prefix("waiting:") {
                            self.status = Status::Thinking; // Was Waiting
                            self.push_log(format!("[WAIT] {}", msg_body));
                            self.dirty = true;
                            self.spinner_state = 0;
                            continue;
                        }

                        if let Some(msg_body) = rest.strip_prefix("working:") {
                            self.status = Status::Running;
                            self.detailed_status = Some(msg_body.to_string());
                            self.dirty = true;
                            self.spinner_state = 0;
                            continue;
                        }

                        if let Some(msg_body) = rest.strip_prefix("compacting:") {
                            self.status = Status::Thinking; // Was Processing
                            self.push_log(format!("[AUTO] {}", msg_body));
                            self.dirty = true;
                            self.spinner_state = 0;
                            continue;
                        }

                        if let Some(msg_body) = rest.strip_prefix("warning:") {
                            self.push_log(format!("[WARN] {}", msg_body));
                            self.dirty = true;
                            continue;
                        }

                        match rest {
                            "done" => {
                                if is_streaming {
                                    self.finalize_and_append_llm_response("");
                                    is_streaming = false;
                                }
                                self.status = Status::Ready; // Was Done
                                self.detailed_status = None;
                                self.dirty = true;
                                if let Some(start_time) = self.processing_start_time.take() {
                                    let elapsed = start_time.elapsed();
                                    let elapsed_secs = elapsed.as_secs();
                                    self.last_elapsed_time = Some(format!(
                                        "{:02}:{:02}:{:02}",
                                        elapsed_secs / 3600,
                                        (elapsed_secs % 3600) / 60,
                                        elapsed_secs % 60
                                    ));
                                }
                                continue;
                            }
                            "cancelled" => {
                                if is_streaming {
                                    self.finalize_and_append_llm_response("");
                                    is_streaming = false;
                                }
                                self.status = Status::Ready; // Was Cancelled
                                self.detailed_status = None;
                                self.dirty = true;
                                self.processing_start_time = None;
                                continue;
                            }
                            "preparing" | "sending" | "waiting" | "processing" => {
                                self.status = Status::Thinking;
                                self.dirty = true;
                                self.spinner_state = 0;
                                continue;
                            }
                            "streaming" => {
                                if !is_streaming {
                                    self.llm_parsing_buffer.clear();
                                    is_streaming = true;
                                }
                                self.status = Status::Thinking; // Streaming is also Thinking/Active
                                self.dirty = true;
                                self.spinner_state = 0;
                                continue;
                            }
                            "idle" => {
                                self.status = Status::Ready;
                                self.detailed_status = None;
                                self.dirty = true;
                                self.spinner_state = 0;
                                continue;
                            }
                            "shell_running" => {
                                self.status = Status::Running;
                                self.dirty = true;
                                self.spinner_state = 0;
                                continue;
                            }
                            "error" => {
                                if is_streaming {
                                    self.finalize_and_append_llm_response("");
                                    is_streaming = false;
                                }
                                self.status = Status::Error;
                                self.detailed_status = None;
                                self.dirty = true;
                                self.processing_start_time = None;
                                continue;
                            }
                            "repomap_building" => {
                                self.repomap_status = crate::tui::state::RepomapStatus::Building;
                                self.dirty = true;
                                continue;
                            }
                            "repomap_ready" => {
                                self.repomap_status = crate::tui::state::RepomapStatus::Ready;
                                self.dirty = true;
                                continue;
                            }
                            "repomap_error" => {
                                self.repomap_status = crate::tui::state::RepomapStatus::Error;
                                self.dirty = true;
                                continue;
                            }
                            _ => {
                                debug!("Filtered out unknown status message: {}", msg);
                                continue;
                            }
                        }
                    }

                    if let Some(payload) = msg.strip_prefix("::append:") {
                        self.append_stream_token_structured(payload);
                        self.dirty = true;
                        continue;
                    }

                    if let Some(rest) = msg.strip_prefix("::error:") {
                        let parts: Vec<&str> = rest.splitn(2, ':').collect();
                        if parts.len() == 2 {
                            self.push_log(format!("[ERROR][{}] {}", parts[0], parts[1]));
                        } else {
                            self.push_log(format!("[ERROR] {}", rest));
                        }
                        self.dirty = true;
                        continue;
                    }

                    if let Some(plan_list_json) = msg.strip_prefix("::plan_list:") {
                        #[derive(serde::Deserialize)]
                        struct PlanListPayload {
                            items: Vec<crate::tui::state::PlanItem>,
                        }
                        if let Ok(payload) = serde_json::from_str::<PlanListPayload>(plan_list_json)
                        {
                            self.apply_plan_list_update(payload.items);
                        } else if let Ok(plan_list) =
                            serde_json::from_str::<Vec<crate::tui::state::PlanItem>>(plan_list_json)
                        {
                            self.apply_plan_list_update(plan_list);
                        }
                        continue;
                    }

                    if msg == "::trigger_compact" {
                        self.push_log("[AUTO] Triggering /compact".to_string());
                        self.dispatch("/compact");
                        self.dirty = true;
                        continue;
                    }

                    if msg == "[SUCCESS] Conversation history has been compacted." {
                        self.auto_compact_pending = false;
                        self.push_log(msg);
                        self.dirty = true;
                        if let Some(last_input) = self.last_user_input.clone() {
                            self.push_log("[AUTO] Retrying last user input.".to_string());
                            self.dispatch(&last_input);
                        }
                        continue;
                    }

                    if let Some(tokens_str) = msg.strip_prefix("::tokens:") {
                        if let Ok(tokens) = tokens_str.parse::<u32>() {
                            self.tokens_used = tokens;
                            self.dirty = true;
                        }
                        continue;
                    }

                    if msg.starts_with("::update_remaining_tokens") {
                        if msg.len() > 22 {
                            if let Ok(context_size) = msg[22..].parse::<u32>() {
                                self.update_remaining_context_tokens(Some(context_size));
                            }
                        } else {
                            self.update_remaining_context_tokens(None);
                        }
                        continue;
                    }

                    if self
                        .last_llm_response_content
                        .as_ref()
                        .is_some_and(|last_content| msg == *last_content)
                    {
                        debug!("Skipping duplicate LLM response: {}", msg);
                        self.last_llm_response_content = None;
                    } else {
                        self.push_log(msg);
                    }
                    self.dirty = true;
                }
            }

            // Timeout Check
            if (matches!(self.status, Status::Thinking | Status::Running))
                && let Some(last_heartbeat) = self.last_heartbeat
                && last_heartbeat.elapsed() > Duration::from_secs(30)
            {
                // Only warn once every 30 seconds to avoid spamming
                // We can reset last_heartbeat to now to silence it for another 30 seconds,
                // essentially treating the warning itself as a heartbeat of sorts (or rather acknowledgement)
                // But better to just log a warning and maybe set a flag?
                // For simplicity, let's just log and update heartbeat so we don't spam.
                self.push_log(
                    "[WARN] No signal from agent for 30s. It might be stuck or network is slow."
                        .to_string(),
                );
                self.last_heartbeat = Some(Instant::now());
                self.dirty = true;
            }

            if event::poll(Duration::from_millis(10))? {
                let event = event::read()?;
                tracing::debug!("Event captured: {:?}", event);

                match event {
                    Event::Resize(w, h) => {
                        self.window_width = w as usize;
                        self.main_content_height = h.saturating_sub(4) as usize; // Status(1) + Input(3) = 4
                        self.log_heights.clear();
                        self.dirty = true;
                    }
                    Event::Mouse(mouse_event) => match mouse_event.kind {
                        event::MouseEventKind::ScrollUp => {
                            self.scroll_up(3);
                        }
                        event::MouseEventKind::ScrollDown => {
                            self.scroll_down(3);
                        }
                        _ => {}
                    },
                    Event::Key(k) => {
                        if k.code == KeyCode::Char('c')
                            && k.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            let now = Instant::now();
                            if let Some(prev) = last_ctrl_c_at
                                && now.duration_since(prev) <= Duration::from_secs(3)
                            {
                                return Ok(());
                            }
                            last_ctrl_c_at = Some(now);
                            self.dispatch("/cancel");
                            self.push_log("[Press Ctrl+C again within 3s to exit]");
                            self.dirty = true;
                            continue;
                        }

                        match self.input_mode {
                            InputMode::Normal => {
                                if handle_normal_mode_key(self, k, terminal)? {
                                    return Ok(());
                                }
                            }
                            InputMode::HistorySearch => {
                                handle_history_search_key(self, k)?;
                            }
                            InputMode::FileSearch => {
                                handle_file_search_key(self, k)?;
                            }
                        }
                    }
                    _ => {}
                }
            }

            if self.dirty {
                let model = self.model.clone();
                terminal.draw(|f| self.view(f, model.as_deref()))?;
                self.dirty = false;
            }
        }
    }
}
