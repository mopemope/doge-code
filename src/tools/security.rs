use crate::config::AppConfig;
use std::sync::Arc;

/// Deprecated: use `crate::execution::ExecutionPolicy` instead.
///
/// Kept as a thin wrapper so external callers keep compiling. The old raw
/// prefix match is NOT used anymore — it could not distinguish
/// `cargo test` from `cargo test; rm -rf …` because execution went through
/// `bash -c`.
#[derive(Debug, Clone)]
#[deprecated(note = "Use crate::execution::ExecutionPolicy instead")]
pub struct SecurityChecker {
    config: Arc<AppConfig>,
}

#[allow(deprecated)]
impl SecurityChecker {
    pub fn new(config: Arc<AppConfig>) -> Self {
        Self { config }
    }

    /// Check if a command is allowed based on the allowed_commands list.
    /// Delegates to `ExecutionPolicy` (shell operators deny the command).
    pub fn is_command_allowed(&self, command: &str) -> bool {
        let policy = crate::execution::ExecutionPolicy::new(self.config.clone());
        if self.config.allowed_commands.is_empty() && !self.config.execution_configured {
            return true;
        }
        policy.check_legacy_shell_command(command).is_ok()
    }
}
