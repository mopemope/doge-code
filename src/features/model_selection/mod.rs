//! Offline same-provider picker; selecting is a separate session transaction.
use crate::features::{openai_subscription::ProviderKind, opencode};
use anyhow::Result;

#[derive(Debug, Clone)]
pub struct ModelPicker {
    pub provider: ProviderKind,
    pub query: String,
    pub selected: usize,
    pub error: Option<String>,
}
impl ModelPicker {
    pub fn new(provider: ProviderKind, query: &str) -> Result<Self> {
        anyhow::ensure!(
            !opencode::catalog(provider).is_empty(),
            "TUI /models supports the offline Go/Zen catalogs only; use dgc models --provider openai for the OAuth account catalog"
        );
        Ok(Self {
            provider,
            query: query.into(),
            selected: 0,
            error: None,
        })
    }
    pub fn results(&self) -> Vec<&'static opencode::catalog::ModelSpec> {
        let query = opencode::model_id(self.provider, &self.query)
            .unwrap_or(&self.query)
            .to_ascii_lowercase();
        let mut results: Vec<_> = opencode::catalog(self.provider)
            .iter()
            .filter(|spec| {
                spec.id.contains(&query) || spec.api.name().to_ascii_lowercase().contains(&query)
            })
            .collect();
        // An exact pasted ID wins over longer prefix matches such as -flash.
        results.sort_by_key(|spec| spec.id != query);
        results
    }

    pub fn refresh(&mut self) {
        self.selected = 0;
        self.error = None;
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
        self.error = None;
    }
}
