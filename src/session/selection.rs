//! Validate a persisted selection before publishing a session or initializing
//! its client. Shared by CLI resume and TUI transitions.
use crate::{config::AppConfig, features::opencode, llm::OpenAIClient, session::SessionData};
use anyhow::Result;

/// Guidance uses explicit checkpoint metadata only. Bindings are opaque and
/// can contain account identity; never expose them or infer a legacy model.
fn recovery_hint(session: &SessionData) -> String {
    let selection = match &session.model_selection {
        Some(selection) => format!(
            "Recorded provider/model: {} / {:?}. Use --provider={} and --model=<recorded-model> with the original account.",
            opencode::provider_name(selection.provider),
            selection.model,
            opencode::provider_name(selection.provider),
        ),
        None => "Provider/model selection was not recorded (legacy). Use the original account, --provider and --model from your startup configuration; no model is inferred from the binding.".into(),
    };
    format!(
        "Recovery: inspect checkpoint {:?} with `dgc session show <id>`. {selection} Resume with `dgc --provider=<provider> --model=<model> --resume=<id>` (TUI) or `dgc --provider=<provider> --model=<model> exec --resume=<id> <instruction>` (CLI). If the original selection is unavailable, start a new session without --resume.",
        session.meta.id,
    )
}

fn with_recovery(error: anyhow::Error, session: &SessionData) -> anyhow::Error {
    // TUI renders Display, while CLI renders the complete error chain. Keep
    // the original reason visible in both without losing the typed cause.
    let message = format!("{error}. {}", recovery_hint(session));
    error.context(message)
}

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
            "Session uses another provider; restart with its original provider (no billing route change). {}",
            recovery_hint(session)
        );
        cfg.model = opencode::model_id(selection.provider, &selection.model)
            .map_err(|error| with_recovery(error, session))?
            .into();
        opencode::api(selection.provider, &cfg.model)
            .map_err(|error| with_recovery(error, session))?;
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
            opencode::api(cfg.provider, &cfg.model)
                .map_err(|error| with_recovery(error, session))?,
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
            "Session belongs to another inference account/provider/model; restore its original selection. {}",
            recovery_hint(session)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        features::openai_subscription::ProviderKind, session::data::SessionModelSelection,
    };

    #[test]
    fn session_selection_recovery_reports_recorded_flags_without_binding_or_key() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = AppConfig {
            project_root: temp.path().into(),
            provider: ProviderKind::OpencodeGo,
            base_url: opencode::GO_BASE.into(),
            model: "gpt-6-luna".into(),
            api_key: Some("SYNTHETIC_KEY_NEVER_PRINT".into()),
            ..Default::default()
        };
        let mut data = SessionData::new();
        data.inference_binding = Some("ACCOUNT_BINDING_NEVER_PRINT".into());
        data.model_selection = Some(SessionModelSelection {
            provider: ProviderKind::OpencodeZen,
            model: "glm-5.3".into(),
        });
        let error = config_for_session(&data, &cfg, &cfg.model).unwrap_err();
        let text = format!("{error:#}");
        assert!(text.contains("another provider"));
        assert!(text.contains("opencode-zen / \"glm-5.3\""));
        assert!(text.contains("--provider=opencode-zen"));
        assert!(text.contains("--model=<recorded-model>"));
        assert!(text.contains("dgc session show <id>") && text.contains(&data.meta.id));
        assert!(text.contains("exec --resume=<id>") && text.contains("(TUI)"));
        assert!(
            !text.contains("ACCOUNT_BINDING_NEVER_PRINT")
                && !text.contains("SYNTHETIC_KEY_NEVER_PRINT")
        );

        data.model_selection.as_mut().unwrap().provider = ProviderKind::OpencodeGo;
        let error = config_for_session(&data, &cfg, &cfg.model).unwrap_err();
        assert!(format!("{error:#}").contains("another inference account/provider/model"));
        assert!(format!("{error:#}").contains("original account"));
        data.model_selection.as_mut().unwrap().model = "unknown-model".into();
        let error = config_for_session(&data, &cfg, &cfg.model).unwrap_err();
        assert!(format!("{error:#}").contains("Recovery:"));
        assert!(format!("{error:#}").contains("unknown-model"));
        assert!(error.to_string().contains("Unknown OpenCode model"));
        assert!(error.to_string().contains("Recovery:"));
    }

    #[test]
    fn session_selection_recovery_legacy_does_not_infer_model_or_rewrite_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = AppConfig {
            project_root: temp.path().into(),
            provider: ProviderKind::OpencodeGo,
            base_url: opencode::GO_BASE.into(),
            model: "gpt-6-luna".into(),
            api_key: None,
            ..Default::default()
        };
        let mut data = SessionData::new();
        data.inference_binding = Some(
            "OpencodeGo:https://opencode.ai/zen/go/v1:ChatCompletions:GLM_BINDING_NEVER_INFER"
                .into(),
        );
        let before = serde_json::to_value(&data).unwrap();
        let error = config_for_session(&data, &cfg, &cfg.model).unwrap_err();
        let text = format!("{error:#}");
        assert!(text.contains("not recorded (legacy)"));
        assert!(text.contains("no model is inferred"));
        assert!(text.contains("original account, --provider and --model"));
        assert!(text.contains("new session without --resume"));
        assert!(!text.contains("GLM_BINDING_NEVER_INFER"));
        assert_eq!(serde_json::to_value(&data).unwrap(), before);
        assert!(
            crate::session::format::format_detail(&data)
                .contains("legacy (startup selection not recorded)")
        );
    }
}
