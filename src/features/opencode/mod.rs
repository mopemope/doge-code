//! API-key OpenCode routes. Authentication is independent of the wire protocol.
use crate::features::openai_subscription::ProviderKind;
use crate::llm::capabilities::ApiKind;
use anyhow::{Result, bail};

mod catalog;
pub use catalog::{catalog, lookup, print_catalog};

pub const GO_BASE: &str = "https://opencode.ai/zen/go/v1";
pub const ZEN_BASE: &str = "https://opencode.ai/zen/v1";

pub fn provider_name(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::OpencodeGo => "opencode-go",
        ProviderKind::OpencodeZen => "opencode-zen",
        ProviderKind::Openai => "openai",
        ProviderKind::OpenaiCompatible => "openai-compatible",
    }
}

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
    let spec = lookup(provider, model)?;
    spec.api.adapter().ok_or_else(|| {
        anyhow::anyhow!(
            "OpenCode model {} uses {}: {}",
            spec.id,
            spec.api.name(),
            spec.api.reason()
        )
    })
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
