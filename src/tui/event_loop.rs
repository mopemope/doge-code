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
    handle_file_search_key, handle_history_search_key, handle_normal_mode_key,
    handle_session_list_key, handle_shell_mode_key,
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
                    if let Some(encoded) = msg.strip_prefix("::shell_output_bin:") {
                        use base64::{Engine as _, engine::general_purpose};
                        if let Ok(decoded) = general_purpose::STANDARD.decode(encoded) {
                            let text = String::from_utf8_lossy(&decoded);
                            self.shell_output_buffer.push_str(&text);
                            self.dirty = true;
                        }
                        continue;
                    }
                    // Legacy shell output handling (keep for compatibility if needed)
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

                    if let Some(payload) = msg.strip_prefix("::lint_issues:") {
                        if let Ok(issues) = serde_json::from_str::<
                            Vec<crate::tui::commands::handlers::slash_commands::lint::LintIssue>,
                        >(payload)
                        {
                            self.push_log(format!(
                                "[lint] Found {} issues. Sending to LLM for analysis...",
                                issues.len()
                            ));
                            self.dirty = true;

                            // Create a prompt for LLM to fix the issues
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

                            prompt.push_str("Please provide specific code fixes for each issue. If you need to see the current content of any file, use the appropriate tool to read it first, then provide the corrected code with clear explanations.");

                            // Send the prompt to LLM via dispatch
                            self.push_log(format!("> {}", prompt));
                            self.last_user_input = Some(prompt.clone());

                            // Trigger LLM processing using the existing dispatch mechanism
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
                        self.push_log(
                            "[lint] Sending full command output to LLM for analysis and fixes...",
                        );
                        self.dirty = true;

                        // Send the prompt to LLM via dispatch
                        self.push_log(format!("> {}", prompt));
                        self.last_user_input = Some(prompt.to_string());

                        // Trigger LLM processing using the existing dispatch mechanism
                        self.dispatch(prompt);
                        continue;
                    }

                    if let Some(prompt) = msg.strip_prefix("::test_failures_analysis:") {
                        self.push_log(
                            "[test] Sending test failures to LLM for analysis and fixes...",
                        );
                        self.dirty = true;

                        // Send the prompt to LLM via dispatch
                        self.push_log(format!("> {}", prompt));
                        self.last_user_input = Some(prompt.to_string());

                        // Trigger LLM processing using the existing dispatch mechanism
                        self.dispatch(prompt);
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

                    // Handle status messages with payloads
                    if let Some(rest) = msg.strip_prefix("::status:") {
                        if let Some(content) = rest.strip_prefix("done:") {
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
                            continue;
                        }

                        if let Some(content) = rest.strip_prefix("error:") {
                            self.finalize_and_append_llm_response(content);
                            is_streaming = false;
                            self.status = Status::Error;
                            self.dirty = true;
                            self.processing_start_time = None;
                            continue;
                        }

                        if let Some(msg_body) = rest.strip_prefix("waiting:") {
                            self.status = Status::Waiting;
                            self.push_log(format!("[WAIT] {}", msg_body));
                            self.dirty = true;
                            self.spinner_state = 0;
                            continue;
                        }

                        if let Some(msg_body) = rest.strip_prefix("compacting:") {
                            self.status = Status::Processing;
                            self.push_log(format!("[AUTO] {}", msg_body));
                            self.dirty = true;
                            self.spinner_state = 0;
                            continue;
                        }

                        if let Some(msg_body) = rest.strip_prefix("warning:") {
                            // Do not change status, just log warning
                            self.push_log(format!("[WARN] {}", msg_body));
                            self.dirty = true;
                            continue;
                        }

                        // Exact matches for status updates without payload
                        match rest {
                            "done" => {
                                if is_streaming {
                                    self.finalize_and_append_llm_response("");
                                    is_streaming = false;
                                }
                                self.status = Status::Done;
                                self.dirty = true;
                                if let Some(start_time) = self.processing_start_time.take() {
                                    let elapsed = start_time.elapsed();
                                    let elapsed_secs = elapsed.as_secs();
                                    let hours = elapsed_secs / 3600;
                                    let minutes = (elapsed_secs % 3600) / 60;
                                    let seconds = elapsed_secs % 60;
                                    self.last_elapsed_time =
                                        Some(format!("{:02}:{:02}:{:02}", hours, minutes, seconds));
                                }
                                continue;
                            }
                            "cancelled" => {
                                if is_streaming {
                                    self.finalize_and_append_llm_response("");
                                    is_streaming = false;
                                }
                                self.status = Status::Cancelled;
                                self.dirty = true;
                                self.processing_start_time = None;
                                continue;
                            }
                            "preparing" => {
                                self.status = Status::Preparing;
                                self.dirty = true;
                                self.spinner_state = 0;
                                continue;
                            }
                            "sending" => {
                                self.status = Status::Sending;
                                self.dirty = true;
                                self.spinner_state = 0;
                                continue;
                            }
                            "waiting" => {
                                self.status = Status::Waiting;
                                self.dirty = true;
                                self.spinner_state = 0;
                                continue;
                            }
                            "streaming" => {
                                if !is_streaming {
                                    self.llm_parsing_buffer.clear();
                                    is_streaming = true;
                                }
                                self.status = Status::Streaming;
                                self.dirty = true;
                                self.spinner_state = 0;
                                continue;
                            }
                            "processing" => {
                                self.status = Status::Processing;
                                self.dirty = true;
                                self.spinner_state = 0;
                                continue;
                            }
                            "idle" => {
                                self.status = Status::Idle;
                                self.dirty = true;
                                self.spinner_state = 0;
                                continue;
                            }
                            "shell_running" => {
                                self.status = Status::ShellCommandRunning;
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

                    // Handle token updates (New format with prompt/total parsing is already handled above in the loop,
                    // but this block handles the simple ::tokens:{n} legacy case if it slips through or for other variants)
                    // Actually, the new format handler at the top of the loop uses continue, so we only get here if it didn't match.
                    // Let's keep the existing handling for other message types.

                    if let Some(payload) = msg.strip_prefix("::append:") {
                        self.append_stream_token_structured(payload);
                        self.dirty = true;
                        continue;
                    }

                    // Handle structured error messages
                    if let Some(rest) = msg.strip_prefix("::error:") {
                        // Format: ::error:{category}:{msg} or ::error:{msg}
                        let parts: Vec<&str> = rest.splitn(2, ':').collect();
                        if parts.len() == 2 {
                            self.push_log(format!("[ERROR][{}] {}", parts[0], parts[1]));
                        } else {
                            self.push_log(format!("[ERROR] {}", rest));
                        }
                        // We don't necessarily change status to Error here as it might be a non-fatal error reported by a tool
                        // But if it's critical, the sender usually sends ::status:error afterward.
                        self.dirty = true;
                        continue;
                    }

                    if let Some(plan_list_json) = msg.strip_prefix("::plan_list:") {
                        // New format: { "items": [...], "approved": bool }
                        #[derive(serde::Deserialize)]
                        struct PlanListPayload {
                            items: Vec<crate::tui::state::PlanItem>,
                            approved: bool,
                        }
                        if let Ok(payload) = serde_json::from_str::<PlanListPayload>(plan_list_json)
                        {
                            self.apply_plan_list_update(payload.items, payload.approved);
                        } else if let Ok(plan_list) =
                            serde_json::from_str::<Vec<crate::tui::state::PlanItem>>(plan_list_json)
                        {
                            // Legacy format: just array of items
                            self.apply_plan_list_update(plan_list, false);
                        }
                        continue;
                    }

                    if msg == "::trigger_compact" {
                        self.push_log(
                            "[AUTO] Triggering /compact due to context length exceeded."
                                .to_string(),
                        );
                        self.dispatch("/compact");
                        self.dirty = true;
                        continue;
                    }

                    if msg == "[SUCCESS] Conversation history has been compacted." {
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
                        continue;
                    }

                    // Fallback for generic logging
                    // Handle token updates legacy fallback if needed
                    if let Some(tokens_str) = msg.strip_prefix("::tokens:") {
                        if let Ok(tokens) = tokens_str.parse::<u32>() {
                            self.tokens_used = tokens;
                            self.dirty = true;
                        }
                        continue;
                    }

                    if msg.starts_with("::update_remaining_tokens") {
                        // Update remaining context tokens
                        if msg.len() > 22 {
                            // "::update_remaining_tokens:".len() == 22
                            if let Ok(context_size) = msg[22..].parse::<u32>() {
                                self.update_remaining_context_tokens(Some(context_size));
                            }
                        } else {
                            // No context size provided, just update with None
                            self.update_remaining_context_tokens(None);
                        }
                        continue;
                    }

                    // Check for duplicate LLM responses
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

            if event::poll(Duration::from_millis(10))? {
                let event = event::read()?;

                // Add debug logging for ALL events to see what's being captured
                tracing::debug!("Event captured: {:?}", event);

                // Handle mouse events first, before any other events
                match event {
                    Event::Resize(w, h) => {
                        self.window_width = w as usize;
                        self.main_content_height = h.saturating_sub(4) as usize;
                        self.log_heights.clear();
                        self.dirty = true;
                        continue;
                    }
                    Event::Mouse(mouse_event) => {
                        tracing::debug!(
                            "Mouse event captured in main event loop: {:?}",
                            mouse_event.kind
                        );

                        // Only handle scroll events for log scrolling
                        match mouse_event.kind {
                            event::MouseEventKind::ScrollUp => {
                                let visible_lines = terminal
                                    .size()
                                    .map(|s| s.height.saturating_sub(3) as usize)
                                    .unwrap_or(20);
                                let scroll_lines = visible_lines.saturating_div(3).max(1);
                                tracing::debug!(
                                    "Mouse scroll up detected, scrolling {} lines",
                                    scroll_lines
                                );
                                self.scroll_up(scroll_lines);
                            }
                            event::MouseEventKind::ScrollDown => {
                                let visible_lines = terminal
                                    .size()
                                    .map(|s| s.height.saturating_sub(3) as usize)
                                    .unwrap_or(20);
                                let scroll_lines = visible_lines.saturating_div(3).max(1);
                                tracing::debug!(
                                    "Mouse scroll down detected, scrolling {} lines",
                                    scroll_lines
                                );
                                self.scroll_down(scroll_lines);
                            }
                            _ => {
                                tracing::debug!(
                                    "Other mouse event ignored: {:?}",
                                    mouse_event.kind
                                );
                            }
                        }
                        continue;
                    }
                    Event::Key(k) => {
                        if self.process_diff_review_key(k)? {
                            continue;
                        }

                        // Global key handlers
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

                        // Mode-specific key handlers
                        match self.input_mode {
                            InputMode::Normal => {
                                if handle_normal_mode_key(self, k, terminal)? {
                                    return Ok(());
                                }
                            }
                            InputMode::Shell => {
                                handle_shell_mode_key(self, k, terminal)?;
                            }
                            InputMode::SessionList => {
                                if handle_session_list_key(self, k, terminal)? {
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
                let amount = self.main_content_height.saturating_sub(1).max(1) as isize;
                self.adjust_diff_scroll(-amount);
                Ok(true)
            }
            KeyCode::PageDown => {
                let amount = self.main_content_height.saturating_sub(1).max(1) as isize;
                self.adjust_diff_scroll(amount);
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

impl TuiApp {}
