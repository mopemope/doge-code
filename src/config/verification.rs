use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct VerificationConfig {
    pub enabled: bool,
    pub enforce: bool,
    pub timeout_ms: u64,
    pub auto_revert: bool,
    pub commands: VerificationCommands,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VerificationCommands {
    pub rust: Vec<String>,
    pub python: Vec<String>,
    pub node: Vec<String>,
    pub typescript: Vec<String>,
    pub go: Vec<String>,
}

impl Default for VerificationCommands {
    fn default() -> Self {
        Self {
            rust: vec![
                "cargo".to_string(),
                "check".to_string(),
                "--quiet".to_string(),
                "--message-format=short".to_string(),
            ],
            python: vec![
                "python3".to_string(),
                "-m".to_string(),
                "py_compile".to_string(),
                "{path}".to_string(),
            ],
            node: vec![
                "node".to_string(),
                "--check".to_string(),
                "{path}".to_string(),
            ],
            typescript: vec![
                "tsc".to_string(),
                "--noEmit".to_string(),
                "--allowSyntheticDefaultImports".to_string(),
                "--target".to_string(),
                "esnext".to_string(),
                "--moduleResolution".to_string(),
                "node".to_string(),
                "{path}".to_string(),
            ],
            go: vec!["go".to_string(), "vet".to_string(), "{path}".to_string()],
        }
    }
}

impl Default for VerificationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            enforce: true,
            timeout_ms: 120_000,
            auto_revert: false,
            commands: VerificationCommands::default(),
        }
    }
}

impl VerificationConfig {
    pub fn apply_partial(&mut self, partial: &PartialVerificationConfig) {
        if let Some(v) = partial.enabled {
            self.enabled = v;
        }
        if let Some(v) = partial.enforce {
            self.enforce = v;
        }
        if let Some(v) = partial.timeout_ms {
            self.timeout_ms = v;
        }
        if let Some(v) = partial.auto_revert {
            self.auto_revert = v;
        }
        if let Some(pc) = &partial.commands {
            if let Some(v) = &pc.rust {
                self.commands.rust = v.clone();
            }
            if let Some(v) = &pc.python {
                self.commands.python = v.clone();
            }
            if let Some(v) = &pc.node {
                self.commands.node = v.clone();
            }
            if let Some(v) = &pc.typescript {
                self.commands.typescript = v.clone();
            }
            if let Some(v) = &pc.go {
                self.commands.go = v.clone();
            }
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct PartialVerificationConfig {
    pub enabled: Option<bool>,
    pub enforce: Option<bool>,
    pub timeout_ms: Option<u64>,
    pub auto_revert: Option<bool>,
    pub commands: Option<PartialVerificationCommands>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct PartialVerificationCommands {
    pub rust: Option<Vec<String>>,
    pub python: Option<Vec<String>>,
    pub node: Option<Vec<String>>,
    pub typescript: Option<Vec<String>>,
    pub go: Option<Vec<String>>,
}
