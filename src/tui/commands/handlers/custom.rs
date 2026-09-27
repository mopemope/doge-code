use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Custom command information
#[derive(Debug, Clone)]
pub struct CustomCommand {
    pub description: String,
    pub content: String,
    pub scope: crate::tui::commands::handlers::dispatch::CommandScope,
    pub namespace: Option<String>,
}

/// Load custom commands from project and user directories
pub fn load_custom_commands(project_root: &Path) -> HashMap<String, CustomCommand> {
    let mut commands = HashMap::new();

    // Load project commands (.doge/commands/)
    let project_commands_dir = project_root.join(".doge").join("commands");
    if project_commands_dir.exists() {
        load_commands_from_directory(
            &project_commands_dir,
            crate::tui::commands::handlers::dispatch::CommandScope::Project,
            &mut commands,
        );
    }

    // Load user commands (~/.config/doge-code/commands/)
    if let Some(home_dir) = dirs::home_dir() {
        let user_commands_dir = home_dir.join(".config").join("doge-code").join("commands");
        if user_commands_dir.exists() {
            load_commands_from_directory(
                &user_commands_dir,
                crate::tui::commands::handlers::dispatch::CommandScope::User,
                &mut commands,
            );
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
            if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
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
                        let namespace = path
                            .parent()
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

impl TuiExecutor {
    /// Handle custom slash commands via the shared AgentTurn job path.
    /// History is only updated inside the job lifecycle; a busy rejection
    /// leaves conversation history untouched.
    pub fn handle_custom_command(&mut self, line: &str, ui: &mut TuiApp) {
        // Parse command and arguments
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            return;
        }

        let command_name = parts[0].trim_start_matches('/');
        let args = &parts[1..];

        // Check if command exists
        if let Some(command) = self.custom_commands.get(command_name).cloned() {
            // Process command content with arguments
            let processed_content = process_command_content(&command.content, args);

            // Shared AgentTurn path (no pre-append to history).
            let _ = crate::tui::commands::agent_job::spawn_agent_turn(
                self,
                ui,
                line,
                processed_content,
                false,
            );
        } else {
            ui.push_log(format!("Unknown command: /{}", command_name));
        }
    }
}

/// Process command content by replacing placeholders with arguments
fn process_command_content(content: &str, args: &[&str]) -> String {
    let mut processed = content.to_string();

    // Replace $ARGUMENTS with all arguments joined by space
    if !args.is_empty() {
        let all_args = args.join(" ");
        processed = processed.replace("$ARGUMENTS", &all_args);

        // Replace $1, $2, etc. with specific arguments
        for (i, arg) in args.iter().enumerate() {
            let placeholder = format!("${}", i + 1);
            processed = processed.replace(&placeholder, arg);
        }
    }

    processed
}
