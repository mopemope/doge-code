use crate::analysis::Analyzer;
use crate::tui::theme::Theme;
use crate::tui::view::TuiApp;
use std::any::Any;
use tracing::{error, info, warn};

use crate::tui::commands::core::{CommandHandler, TuiExecutor};
use crate::tui::commands::handlers::custom::load_custom_commands;

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
            "/help" => {
                ui.push_log("Available commands:");
                ui.push_log("  /help - Show this help message");
                ui.push_log("  /quit - Exit the application");
                ui.push_log("  /clear - Clear the conversation and log area");
                ui.push_log("  /open <path> - Open a file in your editor");
                ui.push_log("  /theme <name> - Switch theme (dark/light)");
                ui.push_log("  /tools - List available tools");
                ui.push_log("  /tokens - Show token usage");
                ui.push_log("  /compact - Compact conversation history to reduce token usage");
                ui.push_log("  /cancel - Cancel the current operation");
                ui.push_log("");
                ui.push_log("Repository Analysis:");
                ui.push_log("  /map - Show repository analysis summary");
                ui.push_log("  /rebuild-repomap - Rebuild repository analysis");
                ui.push_log("");
                ui.push_log("Session Management:");
                ui.push_log(
                    "  /session <new|list|switch|save|delete|current|clear> - Manage sessions",
                );
                ui.push_log("");
                ui.push_log("Task Planning & Execution:");
                ui.push_log("  /plan <task> - Analyze task and create an execution plan");
                ui.push_log("  /plans - List active and recent plans");
                ui.push_log("  /execute [plan_id] - Execute the latest or a specific plan");
                ui.push_log("");
                ui.push_log("Custom commands:");
                self.display_custom_commands_help(ui);
                ui.push_log("");
                ui.push_log("Scroll controls:");
                ui.push_log("  Page Up/Down - Scroll by page");
                ui.push_log("  Ctrl+Up/Down - Scroll by line");
                ui.push_log("  Ctrl+Home - Scroll to top");
                ui.push_log("  Ctrl+End - Scroll to bottom");
                ui.push_log("  Ctrl+L - Return to bottom (auto-scroll");
                ui.push_log("");
                ui.push_log("Other controls:");
                ui.push_log("  @ - File completion");
                ui.push_log("  ! - Shell mode (at start of empty line)");
                ui.push_log("  Esc - Cancel operation or exit shell mode");
                ui.push_log("  Ctrl+C - Cancel (press twice to exit)");
            }
            "/tools" => {
                ui.push_log("Available tools for the LLM:");
                ui.push_log("  - fs_list: List files and directories");
                ui.push_log("  - fs_read: Read a file");
                ui.push_log("  - fs_read_many_files: Read multiple files");
                ui.push_log("  - fs_write: Write to a file");
                ui.push_log("  - search_text: Search for text in files");
                ui.push_log("  - execute_bash: Execute a shell command");
                ui.push_log("  - find_file: Find a file by name or pattern");
                ui.push_log("  - search_repomap: Search the repomap with specific criteria");
            }
            "/clear" => {
                ui.clear_log();
            }
            "/rebuild-repomap" => {
                // Force complete rebuild (ignore cache)
                ui.push_log("[Starting forced complete repomap rebuild (ignoring cache)...]");
                let repomap_clone = self.repomap.clone();
                let project_root = self.cfg.project_root.clone();
                let ui_tx = self.ui_tx.clone();

                tokio::spawn(async move {
                    info!("Starting forced complete repomap rebuild");
                    let start_time = std::time::Instant::now();

                    let mut analyzer = match Analyzer::new(&project_root).await {
                        Ok(analyzer) => analyzer,
                        Err(e) => {
                            error!("Failed to create Analyzer: {:?}", e);
                            if let Some(tx) = ui_tx {
                                let _ = tx.send(format!("[Failed to create analyzer: {}]", e));
                            }
                            return;
                        }
                    };

                    // Clear cache first to force complete rebuild
                    if let Err(e) = analyzer.clear_cache().await {
                        warn!("Failed to clear cache before forced rebuild: {}", e);
                    }

                    match analyzer.build_parallel().await {
                        Ok(map) => {
                            let duration = start_time.elapsed();
                            let symbol_count = map.symbols.len();
                            *repomap_clone.write().await = Some(map);

                            info!(
                                "Forced repomap rebuild completed in {:?} with {} symbols",
                                duration, symbol_count
                            );
                            if let Some(tx) = ui_tx {
                                let _ = tx.send(format!(
                                    "[Forced rebuild completed in {:?} - {} symbols found (cache cleared)]",
                                    duration, symbol_count
                                ));
                            }
                        }
                        Err(e) => {
                            error!("Failed to force rebuild RepoMap: {:?}", e);
                            if let Some(tx) = ui_tx {
                                let _ =
                                    tx.send(format!("[Failed to force rebuild repomap: {}]", e));
                            }
                        }
                    }
                });
            }

            "/cancel" => {
                if let Some(tx) = &self.cancel_tx {
                    let _ = tx.send(true);
                    if let Some(tx) = &self.ui_tx {
                        let _ = tx.send("::status:cancelled".into());
                    }
                    ui.push_log("[Cancelled]");
                    self.cancel_tx = None;
                } else {
                    ui.push_log("[no running task]");
                }
                ui.dirty = true;
            }

            "/compact" => {
                self.handle_compact_command(ui);
            }

            "/map" => {
                // Check if repomap has been generated
                let repomap = self.repomap.clone();
                let ui_tx = self.ui_tx.clone().unwrap();
                tokio::spawn(async move {
                    let repomap_guard = repomap.read().await;
                    if let Some(map) = &*repomap_guard {
                        let _ = ui_tx.send(format!("RepoMap: {} symbols ", map.symbols.len()));
                        for s in map.symbols.iter().take(50) {
                            let _ = ui_tx.send(format!(
                                "{} {}  @{}:{}",
                                s.kind.as_str(),
                                s.name,
                                s.file.display(),
                                s.start_line
                            ));
                        }
                    } else {
                        let _ = ui_tx.send("[repomap] Still generating...".to_string());
                    }
                });
            }
            // Handle /theme command
            line if line.starts_with("/theme ") => {
                let theme_name = line[7..].trim(); // get the string after "/theme "
                match theme_name.to_lowercase().as_str() {
                    "dark" => {
                        ui.theme = Theme::dark();
                        ui.push_log("[Theme switched to dark]");
                    }
                    "light" => {
                        ui.theme = Theme::light();
                        ui.push_log("[Theme switched to light]");
                    }
                    _ => {
                        ui.push_log(format!(
                            "[Unknown theme: {theme_name}. Available themes: dark, light]"
                        ));
                    }
                }
                // Redraw after theme change
                ui.dirty = true;
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
