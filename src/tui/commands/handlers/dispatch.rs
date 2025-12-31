use crate::tui::view::TuiApp;
use std::any::Any;

use crate::tui::commands::core::{CommandHandler, TuiExecutor};
use crate::tui::commands::handlers::custom::load_custom_commands;
use crate::tui::commands::handlers::slash_commands::cancel::handle_cancel;
use crate::tui::commands::handlers::slash_commands::clear::handle_clear;
use crate::tui::commands::handlers::slash_commands::compact::handle_compact;
use crate::tui::commands::handlers::slash_commands::edit_symbol::handle_edit_symbol;
use crate::tui::commands::handlers::slash_commands::fix::handle_fix;
use crate::tui::commands::handlers::slash_commands::git_worktree::handle_git_worktree;
use crate::tui::commands::handlers::slash_commands::help::handle_help;
use crate::tui::commands::handlers::slash_commands::lint::handle_lint;
use crate::tui::commands::handlers::slash_commands::map::handle_map;
use crate::tui::commands::handlers::slash_commands::open::handle_open;
use crate::tui::commands::handlers::slash_commands::quit::handle_quit;
use crate::tui::commands::handlers::slash_commands::rebuild_repomap::handle_rebuild_repomap;
use crate::tui::commands::handlers::slash_commands::stack::handle_stack;
use crate::tui::commands::handlers::slash_commands::test::handle_test;
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
            "/edit-symbol" => handle_edit_symbol(self, ui),
            "/lint" => handle_lint(self, ui),
            "/test" => handle_test(self, ui),
            line if line.starts_with("/stack") => {
                let args = line.strip_prefix("/stack").unwrap_or("").trim();
                handle_stack(self, ui, args);
            }
            line if line.starts_with("/fix") => {
                let args = line.strip_prefix("/fix").unwrap_or("").trim();
                handle_fix(self, ui, args);
            }
            "/git-worktree" => match handle_git_worktree() {
                Ok(message) => ui.push_log(message),
                Err(e) => ui.push_log(format!("Error: {}", e)),
            },
            line if line.starts_with("/open ") => handle_open(self, line, ui),
            line if line.starts_with("/theme ") => handle_theme(line, ui),
            line if line.starts_with("/plan") => {
                let args = line.strip_prefix("/plan").unwrap_or("").trim();
                let session_id = self
                    .session_manager
                    .lock()
                    .unwrap()
                    .get_current_session_id()
                    .unwrap_or("default".to_string());

                if let Err(e) = crate::tui::commands::handlers::slash_commands::plan::handle_plan(
                    args,
                    &session_id,
                    ui,
                    &self.cfg,
                ) {
                    ui.push_log(format!("Error handling plan command: {}", e));
                }
            }
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
