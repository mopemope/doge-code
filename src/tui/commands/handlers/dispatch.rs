use crate::tui::view::TuiApp;
use std::any::Any;

use crate::tui::commands::core::{CommandHandler, TuiExecutor};
use crate::tui::commands::handlers::custom::load_custom_commands;
use crate::tui::commands::handlers::slash_commands::cancel::{
    handle_cancel, handle_cancel_with_args,
};
use crate::tui::commands::handlers::slash_commands::clear::handle_clear;
use crate::tui::commands::handlers::slash_commands::compact::handle_compact;
use crate::tui::commands::handlers::slash_commands::edit_symbol::handle_edit_symbol;
use crate::tui::commands::handlers::slash_commands::fix::handle_fix;
use crate::tui::commands::handlers::slash_commands::git_worktree::handle_git_worktree;
use crate::tui::commands::handlers::slash_commands::help::handle_help;
use crate::tui::commands::handlers::slash_commands::jobs::handle_jobs;
use crate::tui::commands::handlers::slash_commands::lint::handle_lint;
use crate::tui::commands::handlers::slash_commands::map::handle_map;
use crate::tui::commands::handlers::slash_commands::open::handle_open;
use crate::tui::commands::handlers::slash_commands::quit::handle_quit;
use crate::tui::commands::handlers::slash_commands::rebuild_repomap::handle_rebuild_repomap;
use crate::tui::commands::handlers::slash_commands::reset::handle_reset;
use crate::tui::commands::handlers::slash_commands::stack::handle_stack;
use crate::tui::commands::handlers::slash_commands::test::handle_test;

use crate::tui::commands::handlers::slash_commands::theme::handle_theme;
use crate::tui::commands::handlers::slash_commands::tokens::handle_tokens;
use crate::tui::commands::handlers::slash_commands::tools::handle_tools;

// Refactored to delegate slash commands to dedicated modules for better modularity and maintainability.
// This allows each command to be tested independently and keeps dispatch.rs focused on routing.

impl CommandHandler for TuiExecutor {
    fn prepare_exit(&mut self, ui: &mut TuiApp) -> anyhow::Result<bool> {
        self.prepare_session_exit(ui)
    }

