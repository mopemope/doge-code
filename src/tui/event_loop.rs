use anyhow::Result;
use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyModifiers},
    widgets::{Block, Borders},
};
use tui_textarea::{Input, TextArea};

use std::time::{Duration, Instant};
use tokio::process::Command;
use tracing::debug;

use crate::tui::state::{InputMode, Status, TuiApp, save_input_history};

impl TuiApp {
    pub fn event_loop(
        &mut self,
        terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    ) -> Result<()> {
        let mut last_ctrl_c_at: Option<Instant> = None;

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
                            debug!(target: "tui", status_done_content = %content, "Received ::status:done: message. Content is empty: {}", content.is_empty());
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
                                debug!(target: "tui", filtered_status_msg = %msg, "Filtered out status message from log display");
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
                                debug!(target: "tui", "Skipping duplicate LLM response message: {}", msg);
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
                        match k {
                            event::KeyEvent {
                                code: KeyCode::Enter,
                                modifiers: KeyModifiers::ALT,
                                ..
                            } => {
                                self.textarea.insert_newline();
                                self.dirty = true;
                            }
                            // Submit message
                            event::KeyEvent {
                                code: KeyCode::Enter,
                                ..
                            } => {
                                let line = self.textarea.lines().join("\n");
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
                                // Clear the textarea by reinitializing it
                                self.textarea = TextArea::default();
                                self.textarea.set_block(
                                    Block::default().borders(Borders::ALL).title("Input"),
                                );
                                self.textarea.set_placeholder_text("Enter your message...");
                                self.dirty = true;
                                self.spinner_state = 0;
                            }
                            // Other global controls
                            event::KeyEvent {
                                code: KeyCode::Char('!'),
                                ..
                            } if self.textarea.is_empty() => {
                                self.input_mode = InputMode::Shell;
                                self.dirty = true;
                            }
                            event::KeyEvent {
                                code: KeyCode::Esc, ..
                            } => {
                                self.dispatch("/cancel");
                                self.dirty = true;
                            }
                            event::KeyEvent {
                                code: KeyCode::PageUp,
                                ..
                            } => {
                                let visible_lines = terminal
                                    .size()
                                    .map(|s| s.height.saturating_sub(3) as usize)
                                    .unwrap_or(20);
                                self.page_up(visible_lines);
                            }
                            event::KeyEvent {
                                code: KeyCode::PageDown,
                                ..
                            } => {
                                let visible_lines = terminal
                                    .size()
                                    .map(|s| s.height.saturating_sub(3) as usize)
                                    .unwrap_or(20);
                                self.page_down(visible_lines);
                            }
                            event::KeyEvent {
                                code: KeyCode::Home,
                                modifiers: KeyModifiers::CONTROL,
                                ..
                            } => {
                                self.scroll_to_top();
                            }
                            event::KeyEvent {
                                code: KeyCode::End,
                                modifiers: KeyModifiers::CONTROL,
                                ..
                            } => {
                                self.scroll_to_bottom();
                            }
                            event::KeyEvent {
                                code: KeyCode::Up,
                                modifiers: KeyModifiers::CONTROL,
                                ..
                            } => {
                                self.scroll_up(1);
                            }
                            event::KeyEvent {
                                code: KeyCode::Down,
                                modifiers: KeyModifiers::CONTROL,
                                ..
                            } => {
                                self.scroll_down(1);
                            }
                            event::KeyEvent {
                                code: KeyCode::Char('l'),
                                modifiers: KeyModifiers::CONTROL,
                                ..
                            } => {
                                self.scroll_to_bottom();
                            }
                            // Input history navigation
                            event::KeyEvent {
                                code: KeyCode::Up,
                                modifiers: KeyModifiers::NONE,
                                ..
                            } => {
                                if !self.input_history.is_empty() && self.history_index > 0 {
                                    // Save current draft if we're at the end of history
                                    if self.history_index == self.input_history.len() {
                                        self.draft = self.textarea.lines().join("\n");
                                    }
                                    self.history_index -= 1;
                                    self.textarea = TextArea::from(
                                        self.input_history[self.history_index].lines(),
                                    );
                                    self.textarea.set_block(
                                        Block::default().borders(Borders::ALL).title("Input"),
                                    );
                                    self.textarea.set_placeholder_text("Enter your message...");
                                    self.dirty = true;
                                }
                            }
                            event::KeyEvent {
                                code: KeyCode::Down,
                                modifiers: KeyModifiers::NONE,
                                ..
                            } => {
                                if !self.input_history.is_empty()
                                    && self.history_index < self.input_history.len()
                                {
                                    self.history_index += 1;
                                    if self.history_index == self.input_history.len() {
                                        // Restore draft
                                        self.textarea = TextArea::from(self.draft.lines());
                                    } else {
                                        self.textarea = TextArea::from(
                                            self.input_history[self.history_index].lines(),
                                        );
                                    }
                                    self.textarea.set_block(
                                        Block::default().borders(Borders::ALL).title("Input"),
                                    );
                                    self.textarea.set_placeholder_text("Enter your message...");
                                    self.dirty = true;
                                }
                            }
                            // Pass all other key events to the text area
                            _ => {
                                match k.code {
                                    KeyCode::Left => {
                                        self.textarea.move_cursor(tui_textarea::CursorMove::Back);
                                        self.dirty = true;
                                    }
                                    KeyCode::Right => {
                                        self.textarea
                                            .move_cursor(tui_textarea::CursorMove::Forward);
                                        self.dirty = true;
                                    }
                                    KeyCode::Home => {
                                        self.textarea.move_cursor(tui_textarea::CursorMove::Head);
                                        self.dirty = true;
                                    }
                                    KeyCode::End => {
                                        self.textarea.move_cursor(tui_textarea::CursorMove::End);
                                        self.dirty = true;
                                    }
                                    KeyCode::PageUp => {
                                        // Only use PageUp for text area if it's a multi-line input
                                        if self.textarea.lines().len() > 1 {
                                            self.textarea.scroll((10, 0)); // Scroll up by 10 lines
                                            self.dirty = true;
                                        }
                                    }
                                    KeyCode::PageDown => {
                                        // Only use PageDown for text area if it's a multi-line input
                                        if self.textarea.lines().len() > 1 {
                                            self.textarea.scroll((-10, 0)); // Scroll down by 10 lines
                                            self.dirty = true;
                                        }
                                    }
                                    _ => {
                                        if self.textarea.input(Input::from(k)) {
                                            self.dirty = true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    InputMode::Shell => match k.code {
                        KeyCode::Esc => {
                            self.input_mode = InputMode::Normal;
                            self.textarea.delete_line_by_head();
                            self.textarea.delete_line_by_end();
                            self.dirty = true;
                        }
                        KeyCode::Enter => {
                            let command = self.textarea.lines().join("\n");
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
                                    let command_with_redirect = format!("{} 2>&1", command);
                                    let output = Command::new("bash")
                                        .arg("-c")
                                        .arg(&command_with_redirect)
                                        .output()
                                        .await;
                                    tx.send("::status:done".to_string()).ok();

                                    match output {
                                        Ok(output) => {
                                            if !output.stdout.is_empty() {
                                                let output_str =
                                                    String::from_utf8_lossy(&output.stdout)
                                                        .to_string();
                                                tx.send(format!("::shell_output:{}", output_str))
                                                    .ok();
                                            }
                                        }
                                        Err(e) => {
                                            tx.send(format!(
                                                "::shell_output:Failed to execute command: {}",
                                                e
                                            ))
                                            .ok();
                                        }
                                    }
                                });
                            }
                            // Clear the textarea by reinitializing it
                            self.textarea = TextArea::default();
                            self.textarea
                                .set_block(Block::default().borders(Borders::ALL).title("Input"));
                            self.textarea.set_placeholder_text("Enter your message...");
                            self.dirty = true;
                        }
                        _ => {
                            if self.textarea.input(Input::from(k)) {
                                self.dirty = true;
                            }
                        }
                    },
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
