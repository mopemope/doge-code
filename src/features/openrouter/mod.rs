//! Dedicated OpenRouter route. Credentials and catalog never share other providers.
use crate::{config::AppConfig, features::openai_subscription::ProviderKind};
use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const BASE_URL: &str = "https://openrouter.ai/api/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Support {
    Supported,
    Unsupported,
    Unknown,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffortSupport {
    #[default]
    Unknown,
    GatewayAccepted,
    Listed(Vec<String>),
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningMetadata {
    pub supported_efforts: EffortSupport,
    pub mandatory: Option<bool>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogPrices {
    pub prompt: Option<String>,
    pub completion: Option<String>,
    pub request: Option<String>,
    pub conditional: Option<bool>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenRouterModel {
    pub id: String,
    pub name: Option<String>,
    pub input_modalities: Option<Vec<String>>,
    pub output_modalities: Option<Vec<String>>,
    pub supported_parameters: Option<Vec<String>>,
    pub context_window: Option<u32>,
    pub prices: CatalogPrices,
    pub reasoning: ReasoningMetadata,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedModelMetadata {
    pub provider: ProviderKind,
    pub fetched_at: DateTime<Utc>,
    pub model: OpenRouterModel,
}

pub fn validate_base(base: &str) -> Result<()> {
    let valid = url::Url::parse(base).ok().is_some_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("openrouter.ai")
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && matches!(url.path(), "/api/v1" | "/api/v1/")
    });
    ensure!(
        valid,
        "OpenRouter requires https://openrouter.ai/api/v1; use openai-compatible for custom endpoints"
    );
    Ok(())
}
pub fn model_id(model: &str) -> Result<&str> {
    ensure!(
        (1..=256).contains(&model.len())
            && model
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.~:/".contains(&b))
            && model
                .split_once('/')
                .is_some_and(|(publisher, name)| !publisher.is_empty() && !name.is_empty()),
        "Invalid OpenRouter model ID; use the complete publisher/model ID (1-256 ASCII bytes)"
    );
    Ok(model)
}
pub fn inference_binding(model: &str) -> Result<String> {
    Ok(format!(
        "Openrouter:{BASE_URL}:ChatCompletions:{}",
        model_id(model)?
    ))
}

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub struct DecodedResponse {
    pub message: crate::llm::ChoiceMessageWithTools,
    pub usage: Option<crate::llm::types::Usage>,
    pub prompt_tokens: Option<u32>,
}
pub mod catalog;
mod wire;
pub use wire::{adapt_request_value, decode_response, resolve_effort};

impl OpenRouterModel {
    pub fn text_support(&self) -> Support {
        support(self.output_modalities.as_deref(), "text")
    }
    pub fn tools_support(&self) -> Support {
        support(self.supported_parameters.as_deref(), "tools")
    }
}
fn support(values: Option<&[String]>, value: &str) -> Support {
    match values {
        None => Support::Unknown,
        Some(v) if v.iter().any(|s| s == value) => Support::Supported,
        Some(_) => Support::Unsupported,
    }
}
impl SelectedModelMetadata {
    pub fn matches(&self, model: &str) -> bool {
        self.provider == ProviderKind::Openrouter && self.model.id == model
    }
}
pub fn selection_config(
    current: &AppConfig,
    model: &str,
    environment_key: Option<String>,
    metadata: Option<Arc<SelectedModelMetadata>>,
) -> Result<AppConfig> {
    model_id(model)?;
    let mut cfg = current.clone();
    if current.provider == ProviderKind::Openrouter {
        validate_base(&current.base_url)?;
    } else {
        cfg.api_key = environment_key.filter(|key| !key.trim().is_empty());
        cfg.api_key_origin = cfg
            .api_key
            .as_ref()
            .map(|_| crate::config::ConfigOrigin::Environment);
    }
    cfg.api_key = cfg.api_key.filter(|key| !key.trim().is_empty());
    if cfg.api_key.is_none() {
        cfg.api_key_origin = None;
    }
    cfg.provider = ProviderKind::Openrouter;
    cfg.provider_origin = crate::config::ConfigOrigin::Session;
    cfg.base_url = BASE_URL.into();
    cfg.base_url_origin = crate::config::ConfigOrigin::Session;
    cfg.model = model.into();
    cfg.resume = None;
    cfg.openrouter_metadata = metadata.filter(|m| m.matches(model));
    Ok(cfg)
}
pub fn cached_metadata(model: &str) -> Option<Arc<SelectedModelMetadata>> {
    model_id(model).ok()?;
    let path = catalog::default_cache_path()?;
    catalog::read_cached(&path, Utc::now())
        .ok()??
        .snapshot
        .selected_metadata(model)
}
