use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use serde::Deserialize;
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
    pub(crate) fn request_exit(&mut self) {
        self.exit_requested_at.get_or_insert_with(Instant::now);
        self.push_log("Waiting for jobs and saving the current session before exit...");
    }

    pub(crate) fn poll_exit_request(&mut self) -> bool {
        let Some(started) = self.exit_requested_at else {
            return false;
        };
        let result = if let Some(mut handler) = self.handler.take() {
            let result = handler.prepare_exit(self);
            self.handler = Some(handler);
            result
        } else {
            Ok(true)
        };
        match result {
            Ok(true) => true,
            Ok(false) if started.elapsed() < crate::jobs::JOB_SHUTDOWN_GRACE => false,
            Ok(false) => {
                self.exit_requested_at = None;
                self.push_log("Exit blocked: jobs still own the session. Inspect /jobs and retry /quit after cleanup.");
                false
            }
            Err(error) => {
                self.exit_requested_at = None;
                self.push_log(format!("Exit blocked: session remains unsaved: {error}. Fix the cause, retry /session save, then /quit."));
                false
            }
        }
    }

    pub fn event_loop(
        &mut self,
        terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    ) -> Result<()> {
        let mut last_ctrl_c_at: Option<Instant> = None;

        let mut is_streaming = false; // track streaming state
        let mut last_spinner_update = Instant::now(); // Track last spinner update time
        loop {
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

                for raw_message in drained {
                    let (msg, scoped) =
                        if let Some(rest) = raw_message.strip_prefix("::job_message:") {
                            let Some((producer, message)) = rest.split_once(':') else {
                                continue;
                            };
                            let Some(id) = crate::jobs::JobId::parse_arg(producer) else {
                                continue;
                            };
                            if !self.accept_job_message(id) {
                                continue;
                            }
                            (message.to_string(), true)
                        } else {
                            (raw_message, false)
                        };
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

                    if let Some(json) = msg.strip_prefix("::diff_rejected:") {
                        if let Ok(report) =
                            serde_json::from_str::<crate::tools::review::RejectReport>(json)
                        {
                            let payload = self
                                .handler
                                .as_ref()
                                .and_then(|h| h.review_payload(&report.id));
                            self.apply_review_report(report, payload);
                        }
                        continue;
                    }

                    // Diff review handling
                    if let Some(payload) = msg.strip_prefix("::diff_review:") {
                        if let Ok(payload) = serde_json::from_str::<DiffReviewPayload>(payload) {
                            if let Some(id) = &payload.review_id
                                && !self
                                    .handler
                                    .as_ref()
                                    .is_some_and(|h| h.review_payload(id).is_some())
                            {
                                continue;
                            }
                            let review_state = DiffReviewState::from_payload(payload);
                            let file_count = review_state.files.len();
                            let notice = if review_state.rejectable {
                                format!(
                                    "[diff] Ready for review: {file_count} file(s) changed. Use a=accept, r=reject, q=dismiss."
                                )
                            } else {
                                format!(
                                    "[diff] View only: {} Use a=accept, q=dismiss.",
                                    review_state
                                        .reject_reason
                                        .as_deref()
                                        .unwrap_or("No turn-owned rollback capture.")
                                )
                            };
                            self.diff_review = Some(review_state);
                            self.diff_rejected_pending = false;
                            self.dirty = true;
                            self.push_log(notice);
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
                            review_id: None,
                            reject_reason: None,
                            diff: output.to_string(),
                            files: vec![],
                            evidence: Vec::new(),
                            evidence_warnings: Vec::new(),
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

                    // Post-terminal Test/Lint follow-up handoff. The
                    // `JobManager` completion hook emits this only after the
                    // producer terminalized and released foreground
                    // ownership, so the single staged successor (if any)
                    // starts strictly after the predecessor. UI status
                    // messages never authorize this path.
                    if let Some(producer) = msg.strip_prefix("::job_completed:") {
                        if self.handler.is_some() && self.exit_requested_at.is_none() {
                            let mut handler = self.handler.take().unwrap();
                            handler.handle_job_completed(producer, self);
                            self.handler = Some(handler);
                        }
                        self.dirty = true;
                        continue;
                    }

                    if let Some(rest) = msg.strip_prefix("::directive_observed:") {
                        // Paired delivery `::<seq>:<id>`; stale or malformed
                        // ids never overwrite the currently tracked turn.
                        if let Some((seq, id)) = rest.split_once(':')
                            && let Ok(seq) = seq.trim().parse::<u64>()
                        {
                            self.note_directive_observed(seq, id.trim());
                        }
                        continue;
                    }

                    // Handle status messages
                    if let Some(rest) = msg.strip_prefix("::status:") {
                        if let Some(content) = rest.strip_prefix("done:") {
                            if !scoped && self.foreground_busy() {
                                continue;
                            }
                            self.finalize_and_append_llm_response(content);
                            is_streaming = false;
                            if self.foreground_busy() {
                                continue;
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

                        if let Some(content) = rest.strip_prefix("error:") {
                            if !scoped && self.foreground_busy() {
                                continue;
                            }
                            self.finalize_and_append_llm_response(content);
                            is_streaming = false;
                            if self.foreground_busy() {
                                continue;
                            }
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
                                if !scoped && self.foreground_busy() {
                                    continue;
                                }
                                if is_streaming {
                                    self.finalize_and_append_llm_response("");
                                    is_streaming = false;
                                }
                                if self.foreground_busy() {
                                    continue;
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
                                if !scoped && self.foreground_busy() {
                                    continue;
                                }
                                if is_streaming {
                                    self.finalize_and_append_llm_response("");
                                    is_streaming = false;
                                }
                                if self.foreground_busy() {
                                    continue;
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
                                if self.foreground_busy() {
                                    continue;
                                }
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
                                if !scoped && self.foreground_busy() {
                                    continue;
                                }
                                if is_streaming {
                                    self.finalize_and_append_llm_response("");
                                    is_streaming = false;
                                }
                                if self.foreground_busy() {
                                    continue;
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
                            // Replay inherits the original directive id when
                            // known; never records a duplicate observation.
                            self.dispatch_retry_after_compact(&last_input);
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

            if self.poll_exit_request() {
                return Ok(());
            }
            self.dispatch_pending_instruction();

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
                                self.request_exit();
                                continue;
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
                                    self.request_exit();
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

    fn process_diff_review_key(&mut self, key: crossterm::event::KeyEvent) -> Result<bool> {
        if self.diff_review.is_none() {
            return Ok(false);
        }

        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if self.diff_review.as_ref().is_some_and(|r| r.rejecting) {
                    if key.code == KeyCode::Esc {
                        self.dispatch("/cancel");
                    } else {
                        self.push_log("[diff] Rollback is running; Esc requests cancellation.");
                    }
                    return Ok(true);
                }
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

    pub(crate) fn accept_job_message(&self, id: crate::jobs::JobId) -> bool {
        self.latest_agent_job_id == Some(id)
            && self
                .handler
                .as_ref()
                .and_then(|h| h.foreground_job_id())
                .is_none_or(|active| active == id)
    }

    pub(crate) fn foreground_busy(&self) -> bool {
        self.handler
            .as_ref()
            .is_some_and(|handler| handler.foreground_busy())
    }

    /// Called after draining completion messages. Ownership, not a cosmetic
    /// status notice, determines when an accepted instruction may be consumed.
    pub(crate) fn dispatch_pending_instruction(&mut self) {
        if self.exit_requested_at.is_some()
            || self.foreground_busy()
            || (self.handler.is_none() && !matches!(self.status, Status::Ready | Status::Error))
        {
            return;
        }
        let Some(instruction) = self.pending_instructions.front().cloned() else {
            return;
        };
        let previous_start = self.processing_start_time;
        self.processing_start_time = Some(Instant::now());
        let accepted = if let Some(mut handler) = self.handler.take() {
            let accepted = if self.diff_rejected_pending && !instruction.starts_with('/') {
                if handler.foreground_busy() {
                    false
                } else {
                    self.queued_dispatch_rejected = false;
                    let effective = format!(
                        "[SYSTEM NOTE] The user rejected reviewed changes. Account for the restored files and any remaining conflicts.\n\n{instruction}"
                    );
                    handler.handle_augmented_user_prompt(&instruction, &effective, self);
                    !self.queued_dispatch_rejected
                }
            } else {
                handler.handle_queued(&instruction, self)
            };
            self.handler = Some(handler);
            accepted
        } else {
            self.dispatch(&instruction);
            true
        };
        if !accepted {
            self.processing_start_time = previous_start;
        }
        if accepted {
            self.last_elapsed_time = None;
            self.pending_instructions.pop_front();
            if !instruction.starts_with('/') || instruction.starts_with("/reset") {
                self.diff_rejected_pending = false;
            }
        }
        self.dirty = true;
    }

    pub(crate) fn apply_review_report(
        &mut self,
        report: crate::tools::review::RejectReport,
        payload: Option<DiffReviewPayload>,
    ) {
        if !self.diff_review.as_ref().is_some_and(|r| {
            r.review_id.as_deref() == Some(report.id.as_str()) && r.reject_job_id == report.job_id
        }) {
            return;
        }
        self.push_log(format!(
            "[diff] Restored {} mutation(s); {} pending.{}",
            report.restored,
            report.pending,
            report
                .error
                .as_ref()
                .map(|e| format!(" {e}"))
                .unwrap_or_default()
        ));
        for warning in report.warnings {
            self.push_log(format!("[diff][warning] {warning}"));
        }
        self.diff_rejected_pending |= report.restored > 0;
        if report.pending == 0 && report.error.is_none() {
            if let Some(handler) = &self.handler {
                handler.dismiss_review(&report.id);
            }
            self.diff_review = None;
        } else if let Some(payload) = payload {
            self.diff_review = Some(DiffReviewState::from_payload(payload));
        }
        self.dirty = true;
    }

    fn dismiss_diff_review(&mut self) {
        if self.diff_review.as_ref().is_some_and(|r| r.rejecting) {
            return;
        }
        if let Some(review) = self.diff_review.take() {
            if let Some(id) = &review.review_id
                && let Some(handler) = &self.handler
            {
                handler.dismiss_review(id);
            }
            self.push_log("[diff] Closed diff preview. Changes remain applied.".to_string());
            self.dirty = true;
        }
    }

    fn accept_diff_review(&mut self) {
        if self.diff_review.as_ref().is_some_and(|r| r.rejecting) {
            self.push_log("[diff] Rollback is running; wait for its result.");
            return;
        }
        if let Some(review) = self.diff_review.take() {
            if let Some(id) = &review.review_id
                && let Some(handler) = &self.handler
            {
                handler.dismiss_review(id);
            }
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
            self.push_log(format!(
                "[diff] {}",
                review
                    .reject_reason
                    .as_deref()
                    .unwrap_or("No turn-owned rollback capture; view only.")
            ));
            self.diff_review = Some(review);
            self.dirty = true;
            return Ok(());
        }

        let id = review.review_id.clone();
        self.diff_review = Some(review);
        if let Some(id) = id
            && let Some(mut handler) = self.handler.take()
        {
            handler.reject_review(&id, self);
            self.handler = Some(handler);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        jobs::{JobKind, JobManager, JobRunOutcome, JobScope, JobSpec, WorkspaceAccess},
        tui::commands::core::CommandHandler,
    };
    use std::sync::{Arc, Mutex};
    struct QueueHandler {
        jobs: JobManager,
        calls: Arc<Mutex<Vec<String>>>,
        reject_once: bool,
    }
    impl CommandHandler for QueueHandler {
        fn handle(&mut self, line: &str, _: &mut TuiApp) {
            self.calls.lock().unwrap().push(line.into());
        }
        fn foreground_busy(&self) -> bool {
            self.jobs.foreground_id().is_some()
        }
        fn handle_queued(&mut self, line: &str, ui: &mut TuiApp) -> bool {
            if self.reject_once {
                self.reject_once = false;
                return false;
            }
            if self.foreground_busy() {
                return false;
            }
            self.handle(line, ui);
            true
        }
        fn get_custom_commands(&self) -> Vec<String> {
            vec![]
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }
    #[tokio::test]
    async fn queue_waits_for_cancel_cleanup_and_save_barriers_then_fifo_once() {
        for outcome in [
            JobRunOutcome::Completed,
            JobRunOutcome::Cancelled,
            JobRunOutcome::Failed {
                message: "save failed".into(),
            },
        ] {
            let jobs = JobManager::new();
            let (release, held) = tokio::sync::oneshot::channel();
            let (started, running) = tokio::sync::oneshot::channel();
            let cancel = outcome != JobRunOutcome::Completed;
            let id = jobs
                .spawn(
                    JobSpec::new(
                        JobKind::AgentTurn,
                        JobScope::Foreground,
                        WorkspaceAccess::Write,
                        "save/cleanup barrier",
                    ),
                    move |_ctx| async move {
                        started.send(()).unwrap();
                        let _ = held.await;
                        outcome
                    },
                )
                .unwrap();
            running.await.unwrap(); // actual owner is now in cleanup/save
            let calls = Arc::new(Mutex::new(vec![]));
            let mut ui = TuiApp::new_for_test("queue", None, "default");
            ui.handler = Some(Box::new(QueueHandler {
                jobs: jobs.clone(),
                calls: calls.clone(),
                reject_once: false,
            }));
            ui.pending_instructions.extend(["A".into(), "B".into()]);
            ui.status = Status::Ready;
            let start = Instant::now();
            ui.processing_start_time = Some(start);
            if cancel {
                jobs.cancel(id);
            } // cosmetic cancelled/done can already have arrived
            for _ in 0..3 {
                ui.dispatch_pending_instruction();
            }
            assert_eq!(ui.pending_instructions, ["A", "B"]);
            assert!(calls.lock().unwrap().is_empty());
            assert_eq!(ui.processing_start_time, Some(start));
            release.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while jobs.foreground_id().is_some() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            ui.dispatch_pending_instruction();
            ui.dispatch_pending_instruction();
            ui.dispatch_pending_instruction();
            assert_eq!(*calls.lock().unwrap(), ["A", "B"]);
            assert!(ui.pending_instructions.is_empty());
        }
    }
    #[test]
    fn queue_reservation_race_retains_input_and_note_without_duplicate_dispatch() {
        let calls = Arc::new(Mutex::new(vec![]));
        let mut ui = TuiApp::new_for_test("queue", None, "default");
        ui.handler = Some(Box::new(QueueHandler {
            jobs: JobManager::new(),
            calls: calls.clone(),
            reject_once: true,
        }));
        ui.pending_instructions.push_back("A".into());
        let start = Instant::now();
        ui.processing_start_time = Some(start);
        ui.dispatch_pending_instruction();
        assert_eq!(ui.pending_instructions, ["A"]);
        assert_eq!(ui.processing_start_time, Some(start));
        ui.dispatch_pending_instruction();
        ui.dispatch_pending_instruction();
        assert_eq!(*calls.lock().unwrap(), ["A"]);
    }
    #[test]
    fn queue_ownership_overrides_stale_cosmetic_thinking_status() {
        let calls = Arc::new(Mutex::new(vec![]));
        let mut ui = TuiApp::new_for_test("queue", None, "default");
        ui.handler = Some(Box::new(QueueHandler {
            jobs: JobManager::new(),
            calls: calls.clone(),
            reject_once: false,
        }));
        ui.status = Status::Thinking;
        ui.pending_instructions.push_back("A".into());
        ui.dispatch_pending_instruction();
        assert_eq!(*calls.lock().unwrap(), ["A"]);
        assert!(ui.pending_instructions.is_empty());
    }

    #[test]
    fn stale_job_messages_are_rejected_before_stream_processing() {
        let mut ui = TuiApp::new_for_test("scope", None, "default");
        ui.latest_agent_job_id = Some(crate::jobs::JobId(9));
        let start = Instant::now();
        ui.processing_start_time = Some(start);
        ui.llm_parsing_buffer = "new stream".into();
        assert!(!ui.accept_job_message(crate::jobs::JobId(8)));
        assert!(ui.accept_job_message(crate::jobs::JobId(9)));
        assert_eq!(ui.llm_parsing_buffer, "new stream");
        assert_eq!(ui.processing_start_time, Some(start));
    }
    #[test]
    fn running_reject_keeps_panel_and_esc_requests_cancel() {
        let calls = Arc::new(Mutex::new(vec![]));
        let mut ui = TuiApp::new_for_test("reject", None, "default");
        ui.handler = Some(Box::new(QueueHandler {
            jobs: JobManager::new(),
            calls: calls.clone(),
            reject_once: false,
        }));
        let payload = DiffReviewPayload {
            review_id: Some("review".into()),
            reject_reason: None,
            diff: "diff --git a/a b/a\n+agent\n".into(),
            files: vec!["a".into()],
            evidence: vec![],
            evidence_warnings: vec![],
        };
        let mut review = DiffReviewState::from_payload(payload);
        review.rejectable = false;
        review.rejecting = true;
        ui.diff_review = Some(review);
        for code in [KeyCode::Char('a'), KeyCode::Char('q'), KeyCode::Esc] {
            ui.process_diff_review_key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE))
                .unwrap();
            assert!(ui.diff_review.as_ref().unwrap().rejecting);
        }
        assert_eq!(*calls.lock().unwrap(), ["/cancel"]);
    }

    #[test]
    fn legacy_diff_without_capture_is_view_only() {
        let payload = DiffReviewPayload {
            review_id: None,
            reject_reason: None,
            diff: "diff --git a/user b/user\n-user\n+agent\n".into(),
            files: vec!["user".into()],
            evidence: vec![],
            evidence_warnings: vec![],
        };
        let mut ui = TuiApp::new_for_test("review", None, "default");
        ui.diff_review = Some(DiffReviewState::from_payload(payload));
        ui.reject_diff_review().unwrap();
        assert!(!ui.diff_review.unwrap().rejectable);
    }
}