    fn foreground_job_id(&self) -> Option<crate::jobs::JobId> {
        self.jobs.foreground_id()
    }
    fn foreground_busy(&self) -> bool {
        self.jobs.foreground_id().is_some()
    }
    fn handle_queued(&mut self, line: &str, ui: &mut TuiApp) -> bool {
        if self.foreground_busy() {
            return false;
        }
        ui.queued_dispatch_rejected = false;
        self.handle(line, ui);
        !ui.queued_dispatch_rejected
    }
    fn review_payload(&self, id: &str) -> Option<crate::diff_review::DiffReviewPayload> {
        self.tools.review_payload(id)
    }
    fn validate_review_feedback(
        &self,
        batch: &crate::features::review_feedback::FeedbackBatch,
    ) -> anyhow::Result<()> {
        if self.jobs.foreground_id().is_some() {
            anyhow::bail!("Foreground work is running; comments retained.");
        }
        anyhow::ensure!(self.client.is_some(), "No model client; comments retained.");
        self.tools.validate_review_feedback(batch)
    }
    fn submit_review_feedback(
        &mut self,
        mut batch: crate::features::review_feedback::FeedbackBatch,
        ui: &mut TuiApp,
    ) -> anyhow::Result<crate::jobs::JobId> {
        let key = (batch.id.clone(), batch.revision);
        anyhow::ensure!(
            !self.feedback_submissions.contains(&key),
            "This batch revision was already submitted; comments retained."
        );
        self.validate_review_feedback(&batch)?;
        batch.original_directive_ids = self.tools.review_feedback_directives(&batch)?;
        if self.ui_tx.is_none() {
            self.set_ui_tx(ui.sender());
        }
        let job = crate::tui::commands::agent_job::spawn_agent_turn(
            self,
            ui,
            "[Review feedback batch]",
            "Apply the confirmed hunk comments using the existing plan.".into(),
            false,
            crate::tui::commands::agent_job::AgentTurnProvenance::ReviewFeedback {
                batch: Box::new(batch),
            },
        )?;
        self.feedback_submissions.insert(key);
        Ok(job)
    }
    fn dismiss_review(&self, id: &str) {
        self.tools.dismiss_review(id);
    }
    fn reject_review(&mut self, id: &str, ui: &mut TuiApp) {
        self.start_review_reject(id, ui);
    }
    fn handle(&mut self, line: &str, ui: &mut TuiApp) {
        // This function was extracted from the big handlers.rs for readability.
        if self.ui_tx.is_none() {
            self.set_ui_tx(ui.sender());
        }
        let line = line.trim();
        if line.is_empty() {
            return;
        }

        // Integrated Shell Command Handling:
        // Execute shell commands directly if prefixed with '!'
        if let Some(cmd) = line.strip_prefix('!') {
            if let Some(session) = ui.shell_session.as_mut() {
                let cmd = cmd.trim();
                if let Err(e) = session.write(&format!("{}\n", cmd)) {
                    ui.push_log(format!("Failed to write to shell: {}", e));
                }
                // Set status to Running to indicate shell activity
                ui.status = crate::tui::state::Status::Running;
            } else {
                ui.push_log("[ERROR] Shell session is not available.");
            }
            return;
        }

        // Quick Execute Command:
        // Skip implementation plan enforcement for immediate execution
        if let Some(rest) = line.strip_prefix("/quick") {
            let args = rest.trim();
            if args.is_empty() {
                ui.push_log("Usage: /quick <instruction>");
                return;
            }
            self.handle_dispatch_rest(args, ui, true);
            return;
        }

        if line.split_whitespace().next() == Some("/evidence") {
            crate::tui::commands::handlers::slash_commands::evidence::handle_evidence(
                self,
                ui,
                line.strip_prefix("/evidence").unwrap_or("").trim(),
            );
            return;
        }
        match line {
            "/help" => handle_help(self, ui),
            "/tools" => handle_tools(self, ui),
            "/quit" => handle_quit(self, ui),
            "/clear" => handle_clear(self, ui),
            "/tokens" => handle_tokens(self, ui),
            "/rebuild-repomap" => handle_rebuild_repomap(self, ui),
            "/reset" => handle_reset(ui),
            "/jobs" => handle_jobs(self, ui),
            "/compact" => handle_compact(self, ui),
            "/map" => handle_map(self, ui),
            "/edit-symbol" => handle_edit_symbol(self, ui),
            "/lint" => handle_lint(self, ui),
            "/test" => handle_test(self, ui),

            "/cancel" => handle_cancel(self, ui),
            line if line.split_whitespace().next() == Some("/cancel") => {
                let args = line.strip_prefix("/cancel").unwrap_or("").trim();
                handle_cancel_with_args(self, ui, Some(args));
            }

            line if line.starts_with("/stack") => {
                let args = line.strip_prefix("/stack").unwrap_or("").trim();
                handle_stack(self, ui, args);
            }
            line if line.starts_with("/fix") => {
                let args = line.strip_prefix("/fix").unwrap_or("").trim();
                handle_fix(self, ui, args);
            }
            "/git-worktree" => handle_git_worktree(self, ui),
            line if line.starts_with("/git-worktree ") => {
                ui.push_log("Usage: /git-worktree (takes no arguments)");
            }
            line if line.starts_with("/open ") => handle_open(self, line, ui),
            line if line.starts_with("/theme ") => handle_theme(line, ui),
            line if line.starts_with("/plan") => {
                let args = line.strip_prefix("/plan").unwrap_or("").trim();
                let session_id = self
                    .session_manager
                    .lock()
                    .unwrap()
                    .get_current_session_id()
                    .unwrap_or("default".to_string());

                if let Err(e) = crate::tui::commands::handlers::slash_commands::plan::handle_plan(
                    args,
                    &session_id,
                    ui,
                    &self.cfg,
                ) {
                    ui.push_log(format!("プランコマンド処理エラー: {}", e));
                }
            }
            _ => {
                // Rest of content moved to exec.rs
                // Default behavior enforces plan context (skip_plan = false)
                self.handle_dispatch_rest(line, ui, false);
            }
        }
    }

    fn get_custom_commands(&self) -> Vec<String> {
        let custom_commands = load_custom_commands(&self.cfg.project_root);
        custom_commands
            .keys()
            .map(|name| format!("/{}", name))
            .collect()
    }

    fn handle_internal_followup(&mut self, content: &str, ui: &mut TuiApp) {
        // Synthetic analysis turns bypass slash routing entirely so they can
        // never be mistaken for typed user input.
        if self.ui_tx.is_none() {
            self.ui_tx = ui.sender();
        }
        let _ = self.spawn_internal_followup(ui, content, content.to_string());
    }

