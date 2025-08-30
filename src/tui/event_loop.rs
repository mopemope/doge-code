use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyModifiers};

use std::time::{Duration, Instant};
use tracing::debug;

use crate::tui::event_handlers::{handle_normal_mode_key, handle_shell_mode_key};
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
            // Process instruction queue if idle
            let is_idle = matches!(
                self.status,
                Status::Idle | Status::Done | Status::Cancelled | Status::Error
            );
            if is_idle && let Some(instruction) = self.pending_instructions.pop_front() {
                self.dispatch(&instruction);
            }

            // Update spinner state for active statuses and enough time has passed
            let should_update_spinner = matches!(
                self.status,
                Status::Preparing
                    | Status::Sending
                    | Status::Waiting
                    | Status::Streaming
                    | Status::Processing
            );

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
                for msg in drained {
                    // Shell command outputs
                    if let Some(output) = msg.strip_prefix("::shell_output:") {
                        for line in output.lines() {
                            self.push_log(line.to_string());
                        }
                        self.dirty = true;
                        continue;
                    }

                    match msg.as_str() {
                        "::status:done" => {
                            if is_streaming {
                                self.finalize_and_append_llm_response("");
                                is_streaming = false;
                            }
                            self.status = Status::Done;
                            self.dirty = true;
                        }
                        "::status:cancelled" => {
                            if is_streaming {
                                self.finalize_and_append_llm_response("");
                                is_streaming = false;
                            }
                            self.status = Status::Cancelled;
                            self.dirty = true;
                        }
                        "::status:preparing" => {
                            self.status = Status::Preparing;
                            self.dirty = true;
                            self.spinner_state = 0;
                        }
                        "::status:sending" => {
                            self.status = Status::Sending;
                            self.dirty = true;
                            self.spinner_state = 0;
                        }
                        "::status:waiting" => {
                            self.status = Status::Waiting;
                            self.dirty = true;
                            self.spinner_state = 0;
                        }
                        "::status:streaming" => {
                            if !is_streaming {
                                self.llm_parsing_buffer.clear();
                                is_streaming = true;
                            }
                            self.status = Status::Streaming;
                            self.dirty = true;
                            self.spinner_state = 0;
                        }
                        "::status:processing" => {
                            self.status = Status::Processing;
                            self.dirty = true;
                            self.spinner_state = 0;
                        }
                        "::status:shell_running" => {
                            self.status = Status::ShellCommandRunning;
                            self.dirty = true;
                            self.spinner_state = 0;
                        }
                        "::status:error" => {
                            if is_streaming {
                                self.finalize_and_append_llm_response("");
                                is_streaming = false;
                            }
                            self.status = Status::Error;
                            self.dirty = true;
                        }

                        _ if msg.starts_with("::append:") => {
                            let payload = &msg["::append:".len()..];
                            self.append_stream_token_structured(payload);
                            self.dirty = true;
                        }
                        _ if msg.starts_with("::status:done:") => {
                            let content = &msg["::status:done:".len()..];
                            debug!(
                                "Received ::status:done: message. Content is empty: {}",
                                content.is_empty()
                            );
                            self.finalize_and_append_llm_response(content);
                            is_streaming = false;
                            self.status = Status::Done;
                            self.dirty = true;
                        }
                        _ if msg.starts_with("::tokens:") => {
                            let tokens_str = &msg["::tokens:".len()..];
                            if let Ok(tokens) = tokens_str.parse::<u32>() {
                                self.tokens_used = tokens;
                                self.dirty = true;
                            }
                        }
                        _ if msg.starts_with("::status:error:") => {
                            let content = &msg["::status:error:".len()..];
                            self.finalize_and_append_llm_response(content);
                            is_streaming = false;
                            self.status = Status::Error;
                            self.dirty = true;
                        }
                        _ => {
                            if msg.starts_with("::status:") {
                                debug!("Filtered out status message from log display");
                                continue;
                            }

                            // Handle token updates
                            if let Some(tokens_str) = msg.strip_prefix("::tokens:") {
                                if let Ok(tokens) = tokens_str.parse::<u32>() {
                                    self.tokens_used = tokens;
                                    self.dirty = true;
                                }
                                continue;
                            }

                            if self
                                .last_llm_response_content
                                .as_ref()
                                .is_some_and(|last_content| msg == *last_content)
                            {
                                debug!("Skipping duplicate LLM response message: {}", msg);
                                self.last_llm_response_content = None;
                            } else {
                                self.push_log(msg);
                            }
                            self.dirty = true;
                        }
                    }
                }
            }

            if event::poll(Duration::from_millis(50))?
                && let Event::Key(k) = event::read()?
            {
                // Global key handlers
                if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
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

                // Mode-specific key handlers
                match self.input_mode {
                    InputMode::Normal => {
                        if handle_normal_mode_key(self, k, terminal)? {
                            return Ok(());
                        }
                    }
                    InputMode::Shell => {
                        handle_shell_mode_key(self, k)?;
                    }
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
