use anyhow::{Context, Result, anyhow};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};

use std::fs;
use std::io::ErrorKind;
use std::process::Command;
use std::time::{Duration, Instant};
use tracing::debug;

use crate::diff_review::DiffReviewPayload;
use crate::tui::diff_review::DiffReviewState;
use crate::tui::event_handlers::{
    handle_normal_mode_key, handle_session_list_key, handle_shell_mode_key,
};
use crate::tui::state::{InputMode, Status, TuiApp};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct DiffReviewError {
    error: String,
}

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
                self.dirty = true;
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

                    if let Some(payload) = msg.strip_prefix("::diff_review:") {
                        if let Ok(payload) = serde_json::from_str::<DiffReviewPayload>(payload) {
                            let review_state = DiffReviewState::from_payload(payload);
                            let file_count = review_state.files.len();
                            self.diff_review = Some(review_state);
                            self.dirty = true;
                            self.push_log(format!(
                                "[diff] Ready for review: {} file(s) changed. Use a=accept, r=reject.",
                                file_count
                            ));
                        } else if let Ok(err_payload) =
                            serde_json::from_str::<DiffReviewError>(payload)
                        {
                            self.push_log(format!("[diff][error] {}", err_payload.error));
                            self.diff_review = None;
                            self.dirty = true;
                        } else {
                            self.push_log(format!(
                                "[diff][warn] Received unexpected diff payload: {}",
                                payload
                            ));
                            self.dirty = true;
                        }
                        continue;
                    }

                    if let Some(output) = msg.strip_prefix("::diff_output:") {
                        let payload = DiffReviewPayload {
                            diff: output.to_string(),
                            files: vec![],
                        };
                        let review_state = DiffReviewState::from_payload(payload);
                        self.diff_review = Some(review_state);
                        self.dirty = true;
                        self.push_log(
                            "[diff] Received legacy diff payload. Review with a=accept, r=reject."
                                .to_string(),
                        );
                        continue;
                    }

                    #[allow(unreachable_patterns)]
                    match msg.as_str() {
                        "::trigger_compact" => {
                            self.push_log(
                                "[AUTO] Triggering /compact due to context length exceeded."
                                    .to_string(),
                            );
                            self.dispatch("/compact");
                            self.dirty = true;
                        }
                        "[SUCCESS] Conversation history has been compacted." => {
                            // Only clear pending flag on the specific compact success message
                            self.auto_compact_pending = false;
                            self.push_log(msg);
                            self.dirty = true;

                            // Retry the last user input after compacting
                            if let Some(last_input) = self.last_user_input.clone() {
                                self.push_log(
                                    "[AUTO] Retrying last user input after compacting.".to_string(),
                                );
                                self.dispatch(&last_input);
                            }
                        }
                        "::status:done" => {
                            if is_streaming {
                                self.finalize_and_append_llm_response("");
                                is_streaming = false;
                            }
                            self.status = Status::Done;
                            self.dirty = true;

                            // Calculate and store final elapsed time
                            if let Some(start_time) = self.processing_start_time.take() {
                                let elapsed = start_time.elapsed();
                                let elapsed_secs = elapsed.as_secs();
                                let hours = elapsed_secs / 3600;
                                let minutes = (elapsed_secs % 3600) / 60;
                                let seconds = elapsed_secs % 60;
                                self.last_elapsed_time =
                                    Some(format!("{:02}:{:02}:{:02}", hours, minutes, seconds));
                            }
                        }
                        "::status:cancelled" => {
                            if is_streaming {
                                self.finalize_and_append_llm_response("");
                                is_streaming = false;
                            }
                            self.status = Status::Cancelled;
                            self.dirty = true;
                            // Reset processing_start_time to stop the timer
                            self.processing_start_time = None;
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
                        "::status:idle" => {
                            self.status = Status::Idle;
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
                            // Reset processing_start_time to stop the timer
                            self.processing_start_time = None;
                        }
                        "::status:repomap_building" => {
                            self.repomap_status = crate::tui::state::RepomapStatus::Building;
                            self.dirty = true;
                        }
                        "::status:repomap_ready" => {
                            self.repomap_status = crate::tui::state::RepomapStatus::Ready;
                            self.dirty = true;
                            // Optionally, you can change the status or perform other actions
                            // self.status = Status::Idle; // Example: change status to Idle
                        }
                        "::status:repomap_error" => {
                            self.repomap_status = crate::tui::state::RepomapStatus::Error;
                            self.dirty = true;
                        }

                        // Clear auto-compact pending flag when compaction succeeds
                        "[SUCCESS] Conversation history has been compacted." => {
                            // Only clear pending flag on the specific compact success message
                            self.auto_compact_pending = false;
                            self.push_log(msg);
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

                            // Calculate and store final elapsed time
                            if let Some(start_time) = self.processing_start_time.take() {
                                let elapsed = start_time.elapsed();
                                let elapsed_secs = elapsed.as_secs();
                                let hours = elapsed_secs / 3600;
                                let minutes = (elapsed_secs % 3600) / 60;
                                let seconds = elapsed_secs % 60;
                                self.last_elapsed_time =
                                    Some(format!("{:02}:{:02}:{:02}", hours, minutes, seconds));
                            }
                        }
                        _ if msg.starts_with("::tokens:") => {
                            let tokens_str = &msg["::tokens:".len()..];
                            // New format: ::tokens:prompt:{n},total:{m}
                            if let Some(rest) = tokens_str.strip_prefix("prompt:") {
                                // parse prompt and optionally total
                                let mut prompt_val: Option<u32> = None;
                                let mut total_val: Option<u32> = None;
                                for part in rest.split(',') {
                                    let p = part.trim();
                                    if let Some(v) = p.strip_prefix("prompt:") {
                                        if let Ok(n) = v.parse::<u32>() {
                                            prompt_val = Some(n);
                                        }
                                    } else if let Some(v) = p.strip_prefix("total:") {
                                        if let Ok(n) = v.parse::<u32>() {
                                            total_val = Some(n);
                                        }
                                    } else if p.starts_with("total:") {
                                        if let Some(v) = p.strip_prefix("total:")
                                            && let Ok(n) = v.parse::<u32>()
                                        {
                                            total_val = Some(n);
                                        }
                                    } else if let Ok(n) = p.parse::<u32>() {
                                        // legacy single number after prompt:
                                        prompt_val = Some(n);
                                    }
                                }
                                if let Some(pv) = prompt_val {
                                    self.tokens_prompt_used = pv;
                                }
                                if let Some(tv) = total_val {
                                    self.tokens_total_used = Some(tv);
                                }
                                self.dirty = true;

                                // Check auto-compact threshold and trigger if necessary
                                if self.tokens_prompt_used
                                    >= self.auto_compact_prompt_token_threshold
                                    && !self.auto_compact_pending
                                {
                                    self.auto_compact_pending = true;
                                    // Inform user and dispatch compact command
                                    self.push_log(format!(
                                        "[AUTO] prompt tokens {} >= {}; triggering /compact",
                                        self.tokens_prompt_used,
                                        self.auto_compact_prompt_token_threshold
                                    ));
                                    self.dispatch("/compact");
                                }

                                continue;
                            }

                            // Legacy format: ::tokens:{n} => treat as prompt tokens
                            if let Ok(tokens) = tokens_str.parse::<u32>() {
                                self.tokens_prompt_used = tokens;
                                self.dirty = true;

                                // Check auto-compact threshold and trigger if necessary (legacy format)
                                if self.tokens_prompt_used
                                    >= self.auto_compact_prompt_token_threshold
                                    && !self.auto_compact_pending
                                {
                                    self.auto_compact_pending = true;
                                    self.push_log(format!(
                                        "[AUTO] prompt tokens {} >= {}; triggering /compact",
                                        self.tokens_prompt_used,
                                        self.auto_compact_prompt_token_threshold
                                    ));
                                    self.dispatch("/compact");
                                }
                            }
                        }
                        _ if msg.starts_with("::status:error:") => {
                            let content = &msg["::status:error:".len()..];
                            self.finalize_and_append_llm_response(content);
                            is_streaming = false;
                            self.status = Status::Error;
                            self.dirty = true;
                            // Reset processing_start_time to stop the timer
                            self.processing_start_time = None;
                        }
                        _ if msg.starts_with("::todo_list:") => {
                            let todo_list_json = &msg["::todo_list:".len()..];
                            if let Ok(todo_list) = serde_json::from_str::<
                                Vec<crate::tui::state::TodoItem>,
                            >(todo_list_json)
                            {
                                // If the list is non-empty and every item is marked completed,
                                // clear the todo list immediately so it is not displayed in the TUI.
                                let all_completed = !todo_list.is_empty()
                                    && todo_list.iter().all(|t| t.status == "completed");
                                if all_completed {
                                    // Do not display completed-only todo lists
                                    self.todo_list.clear();
                                    self.hide_todo_on_next_instruction = false;
                                } else {
                                    self.todo_list = todo_list.clone();
                                    self.hide_todo_on_next_instruction = false;
                                }
                                self.dirty = true;
                            }
                            continue;
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
                if self.process_diff_review_key(k)? {
                    continue;
                }

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
                    InputMode::SessionList => {
                        if handle_session_list_key(self, k, terminal)? {
                            return Ok(());
                        }
                    }
                }
            }

            // When the UI state is marked as dirty, always perform a full frame redraw.
            // This intentionally favors correctness and prevents ghosting artifacts
            // over aggressively skipping draws.
            if self.dirty {
                let model = self.model.clone();
                terminal.draw(|f| self.view(f, model.as_deref()))?;
                self.dirty = false;
            }
        }
    }
}

