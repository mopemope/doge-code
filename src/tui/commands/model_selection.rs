use super::core::TuiExecutor;
use crate::{
    config::AppConfig,
    features::{model_selection, opencode, openrouter},
    llm::OpenAIClient,
    session::{SessionData, data::SessionModelSelection},
    tui::state::TuiApp,
};
use anyhow::Result;

/// Footer label for the active inference route. A ChatGPT subscription names
/// its account, which every selection change (startup, session switch, new
/// model session) must keep showing.
pub(crate) fn selection_label(cfg: &AppConfig, account: Option<&str>) -> String {
    match account {
        Some(account) => format!("openai | {account} | {}", cfg.model),
        None => cfg.inference_label(),
    }
}

impl TuiExecutor {
    pub(crate) fn ensure_model_selection_idle(&self, ui: &TuiApp) -> Result<()> {
        anyhow::ensure!(
            self.jobs.active_count() == 0 && ui.pending_instructions.is_empty(),
            "Wait for all jobs and queued instructions to finish before selecting a model"
        );
        anyhow::ensure!(
            ui.diff_review.is_none(),
            "Close the diff review before selecting a model"
        );
        Ok(())
    }
    pub(crate) fn prepare_session_selection(
        &self,
        session: &SessionData,
        startup: &AppConfig,
    ) -> Result<(AppConfig, Option<OpenAIClient>)> {
        let cfg =
            crate::session::selection::config_for_session(session, startup, &self.initial_model)?;
        let client = if model_selection::records_selection(cfg.provider) {
            // Validate/build before committing; keep process usage shared by existing clones.
            let mut ready = OpenAIClient::from_config(&cfg)?;
            if let (Some(next), Some(previous)) = (ready.as_mut(), self.client.as_ref()) {
                next.share_usage_from(previous);
            }
            ready
        } else {
            self.client.clone()
        };
        crate::session::selection::validate_binding(session, &cfg, client.as_ref())?;
        Ok((cfg, client))
    }
    pub(crate) fn apply_session_selection(
        &mut self,
        prepared: (AppConfig, Option<OpenAIClient>),
        id: &str,
    ) {
        self.cfg = prepared.0;
        self.client = prepared.1.map(|mut client| {
            client.opencode_session = id.into();
            client
        });
        self.tools.config = std::sync::Arc::new(self.cfg.clone());
    }
    pub(crate) fn sync_selection_ui(&self, ui: &mut TuiApp) {
        ui.model = Some(self.cfg.model.clone());
        ui.cfg = Some(self.cfg.clone());
        ui.inference_label = Some(selection_label(
            &self.cfg,
            self.client
                .as_ref()
                .and_then(|client| client.account_label()),
        ));
        ui.auto_compact_prompt_token_threshold = self.cfg.get_effective_compaction_limit();
        ui.tokens_used = 0;
        ui.tokens_prompt_used = 0;
        ui.update_remaining_context_tokens(self.cfg.get_context_window_size());
        ui.finalize_and_append_llm_response("");
        ui.last_llm_response_content = None;
        ui.last_elapsed_time = None;
    }
    pub(crate) fn start_model_session(
        &mut self,
        provider: crate::features::openai_subscription::ProviderKind,
        model: &str,
        ui: &mut TuiApp,
    ) -> Result<String> {
        self.ensure_model_selection_idle(ui)?;
        let (model, cfg) =
            if provider == crate::features::openai_subscription::ProviderKind::Openrouter {
                openrouter::model_id(model)?;
                let metadata = ui
                    .model_picker
                    .as_ref()
                    .and_then(|picker| {
                        picker
                            .entries
                            .iter()
                            .find(|entry| entry.provider == provider && entry.id == model)
                    })
                    .and_then(|entry| entry.metadata.clone());
                if let Some(picker) = ui.model_picker.as_ref() {
                    anyhow::ensure!(picker.can_select(), "Catalog is still loading");
                    if let Some(entry) = picker
                        .entries
                        .iter()
                        .find(|entry| entry.provider == provider && entry.id == model)
                    {
                        anyhow::ensure!(
                            entry.unavailable_reason.is_none(),
                            "{}",
                            entry.unavailable_reason.as_deref().unwrap_or("Unavailable")
                        );
                    }
                }
                let cfg = openrouter::selection_config(
                    &self.cfg,
                    model,
                    std::env::var("OPENROUTER_API_KEY").ok(),
                    metadata,
                )?;
                (model.to_owned(), cfg)
            } else {
                let spec = opencode::lookup(provider, model)?;
                opencode::api(provider, spec.id)?;
                (
                    spec.id.to_owned(),
                    opencode::selection_config(
                        &self.cfg,
                        provider,
                        spec.id,
                        opencode::key_store::available_key(),
                    )?,
                )
            };
        let selection = SessionModelSelection {
            provider,
            model: model.clone(),
        };
        let mut candidate = SessionData::new();
        candidate.model_selection = Some(selection.clone());
        let mut prepared = self.prepare_session_selection(&candidate, &cfg)?;
        if provider == crate::features::openai_subscription::ProviderKind::Openrouter {
            prepared.0.openrouter_metadata = cfg.openrouter_metadata.clone();
            if let Some(client) = prepared.1.as_mut() {
                client.openrouter_metadata = cfg.openrouter_metadata.clone();
            }
        }
        // Acquire every fallible lock before saving, then publish infallibly.
        let mut verification_queue =
            crate::utils::safe_std_lock(&self.verification_queue, "verification_queue")?;
        let mut history =
            crate::utils::safe_std_lock(&self.conversation_history, "conversation_history")?;
        let (id, outcome) = {
            let mut manager =
                crate::utils::safe_std_lock(&self.session_manager, "session_manager")?;
            manager.create_session_with_model(
                Some(format!(
                    "{}: {}",
                    model_selection::billing_label(provider),
                    model
                )),
                selection,
            )?
        };
        verification_queue.invalidate_selection();
        drop(verification_queue);
        history.clear();
        drop(history);
        self.apply_session_selection(prepared, &id);
        self.reset_session_input(ui);
        self.sync_selection_ui(ui);
        ui.plan_list.clear();
        self.send_plan_items_to_ui(&[]);
        if let crate::session::store::SessionSaveOutcome::DurabilityUnconfirmed { message } =
            outcome
        {
            ui.push_log(format!("Warning: {message}"));
        }
        if self.client.is_none() {
            ui.push_log(model_selection::missing_auth_message(self.cfg.provider));
        }
        ui.push_log(format!(
            "New session: {} / {} ({id})",
            model_selection::billing_label(provider),
            self.cfg.model
        ));
        Ok(id)
    }
}
