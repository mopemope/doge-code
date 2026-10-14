use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub iterations: usize,
    pub tool_calls: usize,
    pub tokens: u64,
    pub elapsed_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verifier {
    pub program: String,
    pub args: Vec<String>,
    pub timeout_ms: u64,
    #[serde(default)]
    pub test_format: Option<String>,
    #[serde(default)]
    pub minimum_tests: Option<usize>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub version: u32,
    pub instruction: String,
    pub editable_paths: BTreeSet<String>,
    #[serde(default)]
    pub protected_suffixes: BTreeMap<String, String>,
    pub verifiers: Vec<Verifier>,
    /// One restart means at most two model turns. V1 deliberately caps at one.
    pub max_restarts: u32,
    pub per_run: Limits,
    pub total_requests: u64,
    pub total_tokens: u64,
    pub total_elapsed_ms: u64,
    /// Explicit cache/toolchain paths only; never model-visible or inherited secrets.
    #[serde(default)]
    pub verifier_env: BTreeMap<String, String>,
}
pub fn relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', '\0'])
        && !Path::new(path).is_absolute()
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        && !path
            .split('/')
            .any(|c| matches!(c, ".git" | ".doge" | "target"))
}
impl Plan {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.version == 1, "unsupported repair plan version");
        ensure!(!self.instruction.trim().is_empty(), "instruction required");
        ensure!(self.max_restarts <= 1, "v1 permits at most one restart");
        ensure!(
            !self.editable_paths.is_empty() && self.editable_paths.iter().all(|p| relative(p)),
            "editable paths must be explicit project-relative files"
        );
        ensure!(
            self.protected_suffixes
                .iter()
                .all(|(p, m)| self.editable_paths.contains(p) && !m.is_empty()),
            "protected suffix needs an editable path and nonempty unique marker"
        );
        ensure!(
            self.per_run.iterations > 0
                && self.per_run.tool_calls > 0
                && self.per_run.tokens > 0
                && self.per_run.elapsed_ms > 0,
            "per-run limits must be finite and positive"
        );
        ensure!(
            self.total_requests > 0
                && self.total_tokens >= self.per_run.tokens
                && self.total_elapsed_ms > 0,
            "total limits must be finite and positive"
        );
        ensure!(
            self.total_elapsed_ms <= 86_400_000 && self.per_run.elapsed_ms <= self.total_elapsed_ms,
            "repair deadline must fit within one day"
        );
        ensure!(
            !self.verifiers.is_empty() && self.verifiers.len() <= 8,
            "declare one to eight verifiers"
        );
        for v in &self.verifiers {
            ensure!(
                matches!(
                    (v.test_format.as_deref(), v.minimum_tests),
                    (None, None) | (Some("cargo" | "go_text" | "go_json"), Some(1..))
                ),
                "test verifier needs supported format and positive minimum_tests"
            );
            ensure!(
                !v.program.is_empty()
                    && !v.program.contains(['/', '\\', '\0'])
                    && v.timeout_ms > 0
                    && v.timeout_ms <= self.total_elapsed_ms,
                "verifier needs bare program and finite timeout"
            );
            ensure!(
                !matches!(
                    v.program.as_str(),
                    "sh" | "bash" | "zsh" | "fish" | "cmd" | "powershell" | "pwsh"
                ),
                "shell verifiers are unsupported"
            );
            ensure!(
                v.args.iter().all(|s| !s.contains('\0')),
                "NUL in verifier argv"
            );
        }
        ensure!(
            self.verifier_env.keys().all(|k| matches!(
                k.as_str(),
                "PATH" | "CARGO_HOME" | "RUSTUP_HOME" | "GOMODCACHE"
            )),
            "only explicit PATH/cache/toolchain environment is supported"
        );
        Ok(())
    }
}
