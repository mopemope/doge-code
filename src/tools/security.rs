use crate::config::AppConfig;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct SecurityChecker {
    config: Arc<AppConfig>,
}

impl SecurityChecker {
    pub fn new(config: Arc<AppConfig>) -> Self {
        Self { config }
    }

    /// Check if a command is allowed based on the allowed_commands list
    pub fn is_command_allowed(&self, command: &str) -> bool {
        // If no allowed commands are specified, allow all commands (backward compatibility)
        if self.config.allowed_commands.is_empty() {
            return true;
        }

        // Check if the command matches any of the allowed commands (prefix match)
        self.config.allowed_commands.iter().any(|allowed| {
            // Exact match or prefix match (with space or end of string)
            command == allowed || command.starts_with(&format!("{} ", allowed))
        })
    }
}
