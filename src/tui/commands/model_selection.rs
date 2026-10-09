use super::core::TuiExecutor;
use crate::{
    config::AppConfig,
    features::opencode,
    llm::OpenAIClient,
    session::{SessionData, data::SessionModelSelection},
    tui::state::TuiApp,
};
use anyhow::Result;

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
    ) -> Result<(AppConfig, Option<OpenAIClient>)> {
        let cfg =
            crate::session::selection::config_for_session(session, &self.cfg, &self.initial_model)?;
        let client = if opencode::default_base(cfg.provider).is_some() {
            // Validate/build before committing; keep process usage shared by existing clones.
            let ready = OpenAIClient::from_config(&cfg)?;
            self.client.clone().or(ready)
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
        if opencode::default_base(self.cfg.provider).is_some() {
            ui.inference_label = Some(self.cfg.inference_label());
        }
        ui.auto_compact_prompt_token_threshold = self.cfg.get_effective_compaction_limit();
        ui.tokens_used = 0;
        ui.tokens_prompt_used = 0;
        ui.update_remaining_context_tokens(self.cfg.get_context_window_size());
        ui.last_llm_response_content = None;
        ui.last_elapsed_time = None;
    }
    pub(crate) fn start_model_session(&mut self, model: &str, ui: &mut TuiApp) -> Result<String> {
        self.ensure_model_selection_idle(ui)?;
        let model = opencode::lookup(self.cfg.provider, model)?;
        opencode::api(self.cfg.provider, model.id)?;
        let selection = SessionModelSelection {
            provider: self.cfg.provider,
            model: model.id.into(),
        };
        let mut candidate = SessionData::new();
        candidate.model_selection = Some(selection.clone());
        let prepared = self.prepare_session_selection(&candidate)?;
        // Acquire every fallible lock before saving, then publish infallibly.
        let mut history =
            crate::utils::safe_std_lock(&self.conversation_history, "conversation_history")?;
        let (id, outcome) = {
            let mut manager =
                crate::utils::safe_std_lock(&self.session_manager, "session_manager")?;
            manager.create_session_with_model(Some(format!("Model: {}", model.id)), selection)?
        };
        history.clear();
        drop(history);
        self.apply_session_selection(prepared, &id);
        self.reset_session_input(ui);
        self.sync_selection_ui(ui);
        ui.plan_list.clear();
        self.send_plan_items_to_ui(&[]);
        for line in self.cfg.inference_diagnostics() {
            ui.push_log(line);
        }
        if let crate::session::store::SessionSaveOutcome::DurabilityUnconfirmed { message } =
            outcome
        {
            ui.push_log(format!("Warning: {message}"));
        }
        ui.push_log(format!("New model session: {} ({id})", self.cfg.model));
        Ok(id)
    }
}
