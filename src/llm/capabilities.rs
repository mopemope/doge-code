//! Local routing hints, not a model catalog. Unknown metadata stays unknown.
use crate::features::openai_subscription::ProviderKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKind {
    ChatCompletions,
    Responses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningSupport {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub reasoning: ReasoningSupport,
    pub chat_tools_require_reasoning_none: bool,
    pub context_window: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endpoint {
    Openai,
    Openrouter,
    Unknown,
}

fn endpoint(base_url: &str) -> Endpoint {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return Endpoint::Unknown;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return Endpoint::Unknown;
    }
    match url.host_str() {
        Some("api.openai.com") => Endpoint::Openai,
        Some("openrouter.ai") => Endpoint::Openrouter,
        _ => Endpoint::Unknown,
    }
}

pub fn resolve(
    provider: ProviderKind,
    base_url: &str,
    api: ApiKind,
    model: &str,
    has_tools: bool,
) -> Capabilities {
    let endpoint = endpoint(base_url);
    // Accept the existing openai/ alias, never an arbitrary vendor prefix.
    let openai_id = model.strip_prefix("openai/").unwrap_or(model);
    let openai_reasoning = match openai_id {
        // Existing routing IDs, not family-prefix or future-snapshot inference.
        "gpt-5-mini" | "gpt-6-luna" | "o3-mini" => ReasoningSupport::Supported,
        "gpt-4"
        | "gpt-4o"
        | "gpt-4o-mini"
        | "gpt-3.5"
        | "gpt-3.5-turbo"
        | "gpt-4.1-mini"
        | "gpt-4.1-mini-2025-04-14" => ReasoningSupport::Unsupported,
        _ => ReasoningSupport::Unknown,
    };
    let route_matches = matches!(
        (provider, api),
        (ProviderKind::OpenaiCompatible, ApiKind::ChatCompletions)
            | (ProviderKind::Openai, ApiKind::Responses)
    );
    let reasoning = if matches!(
        provider,
        ProviderKind::OpencodeGo | ProviderKind::OpencodeZen
    ) && api == ApiKind::Responses
        && model == "gpt-6-luna"
    {
        ReasoningSupport::Supported
    } else if !route_matches {
        ReasoningSupport::Unknown
    } else {
        match endpoint {
            Endpoint::Openai => openai_reasoning,
            // OpenRouter's existing request adapter forwards this parameter.
            // This says nothing about an individual model's context capacity.
            Endpoint::Openrouter if provider == ProviderKind::OpenaiCompatible => {
                ReasoningSupport::Supported
            }
            _ => ReasoningSupport::Unknown,
        }
    };
    let context_window =
        if provider == ProviderKind::OpenaiCompatible && api == ApiKind::ChatCompletions {
            match endpoint {
                Endpoint::Openai => openai_context(openai_id),
                Endpoint::Openrouter => match model.strip_prefix("openai/") {
                    Some(id) => openai_context(id),
                    None => match model {
                        "anthropic/claude-3-5-sonnet"
                        | "anthropic/claude-3-opus"
                        | "anthropic/claude-3-haiku" => Some(200_000),
                        "anthropic/claude-2" => Some(100_000),
                        "kwaipilot/kat-coder-pro" => Some(128_000),
                        "qwen/qwen3-coder" => Some(32_768),
                        "deepseek/deepseek-chat-v3.1" => Some(64_000),
                        _ => None,
                    },
                },
                Endpoint::Unknown => None,
            }
        } else {
            // Subscription slugs have no capacity metadata in the current catalog.
            None
        };
    Capabilities {
        reasoning,
        chat_tools_require_reasoning_none: provider == ProviderKind::OpenaiCompatible
            && endpoint == Endpoint::Openai
            && api == ApiKind::ChatCompletions
            && model == "gpt-6-luna"
            && has_tools,
        context_window,
    }
}

fn openai_context(id: &str) -> Option<u32> {
    match id {
        // Existing verified IDs and legacy exact IDs; no new model metadata.
        // 4.1-mini capacity was verified in the preceding change (2026-10-06):
        // https://developers.openai.com/api/docs/models/gpt-4.1-mini
        // https://openrouter.ai/api/v1/models
        "gpt-4.1-mini" | "gpt-4.1-mini-2025-04-14" => Some(1_047_576),
        "gpt-4o" | "gpt-4o-mini" => Some(128_000),
        "gpt-4" => Some(8_192),
        "gpt-3.5" | "gpt-3.5-turbo" => Some(16_385),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ReasoningEffort, ReasoningMode};
    use crate::llm::reasoning::resolve_hint_for_support;

    #[test]
    fn capability_route_model_and_tools_matrix_preserves_luna_boundary() {
        for (provider, url, api, model, tools, requires_none) in [
            (
                ProviderKind::OpenaiCompatible,
                "https://api.openai.com/v1",
                ApiKind::ChatCompletions,
                "gpt-6-luna",
                true,
                true,
            ),
            (
                ProviderKind::OpenaiCompatible,
                "https://api.openai.com/v1",
                ApiKind::ChatCompletions,
                "gpt-6-luna",
                false,
                false,
            ),
            (
                ProviderKind::Openai,
                "https://api.openai.com/v1",
                ApiKind::Responses,
                "gpt-6-luna",
                true,
                false,
            ),
            (
                ProviderKind::OpenaiCompatible,
                "https://openrouter.ai/api/v1",
                ApiKind::ChatCompletions,
                "gpt-6-luna",
                true,
                false,
            ),
            (
                ProviderKind::OpenaiCompatible,
                "https://custom.invalid/v1",
                ApiKind::ChatCompletions,
                "gpt-6-luna",
                true,
                false,
            ),
            (
                ProviderKind::OpenaiCompatible,
                "https://api.openai.com/v1",
                ApiKind::ChatCompletions,
                "gpt-5-mini",
                true,
                false,
            ),
        ] {
            let caps = resolve(provider, url, api, model, tools);
            assert_eq!(
                caps.chat_tools_require_reasoning_none, requires_none,
                "{provider:?} {url} {api:?} {model} {tools}"
            );
        }
        let native = resolve(
            ProviderKind::Openai,
            "https://api.openai.com/v1",
            ApiKind::Responses,
            "gpt-6-luna",
            true,
        );
        assert_eq!(
            resolve_hint_for_support(
                native.reasoning,
                &ReasoningMode::Auto,
                Some(ReasoningEffort::High)
            ),
            Some(ReasoningEffort::High)
        );
        assert_eq!(native.context_window, None);
        for (provider, api) in [
            (ProviderKind::Openai, ApiKind::ChatCompletions),
            (ProviderKind::OpenaiCompatible, ApiKind::Responses),
        ] {
            let caps = resolve(
                provider,
                "https://api.openai.com/v1",
                api,
                "gpt-6-luna",
                true,
            );
            assert_eq!(caps.reasoning, ReasoningSupport::Unknown);
            assert!(!caps.chat_tools_require_reasoning_none);
            assert_eq!(caps.context_window, None);
        }
    }

    #[test]
    fn capability_capacity_is_scoped_to_endpoint_and_model_identity() {
        for (url, model, capacity) in [
            ("https://api.openai.com/v1", "gpt-4.1-mini", Some(1_047_576)),
            (
                "https://API.OPENAI.COM/v1",
                "openai/gpt-4.1-mini-2025-04-14",
                Some(1_047_576),
            ),
            (
                "https://openrouter.ai/api/v1",
                "openai/gpt-4.1-mini",
                Some(1_047_576),
            ),
            ("https://openrouter.ai/api/v1", "gpt-4.1-mini", None),
            ("https://custom.invalid/v1", "openai/gpt-4.1-mini", None),
            ("https://api.openai.com/v1", "anthropic/claude-3-opus", None),
            (
                "https://openrouter.ai/api/v1",
                "anthropic/claude-3-opus",
                Some(200_000),
            ),
            ("https://openrouter.ai/api/v1", "unknown/gpt-4o", None),
        ] {
            assert_eq!(
                resolve(
                    ProviderKind::OpenaiCompatible,
                    url,
                    ApiKind::ChatCompletions,
                    model,
                    false
                )
                .context_window,
                capacity,
                "{url} {model}"
            );
        }
    }
}
