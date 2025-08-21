use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use std::time::{Duration, Instant};
use tokio::process::Command;
use tracing::debug;

use crate::tui::state::{InputMode, Status, TuiApp, save_input_history};

impl TuiApp {
    pub fn event_loop(&mut self) -> Result<()> {
        if self.history_index > self.input_history.len() {
            self.history_index = self.input_history.len();
        }
        let mut last_ctrl_c_at: Option<Instant> = None;
        let mut dirty = true; // initial full render
        let mut is_streaming = false; // track streaming state
        let mut last_spinner_update = Instant::now(); // Track last spinner update time
        loop {
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
                dirty = true;
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
                    if let Some(output) = msg.strip_prefix("::shell_stdout:") {
                        for line in output.lines() {
                            self.push_log(format!("[stdout] {}", line));
                        }
                        dirty = true;
                        continue;
                    }
                    if let Some(output) = msg.strip_prefix("::shell_stderr:") {
                        for line in output.lines() {
                            self.push_log(format!("[stderr] {}", line));
                        }
                        dirty = true;
                        continue;
                    }

                    match msg.as_str() {
                        "::status:done" => {
                            if is_streaming {
                                self.finalize_and_append_llm_response("");
                                is_streaming = false;
                            }
                            self.status = Status::Done;
                            dirty = true;
                        }
                        "::status:cancelled" => {
                            if is_streaming {
                                self.finalize_and_append_llm_response("");
                                is_streaming = false;
                            }
                            self.status = Status::Cancelled;
                            dirty = true;
                        }
                        "::status:preparing" => {
                            self.status = Status::Preparing;
                            dirty = true;
                            self.spinner_state = 0;
                        }
                        "::status:sending" => {
                            self.status = Status::Sending;
                            dirty = true;
                            self.spinner_state = 0;
                        }
                        "::status:waiting" => {
                            self.status = Status::Waiting;
                            dirty = true;
                            self.spinner_state = 0;
                        }
                        "::status:streaming" => {
                            if !is_streaming {
                                self.current_llm_response = Some(Vec::new());
                                self.llm_parsing_buffer.clear();
                                is_streaming = true;
                            }
                            self.status = Status::Streaming;
                            dirty = true;
                            self.spinner_state = 0;
                        }
                        "::status:processing" => {
                            self.status = Status::Processing;
                            dirty = true;
                            self.spinner_state = 0;
                        }
                        "::status:shell_running" => {
                            self.status = Status::ShellCommandRunning;
                            dirty = true;
                            self.spinner_state = 0;
                        }
                        "::status:error" => {
                            if is_streaming {
                                self.finalize_and_append_llm_response("");
                                is_streaming = false;
                            }
                            self.status = Status::Error;
                            dirty = true;
                        }

                        _ if msg.starts_with("::append:") => {
                            let payload = &msg["::append:".len()..];
                            self.append_stream_token_structured(payload);
                            dirty = true;
                        }
                        _ if msg.starts_with("::status:done:") => {
                            let content = &msg["::status:done:".len()..];
                            debug!(target: "tui", status_done_content = %content, "Received ::status:done: message. Content is empty: {}", content.is_empty());
                            self.finalize_and_append_llm_response(content);
                            is_streaming = false;
                            self.status = Status::Done;
                            dirty = true;
                        }
                        _ if msg.starts_with("::status:error:") => {
                            let content = &msg["::status:error:".len()..];
                            self.finalize_and_append_llm_response(content);
                            is_streaming = false;
                            self.status = Status::Error;
                            dirty = true;
                        }
                        _ => {
                            if msg.starts_with("::status:") {
                                debug!(target: "tui", filtered_status_msg = %msg, "Filtered out status message from log display");
                                continue;
                            }

                            if self
                                .last_llm_response_content
                                .as_ref()
                                .is_some_and(|last_content| msg == *last_content)
                            {
                                debug!(target: "tui", "Skipping duplicate LLM response message: {}", msg);
                                self.last_llm_response_content = None;
                            } else {
                                self.push_log(msg);
                            }
                            dirty = true;
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
                    dirty = true;
                    continue;
                }

                // Mode-specific key handlers
                match self.input_mode {
                    InputMode::Normal => match k.code {
                        KeyCode::Char('!') if self.input.is_empty() => {
                            self.input_mode = InputMode::Shell;
                            dirty = true;
                        }
                        KeyCode::Esc => {
                            self.dispatch("/cancel");
                            dirty = true;
                        }
                        KeyCode::Enter => {
                            if self.compl.visible {
                                self.apply_completion();
                                dirty = true;
                                continue;
                            }
                            let line = std::mem::take(&mut self.input);
                            if !line.trim().is_empty() {
                                if self.input_history.last().map(|s| s.as_str())
                                    != Some(line.as_str())
                                {
                                    self.input_history.push(line.clone());
                                    save_input_history(&self.input_history);
                                }
                                self.history_index = self.input_history.len();
                                self.draft.clear();
                            }
                            if line.trim() == "/quit" {
                                return Ok(());
                            }
                            self.dispatch(&line);
                            dirty = true;
                            self.cursor = 0;
                            self.spinner_state = 0;
                        }
                        _ => {
                            self.handle_common_input_keys(k);
                            dirty = true;
                        }
                    },
                    InputMode::Shell => match k.code {
                        KeyCode::Esc => {
                            self.input_mode = InputMode::Normal;
                            self.input.clear();
                            self.cursor = 0;
                            dirty = true;
                        }
                        KeyCode::Enter => {
                            let command = std::mem::take(&mut self.input);
                            if !command.trim().is_empty() {
                                self.push_log(format!("[shell]$ {}", command));
                                if self.input_history.last().map(|s| s.as_str())
                                    != Some(command.as_str())
                                {
                                    self.input_history.push(command.clone());
                                    save_input_history(&self.input_history);
                                }
                                self.history_index = self.input_history.len();
                                self.draft.clear();

                                let tx = self.inbox_tx.clone().unwrap();
                                tokio::spawn(async move {
                                    tx.send("::status:shell_running".to_string()).ok();
                                    let output =
                                        Command::new("bash").arg("-c").arg(&command).output().await;
                                    tx.send("::status:done".to_string()).ok();

                                    match output {
                                        Ok(output) => {
                                            if !output.stdout.is_empty() {
                                                let stdout =
                                                    String::from_utf8_lossy(&output.stdout)
                                                        .to_string();
                                                tx.send(format!("::shell_stdout:{}", stdout)).ok();
                                            }
                                            if !output.stderr.is_empty() {
                                                let stderr =
                                                    String::from_utf8_lossy(&output.stderr)
                                                        .to_string();
                                                tx.send(format!("::shell_stderr:{}", stderr)).ok();
                                            }
                                        }
                                        Err(e) => {
                                            tx.send(format!(
                                                "::shell_stderr:Failed to execute command: {}",
                                                e
                                            ))
                                            .ok();
                                        }
                                    }
                                });
                            }
                            self.cursor = 0;
                            dirty = true;
                        }
                        _ => {
                            self.handle_common_input_keys(k);
                            dirty = true;
                        }
                    },
                }
            }

            if dirty {
                let model = self.model.clone();
                self.draw_with_model(model.as_deref())?;
                dirty = false;
            }
        }
    }

