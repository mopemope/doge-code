//! API-key OpenCode routes. Authentication is independent of the wire protocol.
use crate::features::openai_subscription::ProviderKind;
use crate::llm::capabilities::ApiKind;
use anyhow::{Result, bail};

pub(crate) mod catalog;
pub mod key_store;
pub use catalog::{catalog, lookup, print_catalog};

pub const GO_BASE: &str = "https://opencode.ai/inference/go/v1";
pub const ZEN_BASE: &str = "https://opencode.ai/inference/v1";

fn legacy_base(provider: ProviderKind) -> Option<&'static str> {
    match provider {
        ProviderKind::OpencodeGo => Some("https://opencode.ai/zen/go/v1"),
        ProviderKind::OpencodeZen => Some("https://opencode.ai/zen/v1"),
        _ => None,
    }
}

/// Keep existing checkpoint and opaque Responses identities stable across
/// official URL aliases. Go and Zen identities always remain distinct.
pub(crate) fn identity_base(provider: ProviderKind, base: &str) -> &str {
    if let Some(legacy) = legacy_base(provider)
        && validate_base(provider, base).is_ok()
    {
        legacy
    } else {
        base.trim_end_matches('/')
    }
}

pub(crate) fn billing_label(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::OpencodeGo => "Go (subscription)",
        ProviderKind::OpencodeZen => "Zen (pay-as-you-go)",
        _ => provider_name(provider),
    }
}

pub(crate) fn auth_guidance(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::OpencodeGo => {
            "OpenCode Go: check OPENCODE_API_KEY and your Go subscription in the OpenCode Console. Run `dgc auth login opencode` (or export OPENCODE_API_KEY) and restart dgc. No Zen fallback is used."
        }
        ProviderKind::OpencodeZen => {
            "OpenCode Zen: check OPENCODE_API_KEY and your Zen billing/access in the OpenCode Console. Run `dgc auth login opencode` (or export OPENCODE_API_KEY) and restart dgc. No Go fallback is used."
        }
        _ => "Check the selected provider API key and access.",
    }
}

pub(crate) fn access_guidance(
    provider: ProviderKind,
    status: u16,
    error_type: Option<&str>,
) -> &'static str {
    if is_free_tier_rejection(provider, status, error_type) {
        "OpenCode rejected this request under its free-tier access conditions (FreeTierError). Free models may be restricted to use within OpenCode. Check the current conditions in the OpenCode Console, then explicitly select another model if needed. No automatic retry, model change or Go/Zen switch was made."
    } else {
        auth_guidance(provider)
    }
}

pub(crate) fn is_free_tier_rejection(
    provider: ProviderKind,
    status: u16,
    error_type: Option<&str>,
) -> bool {
    default_base(provider).is_some() && status == 403 && error_type == Some("FreeTierError")
}

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
    match provider {
        ProviderKind::OpencodeGo => {
            "OpenCode Go API key not set. Run `dgc auth login opencode` (or export OPENCODE_API_KEY), then start dgc --provider go. If you already logged in, `dgc auth status opencode` shows why the stored key was ignored."
        }
        ProviderKind::OpencodeZen => {
            "OpenCode Zen API key not set. Run `dgc auth login opencode` (or export OPENCODE_API_KEY), then start dgc --provider zen. If you already logged in, `dgc auth status opencode` shows why the stored key was ignored."
        }
        _ => {
            "OPENAI_API_KEY not set; cannot call LLM. For OpenCode, run `dgc auth login opencode` and choose --provider go or --provider zen."
        }
    }
}

/// An explicit new-session choice can share a selected OpenCode key between
/// Go and Zen. It never carries credentials from another provider.
pub(crate) fn selection_config(
    current: &crate::config::AppConfig,
    provider: ProviderKind,
    model: &str,
    environment_key: Option<String>,
) -> Result<crate::config::AppConfig> {
    let base = default_base(provider).ok_or_else(|| anyhow::anyhow!("Choose Go or Zen"))?;
    let mut cfg = current.clone();
    if default_base(current.provider).is_some() {
        validate_base(current.provider, &current.base_url)?;
        // A session started without a key picks up one stored since (login in
        // another shell); an existing key is never replaced.
        if cfg
            .api_key
            .as_deref()
            .is_none_or(|key| key.trim().is_empty())
        {
            cfg.api_key = environment_key;
            cfg.api_key_origin = cfg
                .api_key
                .as_ref()
                .map(|_| crate::config::ConfigOrigin::Environment);
        }
    } else {
        cfg.api_key = environment_key;
        cfg.api_key_origin = cfg
            .api_key
            .as_ref()
            .map(|_| crate::config::ConfigOrigin::Environment);
    }
    cfg.api_key = cfg.api_key.filter(|key| !key.trim().is_empty());
    if cfg.api_key.is_none() {
        cfg.api_key_origin = None;
    }
    cfg.provider = provider;
    cfg.provider_origin = crate::config::ConfigOrigin::Session;
    cfg.base_url = base.into();
    cfg.base_url_origin = crate::config::ConfigOrigin::Session;
    cfg.model = model.into();
    cfg.resume = None;
    Ok(cfg)
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
/// Sources (2026-10-17): https://docs.opencode.ai/docs/go/ and /zen/.
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
            base.trim_end_matches('/') == expected
                || Some(base.trim_end_matches('/')) == legacy_base(provider),
            "OpenCode provider requires its own base URL: {expected}; no Go/Zen fallback is performed"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