impl TuiApp {
    fn process_diff_review_key(&mut self, key: KeyEvent) -> Result<bool> {
        if self.diff_review.is_none() {
            return Ok(false);
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.dismiss_diff_review();
                Ok(true)
            }
            KeyCode::Char('a') => {
                self.accept_diff_review();
                Ok(true)
            }
            KeyCode::Char('r') => {
                self.reject_diff_review()?;
                Ok(true)
            }
            KeyCode::Left => {
                self.move_diff_selection(-1);
                Ok(true)
            }
            KeyCode::Right => {
                self.move_diff_selection(1);
                Ok(true)
            }
            KeyCode::Up => {
                self.adjust_diff_scroll(-1);
                Ok(true)
            }
            KeyCode::Down => {
                self.adjust_diff_scroll(1);
                Ok(true)
            }
            KeyCode::PageUp => {
                self.adjust_diff_scroll(-20);
                Ok(true)
            }
            KeyCode::PageDown => {
                self.adjust_diff_scroll(20);
                Ok(true)
            }
            KeyCode::Home => {
                self.set_diff_scroll(0);
                Ok(true)
            }
            KeyCode::End => {
                self.jump_diff_scroll_to_end();
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn dismiss_diff_review(&mut self) {
        if self.diff_review.take().is_some() {
            self.push_log("[diff] Closed diff preview. Changes remain applied.".to_string());
            self.dirty = true;
        }
    }

    fn accept_diff_review(&mut self) {
        if let Some(review) = self.diff_review.take() {
            let file_count = review.files.len();
            self.push_log(format!("[diff] Accepted {} file change(s).", file_count));
            self.dirty = true;
        }
    }

    fn reject_diff_review(&mut self) -> Result<()> {
        let Some(review) = self.diff_review.take() else {
            return Ok(());
        };

        let paths: Vec<String> = review.files.iter().map(|f| f.path.clone()).collect();

        if let Err(e) = revert_paths(&paths) {
            self.push_log(format!("[diff][error] Failed to revert changes: {}", e));
            self.diff_review = Some(review);
            self.dirty = true;
            return Err(e);
        }

        self.push_log("[diff] Rejected changes and restored files.".to_string());
        self.dirty = true;
        Ok(())
    }

    fn move_diff_selection(&mut self, delta: isize) {
        if let Some(review) = self.diff_review.as_mut() {
            if review.files.is_empty() {
                return;
            }

            let current = review.selected as isize;
            let max_index = review.files.len() as isize - 1;
            let mut next = current + delta;
            if next < 0 {
                next = 0;
            } else if next > max_index {
                next = max_index;
            }

            if next != current {
                review.selected = next as usize;
                if let Some(file) = review.files.get_mut(review.selected) {
                    file.scroll = 0;
                }
                self.dirty = true;
            }
        }
    }

    fn adjust_diff_scroll(&mut self, delta: isize) {
        if delta == 0 {
            return;
        }

        if let Some(review) = self.diff_review.as_mut()
            && let Some(file) = review.files.get_mut(review.selected)
        {
            if file.lines.is_empty() {
                return;
            }

            let max_scroll = file.lines.len().saturating_sub(1) as isize;
            let current = file.scroll as isize;
            let mut next = current + delta;
            if next < 0 {
                next = 0;
            } else if next > max_scroll {
                next = max_scroll;
            }

            file.scroll = next as usize;
            self.dirty = true;
        }
    }

    fn set_diff_scroll(&mut self, position: usize) {
        if let Some(review) = self.diff_review.as_mut()
            && let Some(file) = review.files.get_mut(review.selected)
        {
            let max_scroll = file.lines.len().saturating_sub(1);
            file.scroll = position.min(max_scroll);
            self.dirty = true;
        }
    }

    fn jump_diff_scroll_to_end(&mut self) {
        if let Some(review) = self.diff_review.as_mut()
            && let Some(file) = review.files.get_mut(review.selected)
        {
            if file.lines.is_empty() {
                return;
            }
            file.scroll = file.lines.len().saturating_sub(1);
            self.dirty = true;
        }
    }
}

fn revert_paths(paths: &[String]) -> Result<()> {
    for path in paths {
        if path.trim().is_empty() || path == "workspace" {
            continue;
        }

        let tracked = Command::new("git")
            .arg("ls-files")
            .arg("--error-unmatch")
            .arg(path)
            .status()
            .with_context(|| format!("checking tracking status for {}", path))?
            .success();

        if tracked {
            let output = Command::new("git")
                .arg("restore")
                .arg("--worktree")
                .arg("--")
                .arg(path)
                .output()
                .with_context(|| format!("running git restore for {}", path))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(anyhow!(
                    "git restore failed for {}: {}",
                    path,
                    stderr.trim()
                ));
            }
        } else {
            match fs::remove_file(path) {
                Ok(_) => {}
                Err(e) => {
                    if e.kind() == ErrorKind::IsADirectory {
                        fs::remove_dir_all(path)
                            .with_context(|| format!("failed to remove directory {}", path))?;
                    } else if e.kind() != ErrorKind::NotFound {
                        return Err(anyhow!("failed to remove {}: {}", path, e));
                    }
                }
            }
        }
    }

    Ok(())
}
