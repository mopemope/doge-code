use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct LlmConfig {
    pub connect_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub read_idle_timeout_ms: u64,
    pub max_retries: usize,
    pub retry_base_ms: u64,
    pub retry_jitter_ms: u64,
    pub respect_retry_after: bool,
    pub timeout_ms: u64,
    /// Context window size in tokens for the model
    pub context_window_size: Option<u32>,
}

impl LlmConfig {
    pub fn apply_partial(&mut self, partial: &PartialLlmConfig) {
        if let Some(v) = partial.connect_timeout_ms {
            self.connect_timeout_ms = v;
        }
        if let Some(v) = partial.request_timeout_ms {
            self.request_timeout_ms = v;
        }
        if let Some(v) = partial.read_idle_timeout_ms {
            self.read_idle_timeout_ms = v;
        }
        if let Some(v) = partial.max_retries {
            self.max_retries = v;
        }
        if let Some(v) = partial.retry_base_ms {
            self.retry_base_ms = v;
        }
        if let Some(v) = partial.retry_jitter_ms {
            self.retry_jitter_ms = v;
        }
        if let Some(v) = partial.respect_retry_after {
            self.respect_retry_after = v;
        }
        if let Some(v) = partial.timeout_ms {
            self.timeout_ms = v;
        }
        if let Some(v) = partial.context_window_size {
            self.context_window_size = Some(v);
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            connect_timeout_ms: 10000,
            request_timeout_ms: 30000,
            read_idle_timeout_ms: 30000,
            max_retries: 3,
            retry_base_ms: 1000,
            retry_jitter_ms: 500,
            respect_retry_after: true,
            timeout_ms: 30000,
            context_window_size: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct PartialLlmConfig {
    pub connect_timeout_ms: Option<u64>,
    pub request_timeout_ms: Option<u64>,
    pub read_idle_timeout_ms: Option<u64>,
    pub max_retries: Option<usize>,
    pub retry_base_ms: Option<u64>,
    pub retry_jitter_ms: Option<u64>,
    pub respect_retry_after: Option<bool>,
    pub timeout_ms: Option<u64>,
    /// Context window size in tokens for the model
    pub context_window_size: Option<u32>,
}
