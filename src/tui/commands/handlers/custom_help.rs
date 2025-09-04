use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Custom command information
#[derive(Debug, Clone)]
pub struct CustomCommand {
    pub name: String,
    pub description: String,
    pub content: String,
    pub scope: crate::tui::commands::handlers::dispatch::CommandScope,
    pub namespace: Option<String>,
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
                ui.push_log(format!("  /{} - {} {}", name, command.description, scope_str));
            }
        }
    }
}

/// Load custom commands from project and user directories
pub fn load_custom_commands(project_root: &Path) -> HashMap<String, CustomCommand> {
    let mut commands = HashMap::new();
    
    // Load project commands (.doge/commands/)
    let project_commands_dir = project_root.join(".doge").join("commands");
    if project_commands_dir.exists() {
        load_commands_from_directory(&project_commands_dir, CommandScope::Project, &mut commands);
    }
    
    // Load user commands (~/.config/doge-code/commands/)
    if let Some(home_dir) = dirs::home_dir() {
        let user_commands_dir = home_dir.join(".config").join("doge-code").join("commands");
        if user_commands_dir.exists() {
            load_commands_from_directory(&user_commands_dir, CommandScope::User, &mut commands);
        }
    }
    
    commands
}

/// Load commands from a specific directory
fn load_commands_from_directory(
    dir: &Path,
    scope: crate::tui::commands::handlers::dispatch::CommandScope,
    commands: &mut HashMap<String, CustomCommand>,
) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().map_or(false, |ext| ext == "md") {
                // Get command name from file name (without extension)
                if let Some(file_name) = path.file_stem().and_then(|s| s.to_str()) {
                    // Read file content
                    if let Ok(content) = fs::read_to_string(&path) {
                        // Use first line as description, rest as content
                        let lines: Vec<&str> = content.lines().collect();
                        let description = if !lines.is_empty() {
                            lines[0].to_string()
                        } else {
                            format!("Custom command: {}", file_name)
                        };
                        
                        // Determine namespace from relative path
                        let namespace = path.parent()
                            .and_then(|parent| parent.strip_prefix(dir).ok())
                            .and_then(|rel_path| {
                                if rel_path.components().count() > 0 {
                                    Some(rel_path.to_string_lossy().to_string())
                                } else {
                                    None
                                }
                            });
                        
                        let full_content = content;
                        
                        commands.insert(
                            file_name.to_string(),
                            CustomCommand {
                                name: file_name.to_string(),
                                description,
                                content: full_content,
                                scope: scope.clone(),
                                namespace,
                            },
                        );
                    }
                }
            } else if path.is_dir() {
                // Recursively load from subdirectories (namespaces)
                load_commands_from_directory(&path, scope.clone(), commands);
            }
        }
    }
}