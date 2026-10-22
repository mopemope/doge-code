//! Owned model picker entries; catalog browsing never sends inference requests.
use crate::features::openrouter::catalog::{CatalogError, CatalogFreshness, CatalogResult};
use crate::features::{openai_subscription::ProviderKind, opencode, openrouter};
use anyhow::Result;
use std::sync::Arc;

pub(crate) fn config_value<T>(
    provider: ProviderKind,
    source: Option<ProviderKind>,
    value: Option<T>,
) -> Option<T> {
    if provider == ProviderKind::Openrouter || source == Some(ProviderKind::Openrouter) {
        if source == Some(provider) {
            value
        } else {
            None
        }
    } else {
        opencode::config_value(provider, source, value)
    }
}
pub fn provider_name(provider: ProviderKind) -> &'static str {
    if provider == ProviderKind::Openrouter {
        "openrouter"
    } else {
        opencode::provider_name(provider)
    }
}
pub(crate) fn records_selection(provider: ProviderKind) -> bool {
    provider == ProviderKind::Openrouter || opencode::default_base(provider).is_some()
}
pub(crate) fn missing_auth_message(provider: ProviderKind) -> &'static str {
    if provider == ProviderKind::Openrouter {
        "OpenRouter API key not set. Export OPENROUTER_API_KEY, then restart dgc. Catalog browsing and model selection do not require a key."
    } else {
        opencode::missing_auth_message(provider)
    }
}
pub(crate) fn billing_label(provider: ProviderKind) -> &'static str {
    if provider == ProviderKind::Openrouter {
        "OpenRouter (pay-as-you-go)"
    } else {
        opencode::billing_label(provider)
    }
}
#[derive(Debug, Clone)]
pub struct ModelPickerEntry {
    pub provider: ProviderKind,
    pub id: String,
    pub name: Option<String>,
    pub api_label: String,
    pub unavailable_reason: Option<String>,
    pub metadata: Option<Arc<openrouter::SelectedModelMetadata>>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelPickerState {
    ReadyFresh,
    LoadingWithoutSnapshot,
    RefreshingWithSnapshot,
    ReadyStale,
    FailedWithoutSnapshot,
}
#[derive(Debug, Clone)]
pub struct ModelPicker {
    pub provider: ProviderKind,
    pub query: String,
    pub selected: usize,
    pub error: Option<String>,
    pub key_available: bool,
    pub entries: Vec<ModelPickerEntry>,
    pub state: ModelPickerState,
    pub generation: u64,
    pub catalog_job: Option<crate::jobs::JobId>,
    pub fetched_at: Option<chrono::DateTime<chrono::Utc>>,
    pub offline: bool,
    pub excluded_entries: usize,
    pub preferred_model: Option<String>,
}
impl ModelPicker {
    pub fn new(provider: ProviderKind, query: &str) -> Result<Self> {
        anyhow::ensure!(
            records_selection(provider),
            "Use /provider go, zen or openrouter; dgc models --provider openai lists the OAuth account catalog"
        );
        let mut picker = Self {
            provider,
            query: query.into(),
            selected: 0,
            error: None,
            key_available: false,
            entries: Vec::new(),
            state: ModelPickerState::LoadingWithoutSnapshot,
            generation: 0,
            catalog_job: None,
            fetched_at: None,
            offline: false,
            excluded_entries: 0,
            preferred_model: None,
        };
        picker.load_static();
        Ok(picker)
    }
    fn load_static(&mut self) {
        self.entries = opencode::catalog(self.provider)
            .iter()
            .map(|spec| ModelPickerEntry {
                provider: self.provider,
                id: spec.id.into(),
                name: None,
                api_label: spec.api.name().into(),
                unavailable_reason: spec
                    .api
                    .adapter()
                    .is_none()
                    .then(|| format!("{}: {}", spec.api.name(), spec.api.reason())),
                metadata: None,
            })
            .collect();
        self.state = if self.provider == ProviderKind::Openrouter {
            ModelPickerState::LoadingWithoutSnapshot
        } else {
            ModelPickerState::ReadyFresh
        };
    }
    pub fn results(&self) -> Vec<&ModelPickerEntry> {
        let exact_query = opencode::model_id(self.provider, &self.query).unwrap_or(&self.query);
        let query = exact_query.to_ascii_lowercase();
        let mut results: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| {
                entry.id.to_ascii_lowercase().contains(&query)
                    || (self.provider != ProviderKind::Openrouter
                        && entry.api_label.to_ascii_lowercase().contains(&query))
                    || entry
                        .name
                        .as_ref()
                        .is_some_and(|name| name.to_ascii_lowercase().contains(&query))
            })
            .collect();
        results.sort_by(|a, b| {
            (a.id != exact_query)
                .cmp(&(b.id != exact_query))
                .then(a.id.cmp(&b.id))
        });
        results
    }
    pub fn refresh(&mut self) {
        self.selected = 0;
        self.error = None;
    }
    pub fn prefer(&mut self, model: &str) {
        self.preferred_model = Some(model.into());
        self.selected = self
            .results()
            .iter()
            .position(|entry| entry.id == model)
            .unwrap_or(0);
    }
    pub fn cycle_provider(&mut self, backwards: bool) {
        let model = self
            .results()
            .get(self.selected)
            .map(|entry| entry.id.clone());
        self.query = opencode::model_id(self.provider, &self.query)
            .unwrap_or(&self.query)
            .to_owned();
        self.provider = match (self.provider, backwards) {
            (ProviderKind::OpencodeGo, false) | (ProviderKind::Openrouter, true) => {
                ProviderKind::OpencodeZen
            }
            (ProviderKind::OpencodeZen, false) | (ProviderKind::OpencodeGo, true) => {
                ProviderKind::Openrouter
            }
            _ => ProviderKind::OpencodeGo,
        };
        self.generation = self.generation.wrapping_add(1);
        self.catalog_job = None;
        self.fetched_at = None;
        self.offline = false;
        self.excluded_entries = 0;
        self.refresh();
        self.load_static();
        if let Some(model) = model {
            self.prefer(&model);
        }
    }
    pub fn can_select(&self) -> bool {
        matches!(
            self.state,
            ModelPickerState::ReadyFresh | ModelPickerState::ReadyStale
        )
    }
    pub fn apply_catalog(
        &mut self,
        generation: u64,
        result: Result<CatalogResult, CatalogError>,
    ) -> bool {
        if generation != self.generation || self.provider != ProviderKind::Openrouter {
            return false;
        }
        self.catalog_job = None;
        match result {
            Ok(result) => {
                self.fetched_at = Some(result.snapshot.fetched_at);
                self.offline = result.offline;
                self.excluded_entries = result.snapshot.excluded_entries;
                self.error = result.warning.map(|warning| warning.to_string());
                self.entries = result
                    .snapshot
                    .models
                    .iter()
                    .map(|model| {
                        let unavailable_reason = match (model.text_support(), model.tools_support())
                        {
                            (openrouter::Support::Unsupported, _) => {
                                Some("Text output unsupported".into())
                            }
                            (openrouter::Support::Unknown, _) => {
                                Some("Text output support unknown".into())
                            }
                            (_, openrouter::Support::Unsupported) => {
                                Some("Tools unsupported".into())
                            }
                            (_, openrouter::Support::Unknown) => {
                                Some("Tools support unknown".into())
                            }
                            _ => None,
                        };
                        ModelPickerEntry {
                            provider: self.provider,
                            id: model.id.clone(),
                            name: model.name.clone(),
                            api_label: "Chat Completions".into(),
                            unavailable_reason,
                            metadata: result.snapshot.selected_metadata(&model.id),
                        }
                    })
                    .collect();
                self.state = if result.freshness == CatalogFreshness::Fresh {
                    ModelPickerState::ReadyFresh
                } else {
                    ModelPickerState::ReadyStale
                };
                let preferred = self.preferred_model.clone();
                if let Some(model) = preferred {
                    self.prefer(&model);
                } else {
                    self.selected = 0;
                }
            }
            Err(error) => {
                self.error = Some(error.to_string());
                self.state = if self.entries.is_empty() {
                    ModelPickerState::FailedWithoutSnapshot
                } else {
                    ModelPickerState::ReadyStale
                };
            }
        }
        true
    }
    pub fn move_selection(&mut self, down: bool) {
        let count = self.results().len();
        if count > 0 {
            self.selected = if down {
                (self.selected + 1) % count
            } else {
                (self.selected + count - 1) % count
            };
        }
    }
}
