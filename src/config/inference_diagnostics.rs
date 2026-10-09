//! Pure diagnostics: no credential/config loading, network calls or history mutation.
use super::app::FALLBACK_CONTEXT_WINDOW;
use super::{AppConfig, ReasoningMode};
use crate::features::{openai_subscription::ProviderKind, opencode};
use crate::llm::capabilities::{self, ApiKind, ReasoningSupport};

impl AppConfig {
    pub fn inference_label(&self) -> String {
        let auth = if self.provider == ProviderKind::Openai {
            "OAuth"
        } else if self
            .api_key
            .as_ref()
            .is_some_and(|key| !key.trim().is_empty())
        {
            "key set"
        } else {
            "NO KEY"
        };
        format!(
            "{} / {} / {auth}",
            opencode::provider_name(self.provider),
            safe_text(&self.model)
        )
    }

    pub fn inference_diagnostics(&self) -> Vec<String> {
        let api = match self.provider {
            ProviderKind::OpenaiCompatible => Some(ApiKind::ChatCompletions),
            ProviderKind::Openai => Some(ApiKind::Responses),
            provider => opencode::api(provider, &self.model).ok(),
        };
        // Custom URL paths/query/userinfo may contain credentials: display host only.
        let endpoint = opencode::default_base(self.provider)
            .map(str::to_owned)
            .unwrap_or_else(|| {
                if self.provider == ProviderKind::Openai {
                    return "ChatGPT OAuth endpoint".into();
                }
                reqwest::Url::parse(&self.base_url)
                    .ok()
                    .and_then(|url| {
                        url.host_str()
                            .map(|host| format!("{host} (URL path omitted)"))
                    })
                    .unwrap_or_else(|| "custom endpoint (invalid URL)".into())
            });
        let mut lines = vec![format!(
            "Connection: {} | Endpoint: {} | API: {}",
            self.inference_label(),
            endpoint,
            match api {
                Some(ApiKind::ChatCompletions) => "Chat Completions",
                Some(ApiKind::Responses) => "Responses",
                None => "unknown/unsupported",
            }
        )];
        if self.provider != ProviderKind::Openai
            && self
                .api_key
                .as_ref()
                .is_none_or(|key| key.trim().is_empty())
        {
            lines.push(opencode::missing_auth_message(self.provider).into());
        }
        let capacity = match (self.llm.context_window_size, self.get_context_window_size()) {
            (Some(size), _) => {
                format!("{size} tokens (manual override; not verified provider capacity)")
            }
            (_, Some(size)) => format!("{size} tokens (known metadata for selected route/model)"),
            _ => "unknown (no verified capacity for selected route/model)".into(),
        };
        lines.push(format!("Context capacity: {capacity}"));
        let basis = self
            .get_context_window_size()
            .unwrap_or(FALLBACK_CONTEXT_WINDOW);
        lines.push(format!("Compaction: effective {} tokens = min(configured {}, 80% of {}{}); request estimates are approximate",
            self.get_effective_compaction_limit(), self.auto_compact_prompt_token_threshold_for_current_model(), basis,
            if self.get_context_window_size().is_none() { " fallback estimate; NOT model capacity" } else { " context capacity" }));
        let caps = api.map(|api| {
            capabilities::resolve(self.provider, &self.base_url, api, &self.model, true)
        });
        let support = caps
            .map(|caps| caps.reasoning)
            .unwrap_or(ReasoningSupport::Unknown);
        let behavior = if caps.is_some_and(|caps| caps.chat_tools_require_reasoning_none) {
            "tool requests force reasoning_effort=none for this adapter".into()
        } else {
            match self.reasoning.mode {
                ReasoningMode::Off => "no reasoning effort hint; provider default (thinking is NOT guaranteed disabled)".into(),
                ReasoningMode::Fixed => format!("main-agent hint={} (explicit override; support metadata {:?}; provider acceptance not guaranteed)", self.reasoning.fixed_effort.as_api_str(), support),
                ReasoningMode::Auto if support == ReasoningSupport::Supported => format!("adaptive main-agent hints (initial {}, routine {}, deliberative {}, recovery {})",
                    self.reasoning.initial_effort.as_api_str(), self.reasoning.routine_effort.as_api_str(), self.reasoning.deliberative_effort.as_api_str(), self.reasoning.recovery_effort.as_api_str()),
                ReasoningMode::Auto => format!("no effort hint; provider default (support {:?})", support),
            }
        };
        lines.push(format!(
            "Reasoning: mode={} | {}; actual provider thinking is not measured here",
            self.reasoning.mode.as_str(),
            behavior
        ));
        lines.push("Auxiliary requests may omit reasoning hints; usage telemetry is separate from configured effort.".into());
        lines
    }
}

fn safe_text(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control())
        .take(120)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostics_unknown_manual_and_threshold_share_runtime_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = AppConfig {
            provider: ProviderKind::OpencodeGo,
            base_url: opencode::GO_BASE.into(),
            model: "gpt-6-luna".into(),
            project_root: dir.path().into(),
            ..Default::default()
        };
        assert_eq!(cfg.get_context_window_size(), None);
        assert_eq!(cfg.get_effective_compaction_limit(), 102_400);
        let output = cfg.inference_diagnostics().join("\n");
        assert!(output.contains("Context capacity: unknown"));
        assert!(output.contains("128000 fallback estimate; NOT model capacity"));
        assert!(output.contains("NO KEY"));
        assert!(output.contains("adaptive main-agent hints"));
        cfg.llm.context_window_size = Some(20_000);
        cfg.auto_compact_prompt_token_threshold_overrides
            .insert(cfg.model.clone(), 9000);
        assert_eq!(cfg.get_effective_compaction_limit(), 9000);
        let output = cfg.inference_diagnostics().join("\n");
        assert!(output.contains("manual override"));
        assert!(output.contains("effective 9000 tokens"));
        assert!(!output.contains("fallback estimate"));
    }
    #[test]
    fn diagnostics_do_not_expose_keys_or_custom_url_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = AppConfig {
            project_root: dir.path().into(),
            api_key: Some("SECRET_KEY".into()),
            base_url:
                "https://SECRET_USER:SECRET_PASS@example.invalid/SECRET_PATH?key=SECRET_QUERY"
                    .into(),
            model: "unknown\x1b[31m".into(),
            ..Default::default()
        };
        let output = cfg.inference_diagnostics().join("\n");
        assert!(!output.contains("SECRET"));
        assert!(!output.contains('\x1b'));
        assert!(output.contains("provider default"));
        assert!(output.contains("key set"));
    }
    #[test]
    fn diagnostics_reasoning_off_fixed_and_forced_none() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = AppConfig {
            project_root: dir.path().into(),
            base_url: "https://custom.invalid/v1".into(),
            model: "unknown".into(),
            ..Default::default()
        };
        cfg.reasoning.mode = ReasoningMode::Off;
        assert!(
            cfg.inference_diagnostics()
                .join("\n")
                .contains("NOT guaranteed disabled")
        );
        cfg.reasoning.mode = ReasoningMode::Fixed;
        assert!(
            cfg.inference_diagnostics()
                .join("\n")
                .contains("explicit override; support metadata Unknown")
        );
        cfg.base_url = "https://api.openai.com/v1".into();
        cfg.model = "gpt-6-luna".into();
        assert!(
            cfg.inference_diagnostics()
                .join("\n")
                .contains("force reasoning_effort=none")
        );
    }
}
