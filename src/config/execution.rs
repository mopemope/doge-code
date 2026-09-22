//! Execution policy configuration (`[execution]` section).
//!
//! Structured process execution (`execute_process`) is gated by an explicit
//! policy. Legacy `allowed_commands` is handled as a deprecated fallback
//! (see `crate::execution::policy`).

use serde::{Deserialize, Serialize};

/// How structured process execution is gated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// No policy checks on the program itself (cwd/env still validated).
    #[default]
    Unrestricted,
    /// Only programs listed in `allowed_programs` may run.
    Allowlist,
    /// Deny every process execution.
    Deny,
}

/// Resolved execution policy.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExecutionConfig {
    pub mode: ExecutionMode,
    pub allowed_programs: Vec<String>,
    pub allow_shell: bool,
    pub allowed_env: Vec<String>,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            mode: ExecutionMode::Unrestricted,
            allowed_programs: Vec::new(),
            // Shell is on by default for backward compatibility; explicit
            // `[execution]` config can turn it off.
            allow_shell: true,
            allowed_env: vec![
                "RUST_BACKTRACE".to_string(),
                "RUST_LOG".to_string(),
                "CARGO_TERM_COLOR".to_string(),
            ],
        }
    }
}

impl ExecutionConfig {
    /// True when this is the implicit default (no explicit `[execution]`).
    /// Used to decide whether legacy `allowed_commands` takes over.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn apply_partial(&mut self, partial: &PartialExecutionConfig) {
        if let Some(mode) = partial.mode {
            self.mode = mode;
        }
        if let Some(ref programs) = partial.allowed_programs {
            self.allowed_programs = programs.clone();
        }
        if let Some(allow_shell) = partial.allow_shell {
            self.allow_shell = allow_shell;
        }
        if let Some(ref env) = partial.allowed_env {
            self.allowed_env = env.clone();
        }
    }
}

/// Partial `[execution]` table for layered config merge.
///
/// Project config wins over global/user config per field (same precedence as
/// the rest of `FileConfig`).
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct PartialExecutionConfig {
    pub mode: Option<ExecutionMode>,
    pub allowed_programs: Option<Vec<String>>,
    pub allow_shell: Option<bool>,
    pub allowed_env: Option<Vec<String>>,
}

impl PartialExecutionConfig {
    /// True when no field is set (e.g. a bare `[execution]` table). An empty
    /// table must NOT flip authority away from legacy `allowed_commands`.
    pub fn is_empty(&self) -> bool {
        self.mode.is_none()
            && self.allowed_programs.is_none()
            && self.allow_shell.is_none()
            && self.allowed_env.is_none()
    }
}

/// Merge global/user partial with project partial. Project wins per field.
/// Returns `None` when neither side sets any field, so an empty `[execution]`
/// table does not count as "configured".
pub fn merge_execution(
    file: Option<&PartialExecutionConfig>,
    project: Option<&PartialExecutionConfig>,
) -> Option<PartialExecutionConfig> {
    let merged = match (file, project) {
        (None, None) => None,
        (Some(f), None) => Some(f.clone()),
        (None, Some(p)) => Some(p.clone()),
        (Some(f), Some(p)) => Some(PartialExecutionConfig {
            mode: p.mode.or(f.mode),
            allowed_programs: p
                .allowed_programs
                .clone()
                .or_else(|| f.allowed_programs.clone()),
            allow_shell: p.allow_shell.or(f.allow_shell),
            allowed_env: p.allowed_env.clone().or_else(|| f.allowed_env.clone()),
        }),
    };
    merged.filter(|m| !m.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_is_unrestricted_with_shell() {
        let cfg = ExecutionConfig::default();
        assert_eq!(cfg.mode, ExecutionMode::Unrestricted);
        assert!(cfg.allow_shell);
        assert!(cfg.is_default());
    }

    #[test]
    fn test_mode_serde_snake_case() {
        let mode: ExecutionMode = serde_json::from_str(r#""allowlist""#).expect("parse");
        assert_eq!(mode, ExecutionMode::Allowlist);
        assert_eq!(
            serde_json::to_string(&ExecutionMode::Deny).unwrap(),
            r#""deny""#
        );
        assert_eq!(
            serde_json::to_string(&ExecutionMode::Unrestricted).unwrap(),
            r#""unrestricted""#
        );
    }

    #[test]
    fn test_apply_partial_overrides() {
        let mut cfg = ExecutionConfig::default();
        cfg.apply_partial(&PartialExecutionConfig {
            mode: Some(ExecutionMode::Allowlist),
            allowed_programs: Some(vec!["cargo".to_string()]),
            allow_shell: Some(false),
            allowed_env: None,
        });
        assert_eq!(cfg.mode, ExecutionMode::Allowlist);
        assert_eq!(cfg.allowed_programs, vec!["cargo".to_string()]);
        assert!(!cfg.allow_shell);
        assert!(!cfg.is_default());
    }

    #[test]
    fn test_merge_execution_project_wins_per_field() {
        let file = PartialExecutionConfig {
            mode: Some(ExecutionMode::Allowlist),
            allowed_programs: Some(vec!["cargo".to_string()]),
            allow_shell: Some(false),
            allowed_env: None,
        };
        let project = PartialExecutionConfig {
            mode: None,
            allowed_programs: Some(vec!["cargo".to_string(), "git".to_string()]),
            allow_shell: None,
            allowed_env: Some(vec!["RUST_BACKTRACE".to_string()]),
        };
        let merged = merge_execution(Some(&file), Some(&project)).expect("merged");
        // Project programs win (replace, not union).
        assert_eq!(
            merged.allowed_programs,
            Some(vec!["cargo".to_string(), "git".to_string()])
        );
        // Unspecified project fields fall back to global.
        assert_eq!(merged.mode, Some(ExecutionMode::Allowlist));
        assert_eq!(merged.allow_shell, Some(false));
        assert_eq!(merged.allowed_env, Some(vec!["RUST_BACKTRACE".to_string()]));
    }

    #[test]
    fn test_merge_execution_both_none() {
        assert!(merge_execution(None, None).is_none());
    }

    #[test]
    fn test_merge_execution_empty_table_is_not_configured() {
        let empty = PartialExecutionConfig::default();
        assert!(empty.is_empty());
        assert!(merge_execution(Some(&empty), None).is_none());
        assert!(merge_execution(None, Some(&empty)).is_none());
        assert!(merge_execution(Some(&empty), Some(&empty)).is_none());
    }
}
