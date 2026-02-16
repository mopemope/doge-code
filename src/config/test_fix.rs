use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct TestFixConfig {
    pub enabled: bool,
    pub max_iterations: usize,
    pub test_timeout_ms: u64,
    pub auto_gen_regression_test: bool,
}

impl Default for TestFixConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_iterations: 3,
            test_timeout_ms: 120_000,
            auto_gen_regression_test: true,
        }
    }
}

impl TestFixConfig {
    pub fn apply_partial(&mut self, partial: &PartialTestFixConfig) {
        if let Some(v) = partial.enabled {
            self.enabled = v;
        }
        if let Some(v) = partial.max_iterations {
            self.max_iterations = v;
        }
        if let Some(v) = partial.test_timeout_ms {
            self.test_timeout_ms = v;
        }
        if let Some(v) = partial.auto_gen_regression_test {
            self.auto_gen_regression_test = v;
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct PartialTestFixConfig {
    pub enabled: Option<bool>,
    pub max_iterations: Option<usize>,
    pub test_timeout_ms: Option<u64>,
    pub auto_gen_regression_test: Option<bool>,
}
