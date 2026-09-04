use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyModifiers};
use serde::Deserialize;
use std::fs;
use std::io::ErrorKind;
use std::process::Command;
use std::time::{Duration, Instant};
use tracing::debug;

use crate::diff_review::DiffReviewPayload;
use crate::tui::diff_review::DiffReviewState;
use crate::tui::event_handlers::{
    handle_file_search_key, handle_history_search_key, handle_normal_mode_key,
};
use crate::tui::state::{InputMode, Status, TuiApp};

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
            // Process instruction queue if ready
            let is_ready = matches!(self.status, Status::Ready | Status::Error);
            if is_ready && let Some(instruction) = self.pending_instructions.pop_front() {
                // If the user rejected a diff review, prepend a system note so the
                // LLM knows its changes were reverted. Slash commands are passed
                // through untouched (prepending would break `/` routing); `/reset`
                // starts a fresh context, so the pending note is dropped with it.
                if self.diff_rejected_pending && instruction.starts_with("/reset") {
                    self.diff_rejected_pending = false;
                }
                if self.diff_rejected_pending && !instruction.starts_with('/') {
                    self.diff_rejected_pending = false;
                    let instruction_with_note = format!(
                        "[SYSTEM NOTE] The user rejected the previous changes and the affected files were reverted to their pre-change state. Take this into account when proceeding.\n\n{}",
                        instruction
                    );
                    self.dispatch(&instruction_with_note);
                } else {
                    self.dispatch(&instruction);
                }
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

                    // Diff review handling
                    if let Some(payload) = msg.strip_prefix("::diff_review:") {
                        if let Ok(payload) = serde_json::from_str::<DiffReviewPayload>(payload) {
                            let review_state = DiffReviewState::from_payload(payload);
                            let file_count = review_state.files.len();
                            self.diff_review = Some(review_state);
                            self.diff_rejected_pending = false;
                            self.dirty = true;
                            self.push_log(format!(
                                "[diff] Ready for review: {} file(s) changed. Use a=accept, r=reject, q=dismiss.",
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

                    // Legacy raw-diff payloads have no reliable file list, so
                    // rejecting (reverting) them would be unsafe. Nothing in the
                    // codebase emits this message anymore; view-only.
                    if let Some(output) = msg.strip_prefix("::diff_output:") {
                        let payload = DiffReviewPayload {
                            diff: output.to_string(),
                            files: vec![],
                        };
                        let review_state = DiffReviewState::from_payload(payload);
                        self.diff_review = Some(review_state);
                        self.diff_rejected_pending = false;
                        self.dirty = true;
                        self.push_log(
                            "[diff] Received legacy diff payload. View only (a=accept, q=dismiss)."
                                .to_string(),
                        );
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
                        // Expected format: "prompt:<n>,total:<n>" (see handlers).
                        let mut prompt: Option<u64> = None;
                        let mut total: Option<u64> = None;
                        for part in tokens_str.split(',') {
                            if let Some(value) = part.strip_prefix("prompt:") {
                                prompt = value.trim().parse::<u64>().ok();
                            } else if let Some(value) = part.strip_prefix("total:") {
                                total = value.trim().parse::<u64>().ok();
                            } else if let Ok(legacy) = part.trim().parse::<u32>() {
                                // Legacy bare-number payload.
                                prompt = Some(legacy as u64);
                            }
                        }
                        if let Some(prompt_tokens) = prompt {
                            self.tokens_prompt_used = prompt_tokens.min(u32::MAX as u64) as u32;
                            self.dirty = true;
                        }
                        if let Some(total_tokens) = total {
                            self.tokens_used = total_tokens.min(u32::MAX as u64) as u32;
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
                && last_heartbeat.elapsed() > Duration::from_secs(180)
            {
                // Only warn once every 30 seconds to avoid spamming
                // We can reset last_heartbeat to now to silence it for another 30 seconds,
                // essentially treating the warning itself as a heartbeat of sorts (or rather acknowledgement)
                // But better to just log a warning and maybe set a flag?
                // For simplicity, let's just log and update heartbeat so we don't spam.
                self.push_log(
                    "[WARN] No signal from agent. It might be stuck or network is slow."
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

                        // Diff review keys take precedence while the panel is open,
                        // but only when the input box is empty; otherwise the keys
                        // must keep flowing into the textarea (typing "run the
                        // tests" must not trigger a revert).
                        if self.diff_review.is_some()
                            && self.input_is_empty()
                            && self.process_diff_review_key(k)?
                        {
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

impl TuiApp {
    /// True when the input textarea has no user-typed content.
    fn input_is_empty(&self) -> bool {
        self.textarea.lines().iter().all(|line| line.is_empty())
    }

    fn process_diff_review_key(
        &mut self,
        key: ratatui::crossterm::event::KeyEvent,
    ) -> Result<bool> {
        if self.diff_review.is_none() {
            return Ok(false);
        }

        use ratatui::crossterm::event::KeyCode;

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

        if !review.rejectable {
            self.push_log(
                "[diff] This diff has no reliable file list (legacy payload); reject is unavailable."
                    .to_string(),
            );
            self.diff_review = Some(review);
            self.dirty = true;
            return Ok(());
        }

        let paths: Vec<String> = review.file_paths();
        let project_root = self.cfg.as_ref().map(|c| c.project_root.clone());

        // On failure, keep the panel open so the user can retry; a failed
        // revert must never propagate out of the event loop (that would
        // terminate the whole TUI).
        if let Err(e) = revert_paths(&paths, project_root.as_deref()) {
            self.push_log(format!("[diff][error] Failed to revert changes: {}", e));
            self.diff_review = Some(review);
            self.dirty = true;
            return Ok(());
        }

        self.diff_rejected_pending = true;
        self.push_log(
            "[diff] Rejected changes and restored the agent's modified files. The agent will be notified on your next instruction."
                .to_string(),
        );
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

        let viewport = self.diff_viewport_height.get().max(1);
        if let Some(review) = self.diff_review.as_mut()
            && let Some(file) = review.files.get_mut(review.selected)
        {
            if file.lines.is_empty() {
                return;
            }

            let max_scroll = file.lines.len().saturating_sub(viewport) as isize;
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
        let viewport = self.diff_viewport_height.get().max(1);
        if let Some(review) = self.diff_review.as_mut()
            && let Some(file) = review.files.get_mut(review.selected)
        {
            let max_scroll = file.lines.len().saturating_sub(viewport);
            file.scroll = position.min(max_scroll);
            self.dirty = true;
        }
    }

    fn jump_diff_scroll_to_end(&mut self) {
        let viewport = self.diff_viewport_height.get().max(1);
        if let Some(review) = self.diff_review.as_mut()
            && let Some(file) = review.files.get_mut(review.selected)
        {
            if file.lines.is_empty() {
                return;
            }
            file.scroll = file.lines.len().saturating_sub(viewport);
            self.dirty = true;
        }
    }
}

fn revert_paths(paths: &[String], project_root: Option<&std::path::Path>) -> Result<()> {
    let cwd = project_root.map(|p| p.to_path_buf()).unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });

    for path in paths {
        if path.trim().is_empty() || path == "workspace" {
            continue;
        }

        let tracked = Command::new("git")
            .arg("ls-files")
            .arg("--error-unmatch")
            .arg(path)
            .current_dir(&cwd)
            .output()
            .with_context(|| format!("checking tracking status for {}", path))?
            .status
            .success();

        if tracked {
            let output = Command::new("git")
                .arg("restore")
                .arg("--worktree")
                .arg("--")
                .arg(path)
                .current_dir(&cwd)
                .output()
                .with_context(|| format!("running git restore for {}", path))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(anyhow::anyhow!(
                    "git restore failed for {}: {}",
                    path,
                    stderr.trim()
                ));
            }
        } else {
            let full_path = cwd.join(path);
            match fs::remove_file(&full_path) {
                Ok(_) => {}
                Err(e) => {
                    if e.kind() == ErrorKind::IsADirectory {
                        fs::remove_dir_all(&full_path)
                            .with_context(|| format!("failed to remove directory {}", path))?;
                    } else if e.kind() != ErrorKind::NotFound {
                        return Err(anyhow::anyhow!("failed to remove {}: {}", path, e));
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    fn run_git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo(dir: &Path) {
        run_git(dir, &["init", "-q"]);
        run_git(dir, &["config", "user.email", "test@example.com"]);
        run_git(dir, &["config", "user.name", "Test User"]);
    }

    #[test]
    fn test_revert_paths_restores_tracked_file() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        init_repo(root);

        std::fs::write(root.join("foo.txt"), "original\n").unwrap();
        run_git(root, &["add", "foo.txt"]);
        run_git(root, &["commit", "-q", "-m", "init"]);
        std::fs::write(root.join("foo.txt"), "modified\n").unwrap();

        revert_paths(&["foo.txt".to_string()], Some(root)).unwrap();

        let content = std::fs::read_to_string(root.join("foo.txt")).unwrap();
        assert_eq!(content, "original\n");
    }

    #[test]
    fn test_revert_paths_removes_untracked_file() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        init_repo(root);

        std::fs::write(root.join("new_file.txt"), "created by agent\n").unwrap();
        assert!(root.join("new_file.txt").exists());

        revert_paths(&["new_file.txt".to_string()], Some(root)).unwrap();
        assert!(!root.join("new_file.txt").exists());
    }

    #[test]
    fn test_revert_paths_ignores_workspace_placeholder() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        init_repo(root);

        // "workspace" is a synthetic placeholder and must not cause errors
        revert_paths(&["workspace".to_string(), "".to_string()], Some(root)).unwrap();
    }

    #[test]
    fn test_revert_paths_skips_missing_files() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        init_repo(root);

        // A path that neither exists nor is tracked should be ignored, not error
        revert_paths(&["does_not_exist.txt".to_string()], Some(root)).unwrap();
    }
}