    // Helper function for common input key handling
    fn handle_common_input_keys(&mut self, k: event::KeyEvent) {
        match k.code {
            KeyCode::Backspace => {
                if self.compl.visible {
                    let changed = self.backspace_at_cursor();
                    self.compl.reset();
                    if changed && self.history_index == self.input_history.len() {
                        self.draft = self.input.clone();
                    }
                } else {
                    let changed = self.backspace_at_cursor();
                    if changed && self.history_index == self.input_history.len() {
                        self.draft = self.input.clone();
                    }
                    self.update_completion();
                }
            }
            KeyCode::Delete => {
                let changed = self.delete_at_cursor();
                if changed && self.history_index == self.input_history.len() {
                    self.draft = self.input.clone();
                }
                self.update_completion();
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
            }
            KeyCode::Right => {
                if self.cursor < self.input.chars().count() {
                    self.cursor += 1;
                }
            }
            KeyCode::Home => {
                self.cursor = 0;
            }
            KeyCode::End => {
                self.cursor = self.input.chars().count();
            }
            KeyCode::Up => {
                if self.compl.visible {
                    if !self.compl.items.is_empty() {
                        self.compl.selected = (self.compl.selected + self.compl.items.len() - 1)
                            % self.compl.items.len();
                    }
                } else if self.history_index > 0 {
                    if self.history_index == self.input_history.len() {
                        self.draft = self.input.clone();
                    }
                    self.history_index -= 1;
                    self.input = self.input_history[self.history_index].clone();
                    self.cursor = self.input.chars().count();
                }
            }
            KeyCode::Down => {
                if self.compl.visible {
                    if !self.compl.items.is_empty() {
                        self.compl.selected = (self.compl.selected + 1) % self.compl.items.len();
                    }
                } else if self.history_index < self.input_history.len() {
                    self.history_index += 1;
                    if self.history_index == self.input_history.len() {
                        self.input = self.draft.clone();
                    } else {
                        self.input = self.input_history[self.history_index].clone();
                    }
                    self.cursor = self.input.chars().count();
                }
            }
            KeyCode::Char(c) => {
                if c == ' ' && self.compl.visible {
                    self.compl.reset();
                    self.compl.suppress_once = true;
                }
                self.insert_at_cursor(&c.to_string());
                if c == '@' {
                    self.compl.suppress_once = false;
                }
                if self.history_index == self.input_history.len() {
                    self.draft = self.input.clone();
                }
                self.update_completion();
            }
            _ => {}
        }
    }
}