    fn handle_retry_turn(
        &mut self,
        display: &str,
        content: &str,
        directive_id: Option<String>,
        ui: &mut TuiApp,
    ) {
        if self.ui_tx.is_none() {
            self.ui_tx = ui.sender();
        }
        let _ = self.spawn_retry_turn(ui, display, content.to_string(), directive_id);
    }

    fn handle_augmented_user_prompt(&mut self, raw: &str, effective: &str, ui: &mut TuiApp) {
        if self.ui_tx.is_none() {
            self.ui_tx = ui.sender();
        }
        let _ = self.spawn_augmented_user_prompt(ui, raw, effective.to_string(), false);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn handle_job_completed(&mut self, producer: &str, ui: &mut TuiApp) {
        if let Some(id) = crate::jobs::JobId::parse_arg(producer)
            && let Some(job) = self.jobs.get_snapshot(id)
            && job.status.is_terminal()
            && let Some(draft) = ui.review_feedback.as_mut().filter(|d| d.job_id == Some(id))
        {
            if job.status == crate::jobs::JobStatus::Failed {
                draft.outcome = Some(format!(
                    "Failed: {}. {}",
                    job.error.as_deref().unwrap_or("Repair job failed"),
                    draft.outcome.as_deref().unwrap_or("Comments retained.")
                ));
            } else if draft.outcome.is_none() {
                draft.outcome = Some(format!(
                    "Repair job {}: {}. Comments retained; inspect the full resulting diff.",
                    id, job.status
                ));
            }
            ui.dirty = true;
            if ui.latest_agent_job_id == Some(id)
                && let Some(payload) = self.tools.finish_review_for_job(id)
            {
                let review = crate::tui::diff_review::DiffReviewState::from_payload(
                    crate::tools::provenance::enrich_diff_review_with_evidence(
                        &self.tools,
                        payload,
                    ),
                );
                if ui.diff_review.as_ref().and_then(|r| r.review_id.as_ref())
                    != review.review_id.as_ref()
                {
                    ui.feedback_review_arrived(&review);
                    ui.diff_review = Some(review);
                }
            }
        }
        if !self.handle_evidence_completed(producer, ui)
            && !self.handle_compact_completed(producer, ui)
        {
            if let Some(id) = crate::jobs::JobId::parse_arg(producer)
                && let Some(job) = self.jobs.get_snapshot(id)
                && job.kind == crate::jobs::JobKind::DiffReject
                && job.status.is_terminal()
                && let Some(review) = ui
                    .diff_review
                    .as_ref()
                    .filter(|r| r.rejecting && r.reject_job_id == Some(id))
                && let Some(review_id) = review.review_id.clone()
                && let Some(report) = self.tools.interrupted_review_report(&review_id, id)
            {
                let payload = self.tools.review_payload(&review_id);
                ui.apply_review_report(report, payload);
            }
            if let Some(id) = crate::jobs::JobId::parse_arg(producer)
                && let Some(job) = self.jobs.get_snapshot(id)
                && job.status.is_terminal()
                && self.jobs.foreground_id().is_none()
            {
                ui.status = if job.status == crate::jobs::JobStatus::Failed {
                    crate::tui::state::Status::Error
                } else {
                    crate::tui::state::Status::Ready
                };
                ui.detailed_status = None;
                if let Some(started) = ui.processing_start_time.take() {
                    ui.last_elapsed_time = Some(crate::jobs::types::format_elapsed(
                        started.elapsed().as_millis(),
                    ));
                }
            }
            self.handle_deferred_followup(producer, ui);
        }
    }
}

impl TuiExecutor {
    /// Display help for custom commands
    pub fn display_custom_commands_help(&self, ui: &mut TuiApp) {
        let custom_commands = load_custom_commands(&self.cfg.project_root);

        if custom_commands.is_empty() {
            ui.push_log("  (No custom commands found)");
        } else {
            for (name, command) in custom_commands {
                let scope_str = match command.scope {
                    CommandScope::Project => {
                        if let Some(namespace) = &command.namespace {
                            format!("(project:{})", namespace)
                        } else {
                            "(project)".to_string()
                        }
                    }
                    CommandScope::User => "(user)".to_string(),
                };
                ui.push_log(format!(
                    "  /{} - {} {}",
                    name, command.description, scope_str
                ));
            }
        }
    }
}

/// Command scope (project or user)
#[derive(Debug, Clone)]
pub enum CommandScope {
    Project,
    User,
}
