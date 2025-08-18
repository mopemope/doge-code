use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use std::time::{Duration, Instant};
use tracing::debug; // tracingをインポート

use crate::tui::state::{Status, TuiApp, save_input_history}; // import TuiApp and save_input_history

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
            // Update spinner state if status is Idle and enough time has passed
            if self.status == Status::Idle
                && last_spinner_update.elapsed() >= Duration::from_millis(200)
            {
                self.spinner_state = self.spinner_state.wrapping_add(1);
                // debug!(spinner_state = self.spinner_state, "Spinner state updated"); // デバッグログ追加
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
                    match msg.as_str() {
                        "::status:done" => {
                            if is_streaming {
                                // Removed: self.push_log(" --- LLM Response End --- ".to_string());
                                self.finalize_and_append_llm_response("");
                                is_streaming = false;
                            }
                            self.status = Status::Done;
                            dirty = true;
                        }
                        "::status:cancelled" => {
                            if is_streaming {
                                // Removed: self.push_log(" --- LLM Response End (Cancelled) --- ".to_string());
                                self.finalize_and_append_llm_response("");
                                is_streaming = false;
                            }
                            self.status = Status::Cancelled;
                            dirty = true;
                        }
                        "::status:streaming" => {
                            if !is_streaming {
                                // Removed: self.push_log(" --- LLM Response Start --- ".to_string());
                                self.current_llm_response = Some(Vec::new());
                                self.llm_parsing_buffer.clear();
                                is_streaming = true;
                            }
                            self.status = Status::Streaming;
                            dirty = true;
                            // Reset spinner state when transitioning to Streaming
                            self.spinner_state = 0;
                        }
                        "::status:error" => {
                            if is_streaming {
                                // Removed: self.push_log(" --- LLM Response End (Error) --- ".to_string());
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
                            // Check if the message is the same as the last LLM response to avoid duplication
                            if self
                                .last_llm_response_content
                                .as_ref()
                                .is_some_and(|last_content| msg == *last_content)
                            {
                                debug!(target: "tui", "Skipping duplicate LLM response message: {}", msg);
                                // Clear the stored content after matching to allow future messages
                                self.last_llm_response_content = None;
                            } else {
                                self.push_log(msg);
                            }
                            dirty = true;
                        }
                    }
                }
            }

            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(k) => match k.code {
                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
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
                            // reset cursor to start of new empty input
                            self.cursor = 0;
                            // Reset spinner state when a new command is issued
                            self.spinner_state = 0;
                        }
                        KeyCode::Backspace => {
                            if self.compl.visible {
                                // if completion visible, backspace should close it and also backspace input
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
                            dirty = true;
                        }
                        KeyCode::Delete => {
                            let changed = self.delete_at_cursor();
                            if changed && self.history_index == self.input_history.len() {
                                self.draft = self.input.clone();
                            }
                            self.update_completion();
                            dirty = true;
                        }
                        KeyCode::Left => {
                            if self.cursor > 0 {
                                self.cursor -= 1;
                                dirty = true;
                            }
                        }
                        KeyCode::Right => {
                            if self.cursor < self.input.chars().count() {
                                self.cursor += 1;
                                dirty = true;
                            }
                        }
                        KeyCode::Home => {
                            self.cursor = 0;
                            dirty = true;
                        }
                        KeyCode::End => {
                            self.cursor = self.input.chars().count();
                            dirty = true;
                        }
                        KeyCode::Up => {
                            if self.compl.visible {
                                if !self.compl.items.is_empty() {
                                    self.compl.selected =
                                        (self.compl.selected + 1) % self.compl.items.len();
                                    dirty = true;
                                }
                            } else if self.history_index > 0 {
                                if self.history_index == self.input_history.len() {
                                    self.draft = self.input.clone();
                                }
                                self.history_index -= 1;
                                self.input = self.input_history[self.history_index].clone();
                                // reset cursor to end of loaded history line
                                self.cursor = self.input.chars().count();
                                dirty = true;
                            }
                        }
                        KeyCode::Down => {
                            if self.compl.visible {
                                if !self.compl.items.is_empty() {
                                    if self.compl.selected == 0 {
                                        self.compl.selected = self.compl.items.len() - 1;
                                    } else {
                                        self.compl.selected -= 1;
                                    }
                                    dirty = true;
                                }
                            } else if self.history_index < self.input_history.len() {
                                self.history_index += 1;
                                if self.history_index == self.input_history.len() {
                                    self.input = self.draft.clone();
                                } else {
                                    self.input = self.input_history[self.history_index].clone();
                                }
                                self.cursor = self.input.chars().count();
                                dirty = true;
                            }
                        }
                        KeyCode::Char(c) => {
                            if c == ' ' && self.compl.visible {
                                self.compl.reset();
                                self.compl.suppress_once = true;
                                self.insert_at_cursor(&c.to_string());
                                if self.history_index == self.input_history.len() {
                                    self.draft = self.input.clone();
                                }
                                dirty = true;
                                continue;
                            }
                            self.insert_at_cursor(&c.to_string());
                            if c == '@' {
                                self.compl.suppress_once = false;
                            }
                            if self.history_index == self.input_history.len() {
                                self.draft = self.input.clone();
                            }
                            self.update_completion();
                            dirty = true;
                        }
                        _ => {}
                    },
                    Event::Resize(_, _) => {
                        dirty = true;
                    }
                    _ => {}
                }
            }

            if dirty {
                self.draw_with_model(self.model.as_deref())?;
                dirty = false;
            }
        }
    }
}
