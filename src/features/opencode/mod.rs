//! API-key OpenCode routes. Authentication is independent of the wire protocol.
use crate::features::openai_subscription::ProviderKind;
use crate::llm::capabilities::ApiKind;
use anyhow::{Result, bail};

pub const GO_BASE: &str = "https://opencode.ai/zen/go/v1";
pub const ZEN_BASE: &str = "https://opencode.ai/zen/v1";

pub fn default_base(provider: ProviderKind) -> Option<&'static str> {
    match provider {
        ProviderKind::OpencodeGo => Some(GO_BASE),
        ProviderKind::OpencodeZen => Some(ZEN_BASE),
        _ => None,
    }
}

/// A legacy unscoped key/base/model must not silently migrate to a new
/// provider. Shared non-inference config remains provider-independent.
pub(crate) fn config_value<T>(
    provider: ProviderKind,
    source: Option<ProviderKind>,
    value: Option<T>,
) -> Option<T> {
    let scoped = default_base(provider).is_some()
        || source.is_some_and(|source| default_base(source).is_some());
    if !scoped || source == Some(provider) {
        value
    } else {
        None
    }
}

pub(crate) fn missing_auth_message(provider: ProviderKind) -> &'static str {
    if default_base(provider).is_some() {
        "OpenCode API key not set; use OPENCODE_API_KEY or --api-key for the selected provider."
    } else {
        "OPENAI_API_KEY not set; cannot call LLM."
    }
}

pub fn model_id(provider: ProviderKind, model: &str) -> Result<&str> {
    let prefix = match provider {
        ProviderKind::OpencodeGo => "opencode-go/",
        ProviderKind::OpencodeZen => "opencode/",
        _ => return Ok(model),
    };
    let id = model.strip_prefix(prefix).unwrap_or(model);
    if id.is_empty() || id.contains('/') {
        bail!("OpenCode model prefix does not match the selected provider");
    }
    Ok(id)
}

/// Verified endpoint table, not a family-prefix guess. Unknown IDs fail before
/// sending a key or accidentally selecting a billable alternative route.
/// Sources (2026-10-09): https://docs.opencode.ai/docs/go/ and /zen/.
pub fn api(provider: ProviderKind, model: &str) -> Result<ApiKind> {
    let id = model_id(provider, model)?;
    let common_chat = matches!(
        id,
        "glm-5.3-flash"
            | "glm-5.3"
            | "glm-5.2"
            | "deepseek-v4.1-flash"
            | "deepseek-v4-pro"
            | "deepseek-v4-flash"
            | "deepseek-v4-flash-vision-exp"
    );
    let supported = match provider {
        ProviderKind::OpencodeGo => match id {
            "grok-4.7"
            | "grok-4.6"
            | "gpt-6-luna"
            | "gpt-5.6-luna"
            | "muse-spark-1.3-contributor"
            | "muse-spark-1.2-contributor" => Some(ApiKind::Responses),
            "kimi-k3"
            | "kimi-k2.7-code"
            | "kimi-k2.6"
            | "longcat-2.0"
            | "longcat-2.5-preview-free"
            | "step-5-preview-free"
            | "mimo-v2.6-flash"
            | "mimo-v2.6-pro"
            | "mimo-v2.5"
            | "mimo-v2.5-pro"
            | "hy4-preview"
            | "hy3"
            | "space-bunny" => Some(ApiKind::ChatCompletions),
            _ if common_chat => Some(ApiKind::ChatCompletions),
            _ => None,
        },
        ProviderKind::OpencodeZen => match id {
            "gpt-6-astra"
            | "gpt-6-sol"
            | "gpt-6.1-sol"
            | "gpt-6-luna"
            | "gpt-5.6-sol"
            | "gpt-5.6-terra"
            | "gpt-5.6-luna"
            | "gpt-5.5"
            | "gpt-5.5-pro"
            | "gpt-5.4"
            | "gpt-5.4-pro"
            | "gpt-5.4-mini"
            | "gpt-5.4-nano"
            | "gpt-5.3-codex"
            | "gpt-5.3-codex-spark"
            | "gpt-5.2"
            | "gpt-5.2-codex"
            | "gpt-5.1"
            | "gpt-5.1-codex"
            | "gpt-5.1-codex-max"
            | "gpt-5.1-codex-mini"
            | "gpt-5"
            | "gpt-5-codex"
            | "gpt-5-nano"
            | "grok-4.7"
            | "grok-4.6"
            | "grok-4.5"
            | "grok-build-0.1"
            | "muse-spark-1.3"
            | "muse-spark-1.2"
            | "muse-spark-1.3-contributor-free" => Some(ApiKind::Responses),
            "qwen3.8-max"
            | "minimax-m3"
            | "minimax-m2.7"
            | "minimax-m2.5"
            | "glm-5.1"
            | "glm-5"
            | "kimi-k2.5"
            | "kimi-k2.6"
            | "kimi-k2.7-code"
            | "kimi-k3"
            | "mistral-large-4"
            | "big-pickle"
            | "space-bunny-free"
            | "longcat-2.5-preview-free"
            | "step-5-preview-free"
            | "exo-free"
            | "mimo-v2.6-flash-free"
            | "mimo-v2.5-free"
            | "ling-3.1-flash-free"
            | "ling-3.0-flash-fin-free"
            | "nemotron-3-ultra-free"
            | "nemotron-3.5-lightning-free" => Some(ApiKind::ChatCompletions),
            _ if common_chat => Some(ApiKind::ChatCompletions),
            _ => None,
        },
        _ => bail!("not an OpenCode provider"),
    };
    supported.ok_or_else(|| anyhow::anyhow!(
        "OpenCode model {id} has an unsupported or unknown API; only verified Chat Completions and Responses routes are supported (Messages/Google are unavailable)"))
}

pub fn validate_base(provider: ProviderKind, base: &str) -> Result<()> {
    if let Some(expected) = default_base(provider) {
        anyhow::ensure!(
            base.trim_end_matches('/') == expected,
            "OpenCode provider requires its own base URL: {expected}; no Go/Zen fallback is performed"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
