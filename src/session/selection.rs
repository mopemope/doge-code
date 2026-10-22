//! Validate a persisted selection before publishing a session or initializing
//! its client. Shared by CLI resume and TUI transitions.
use crate::{
    config::AppConfig,
    features::{model_selection, openai_subscription::ProviderKind, opencode, openrouter},
    llm::OpenAIClient,
    session::SessionData,
};
use anyhow::Result;

/// Guidance uses explicit checkpoint metadata only. Bindings are opaque and
/// can contain account identity; never expose them or infer a legacy model.
fn recovery_hint(session: &SessionData) -> String {
    let selection = match &session.model_selection {
        Some(selection) => format!(
            "Recorded provider/model: {} / {:?}. Use --provider={} and --model=<recorded-model> with the original account.",
            model_selection::provider_name(selection.provider),
            selection.model,
            model_selection::provider_name(selection.provider),
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
        if selection.provider == ProviderKind::Openrouter {
            cfg.model = openrouter::model_id(&selection.model)
                .map_err(|error| with_recovery(error, session))?
                .into();
            openrouter::validate_base(&cfg.base_url)
                .map_err(|error| with_recovery(error, session))?;
        } else {
            cfg.model = opencode::model_id(selection.provider, &selection.model)
                .map_err(|error| with_recovery(error, session))?
                .into();
            opencode::api(selection.provider, &cfg.model)
                .map_err(|error| with_recovery(error, session))?;
        }
    }
    cfg.openrouter_metadata = if cfg.provider == ProviderKind::Openrouter {
        openrouter::cached_metadata(&cfg.model)
    } else {
        None
    };
    // OpenCode bindings need no key or account lookup. Reject them before
    // client construction, including keyless startup.
    validate_binding(session, &cfg, None)?;
    if let Some(identity) = cfg.api_key_responses_identity() {
        let messages = session
            .conversation_messages()
            .map_err(|error| with_recovery(error, session))?;
        crate::features::openai_subscription::responses::validate_api_key_history(
            &cfg.model, &identity, &messages,
        )
        .map_err(|error| with_recovery(error, session))?;
    }
    Ok(cfg)
}

pub(crate) fn validate_binding(
    session: &SessionData,
    cfg: &AppConfig,
    client: Option<&OpenAIClient>,
) -> Result<()> {
    let expected = if cfg.provider == ProviderKind::Openrouter {
        openrouter::validate_base(&cfg.base_url).map_err(|error| with_recovery(error, session))?;
        Some(
            openrouter::inference_binding(&cfg.model)
                .map_err(|error| with_recovery(error, session))?,
        )
    } else if opencode::default_base(cfg.provider).is_some() {
        Some(format!(
            "{:?}:{}:{:?}:{}",
            cfg.provider,
            opencode::identity_base(cfg.provider, &cfg.base_url),
            opencode::api(cfg.provider, &cfg.model)
                .map_err(|error| with_recovery(error, session))?,
            cfg.model
        ))
    } else if cfg.provider == crate::features::openai_subscription::ProviderKind::OpenaiCompatible {
        Some(
            cfg.api_key_responses_identity()
                .unwrap_or_else(|| "openai-compatible".into()),
        )
    } else {
        client
            .map(|client| client.inference_binding(&cfg.model))
            .transpose()?
    };
    if let (Some(binding), Some(expected)) = (&session.inference_binding, expected) {
        if binding == "openai-compatible" && cfg.api_key_responses_identity().is_some() {
            anyhow::bail!(
                "This session is bound to Chat Completions, while the default official OpenAI route uses Responses. Resume with --provider openai-compatible to keep using Chat Completions, or start a new Responses conversation."
            );
        }
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

    fn compatible_config(root: &std::path::Path) -> AppConfig {
        AppConfig {
            project_root: root.into(),
            api_key: Some("SYNTHETIC_KEY_NEVER_PRINT".into()),
            ..Default::default()
        }
    }

    fn user_message(content: &str) -> crate::llm::types::ChatMessage {
        crate::llm::types::ChatMessage {
            reasoning: Default::default(),
            provider_state: None,
            role: "user".into(),
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    #[test]
    fn old_chat_binding_is_rejected_with_explicit_resume_guidance_before_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = compatible_config(temp.path());
        let mut data = SessionData::new();
        data.inference_binding = Some("openai-compatible".into());
        data.replace_conversation_messages(&[user_message("preserve this history")])
            .unwrap();
        let before = serde_json::to_vec(&data).unwrap();

        let error = config_for_session(&data, &cfg, &cfg.model).unwrap_err();
        let text = format!("{error:#}");
        assert!(text.contains("Chat Completions"), "{text}");
        assert!(text.contains("--provider openai-compatible"), "{text}");
        assert!(
            text.contains("start a new Responses conversation"),
            "{text}"
        );
        assert!(!text.contains("SYNTHETIC_KEY_NEVER_PRINT"));
        assert_eq!(serde_json::to_vec(&data).unwrap(), before);

        let explicit_chat = AppConfig {
            provider_origin: crate::config::ConfigOrigin::Cli,
            ..cfg
        };
        assert_eq!(
            explicit_chat
                .resolved_api_for_model(&explicit_chat.model)
                .unwrap(),
            crate::llm::capabilities::ApiKind::ChatCompletions
        );
        config_for_session(&data, &explicit_chat, &explicit_chat.model)
            .expect("explicit compatible selection resumes its Chat Completions binding");
    }

    #[test]
    fn unbound_legacy_responses_preflight_accepts_text_tools_and_rejects_lost_state() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = compatible_config(temp.path());
        let mut valid = SessionData::new();
        valid
            .replace_conversation_messages(&[
                user_message("read a file"),
                crate::llm::types::ChatMessage {
                    reasoning: Default::default(),
                    provider_state: None,
                    role: "assistant".into(),
                    content: None,
                    tool_calls: vec![crate::llm::types::ToolCall {
                        id: Some("call-1".into()),
                        r#type: "function".into(),
                        function: crate::llm::types::ToolCallFunction {
                            name: "fs_read".into(),
                            arguments: "{}".into(),
                        },
                    }],
                    tool_call_id: None,
                },
                crate::llm::types::ChatMessage {
                    reasoning: Default::default(),
                    provider_state: None,
                    role: "tool".into(),
                    content: Some("file contents".into()),
                    tool_calls: Vec::new(),
                    tool_call_id: Some("call-1".into()),
                },
            ])
            .unwrap();
        config_for_session(&valid, &cfg, &cfg.model)
            .expect("canonical text/tool history can be projected");

        let mut lost_reasoning = SessionData::new();
        lost_reasoning
            .replace_conversation_messages(&[crate::llm::types::ChatMessage {
                reasoning: crate::llm::types::ReasoningPayload {
                    reasoning_content: Some("PRIVATE_REASONING_SENTINEL".into()),
                    reasoning_details: Some(
                        serde_json::json!({"opaque":"PRIVATE_DETAIL_SENTINEL"}),
                    ),
                },
                provider_state: None,
                role: "assistant".into(),
                content: Some("answer".into()),
                tool_calls: Vec::new(),
                tool_call_id: None,
            }])
            .unwrap();
        let before = serde_json::to_vec(&lost_reasoning).unwrap();
        let error = config_for_session(&lost_reasoning, &cfg, &cfg.model).unwrap_err();
        let text = format!("{error:#}");
        assert!(text.contains("reasoning fields"), "{text}");
        assert!(!text.contains("PRIVATE_REASONING_SENTINEL"));
        assert!(!text.contains("PRIVATE_DETAIL_SENTINEL"));
        assert_eq!(serde_json::to_vec(&lost_reasoning).unwrap(), before);
    }

    #[test]
    fn responses_preflight_rejects_invalid_pairs_and_foreign_provider_state() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = compatible_config(temp.path());
        let mut malformed = SessionData::new();
        malformed
            .replace_conversation_messages(&[
                crate::llm::types::ChatMessage {
                    reasoning: Default::default(),
                    provider_state: None,
                    role: "assistant".into(),
                    content: None,
                    tool_calls: vec![crate::llm::types::ToolCall {
                        id: Some("duplicate".into()),
                        r#type: "function".into(),
                        function: crate::llm::types::ToolCallFunction {
                            name: "fs_read".into(),
                            arguments: "{}".into(),
                        },
                    }],
                    tool_call_id: None,
                },
                crate::llm::types::ChatMessage {
                    reasoning: Default::default(),
                    provider_state: None,
                    role: "tool".into(),
                    content: Some("result".into()),
                    tool_calls: Vec::new(),
                    tool_call_id: Some("wrong-id".into()),
                },
            ])
            .unwrap();
        assert!(config_for_session(&malformed, &cfg, &cfg.model).is_err());

        let mut foreign_state = SessionData::new();
        foreign_state
            .replace_conversation_messages(&[crate::llm::types::ChatMessage {
                reasoning: Default::default(),
                provider_state: Some(crate::features::openai_subscription::ProviderState {
                    version: 1,
                    account: "FOREIGN_ACCOUNT_SENTINEL".into(),
                    model: cfg.model.clone(),
                    output: vec![serde_json::json!({
                        "type":"reasoning", "encrypted_content":"OPAQUE_STATE_SENTINEL"
                    })],
                    additional_tool_names: Vec::new(),
                }),
                role: "assistant".into(),
                content: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            }])
            .unwrap();
        let before = serde_json::to_vec(&foreign_state).unwrap();
        let error = config_for_session(&foreign_state, &cfg, &cfg.model).unwrap_err();
        let text = format!("{error:#}");
        assert!(text.contains("different account or model"), "{text}");
        assert!(!text.contains("FOREIGN_ACCOUNT_SENTINEL"));
        assert!(!text.contains("OPAQUE_STATE_SENTINEL"));
        assert_eq!(serde_json::to_vec(&foreign_state).unwrap(), before);

        let identity = cfg.api_key_responses_identity().unwrap();
        for (version, account, model, role) in [
            (2, identity.as_str(), cfg.model.as_str(), "assistant"),
            (1, identity.as_str(), "different-model", "assistant"),
            (1, identity.as_str(), cfg.model.as_str(), "user"),
        ] {
            let mut invalid = SessionData::new();
            invalid
                .replace_conversation_messages(&[crate::llm::types::ChatMessage {
                    reasoning: Default::default(),
                    provider_state: Some(crate::features::openai_subscription::ProviderState {
                        version,
                        account: account.into(),
                        model: model.into(),
                        output: vec![serde_json::json!({
                            "type":"reasoning", "encrypted_content":"STATE_SENTINEL"
                        })],
                        additional_tool_names: Vec::new(),
                    }),
                    role: role.into(),
                    content: None,
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                }])
                .unwrap();
            let before = serde_json::to_vec(&invalid).unwrap();
            let error = config_for_session(&invalid, &cfg, &cfg.model).unwrap_err();
            let text = format!("{error:#}");
            assert!(
                text.contains("different account or model") || text.contains("provider state"),
                "{text}"
            );
            assert!(!text.contains("STATE_SENTINEL"));
            assert_eq!(serde_json::to_vec(&invalid).unwrap(), before);
        }
    }
}
