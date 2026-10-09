//! Validate a persisted selection before publishing a session or initializing
//! its client. Shared by CLI resume and TUI transitions.
use crate::{config::AppConfig, features::opencode, llm::OpenAIClient, session::SessionData};
use anyhow::Result;

pub(crate) fn config_for_session(
    session: &SessionData,
    startup: &AppConfig,
    initial_model: &str,
) -> Result<AppConfig> {
    let mut cfg = startup.clone();
    cfg.model = initial_model.into();
    if let Some(selection) = &session.model_selection {
        anyhow::ensure!(
            selection.provider == cfg.provider,
            "Session uses another provider; restart with its original provider (no billing route change)"
        );
        cfg.model = opencode::model_id(selection.provider, &selection.model)?.into();
        opencode::api(selection.provider, &cfg.model)?;
    }
    // OpenCode bindings need no key or account lookup. Reject them before
    // client construction, including keyless startup.
    validate_binding(session, &cfg, None)?;
    Ok(cfg)
}

pub(crate) fn validate_binding(
    session: &SessionData,
    cfg: &AppConfig,
    client: Option<&OpenAIClient>,
) -> Result<()> {
    let expected = if opencode::default_base(cfg.provider).is_some() {
        Some(format!(
            "{:?}:{}:{:?}:{}",
            cfg.provider,
            cfg.base_url.trim_end_matches('/'),
            opencode::api(cfg.provider, &cfg.model)?,
            cfg.model
        ))
    } else {
        client
            .map(|client| client.inference_binding(&cfg.model))
            .transpose()?
    };
    if let (Some(binding), Some(expected)) = (&session.inference_binding, expected) {
        anyhow::ensure!(
            binding == &expected,
            "Session belongs to another inference account/provider/model; restore its original selection"
        );
    }
    Ok(())
}
