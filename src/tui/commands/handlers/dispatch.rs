use crate::tui::view::TuiApp;
use std::any::Any;

use crate::tui::commands::core::{CommandHandler, TuiExecutor};
use crate::tui::commands::handlers::custom::load_custom_commands;
use crate::tui::commands::handlers::slash_commands::cancel::handle_cancel;
use crate::tui::commands::handlers::slash_commands::clear::handle_clear;
use crate::tui::commands::handlers::slash_commands::compact::handle_compact;
use crate::tui::commands::handlers::slash_commands::git_worktree::handle_git_worktree;
use crate::tui::commands::handlers::slash_commands::help::handle_help;
use crate::tui::commands::handlers::slash_commands::map::handle_map;
use crate::tui::commands::handlers::slash_commands::open::handle_open;
use crate::tui::commands::handlers::slash_commands::quit::handle_quit;
use crate::tui::commands::handlers::slash_commands::rebuild_repomap::handle_rebuild_repomap;
use crate::tui::commands::handlers::slash_commands::theme::handle_theme;
use crate::tui::commands::handlers::slash_commands::tokens::handle_tokens;
use crate::tui::commands::handlers::slash_commands::tools::handle_tools;

// Refactored to delegate slash commands to dedicated modules for better modularity and maintainability.
// This allows each command to be tested independently and keeps dispatch.rs focused on routing.

impl CommandHandler for TuiExecutor {
    fn handle(&mut self, line: &str, ui: &mut TuiApp) {
        // This function was extracted from the big handlers.rs for readability.
        if self.ui_tx.is_none() {
            self.ui_tx = ui.sender();
        }
        let line = line.trim();
        if line.is_empty() {
            return;
        }

        match line {
            "/help" => handle_help(self, ui),
            "/tools" => handle_tools(self, ui),
            "/quit" => handle_quit(self, ui),
            "/clear" => handle_clear(self, ui),
            "/tokens" => handle_tokens(self, ui),
            "/rebuild-repomap" => handle_rebuild_repomap(self, ui),
            "/cancel" => handle_cancel(self, ui),
            "/compact" => handle_compact(self, ui),
            "/map" => handle_map(self, ui),
            "/git-worktree" => match handle_git_worktree() {
                Ok(message) => ui.push_log(message),
                Err(e) => ui.push_log(format!("Error: {}", e)),
            },
            line if line.starts_with("/open ") => handle_open(self, line, ui),
            line if line.starts_with("/theme ") => handle_theme(self, line, ui),
            _ => {
                // Rest of content moved to exec.rs
                self.handle_dispatch_rest(line, ui);
            }
        }
    }

    fn get_custom_commands(&self) -> Vec<String> {
        let custom_commands = load_custom_commands(&self.cfg.project_root);
        custom_commands
            .keys()
            .map(|name| format!("/{}", name))
            .collect()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl TuiExecutor {
    /// Display help for custom commands
    pub fn display_custom_commands_help(&self, ui: &mut TuiApp) {
        let custom_commands = load_custom_commands(&self.cfg.project_root);

        if custom_commands.is_empty() {
            ui.push_log("  (No custom commands found)");
        } else {
            for (name, command) in custom_commands {
                let scope_str = match command.scope {
                    CommandScope::Project => {
                        if let Some(namespace) = &command.namespace {
                            format!("(project:{})", namespace)
                        } else {
                            "(project)".to_string()
                        }
                    }
                    CommandScope::User => "(user)".to_string(),
                };
                ui.push_log(format!(
                    "  /{} - {} {}",
                    name, command.description, scope_str
                ));
            }
        }
    }
}

/// Command scope (project or user)
#[derive(Debug, Clone)]
pub enum CommandScope {
    Project,
    User,
}
