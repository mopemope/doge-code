use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command as StdCommand;

/// Configuration for linters per language
#[derive(Debug, Clone)]
pub struct LintConfig {
    pub commands: Vec<LintCommand>,
}

#[derive(Debug, Clone)]
pub struct LintCommand {
    pub command: String,
    pub args: Vec<String>,
    pub auto_fix_flag: Option<String>,
}

/// Run linting for Go, Rust, and TypeScript projects
pub fn handle_lint(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    ui.push_log("Running lint command...");

    let project_root = &executor.cfg.project_root;
    ui.push_log(format!("Project root: {}", project_root.display()));

    // Detect languages in the project
    let detected_languages = detect_project_languages(project_root);

    ui.push_log(format!("Detected languages: {:?}", detected_languages));

    if detected_languages.is_empty() {
        ui.push_log("No supported languages (Go, Rust, TypeScript) detected in the project.");
        return;
    }

    // Run linters for each detected language
    for lang in detected_languages {
        run_language_linter(executor, ui, &lang);
    }

    ui.push_log("Linting completed.");
}

/// Detect project languages by checking for common file extensions and configuration files
fn detect_project_languages(project_root: &Path) -> Vec<String> {
    let mut languages = std::collections::HashSet::new();

    // Walk through the project directory
    if let Ok(entries) = std::fs::read_dir(project_root) {
        for entry in entries.flatten() {
            if let Ok(file_type) = entry.file_type() {
                if file_type.is_file() {
                    // Check file extensions
                    if let Some(ext) = entry.path().extension().and_then(|s| s.to_str()) {
                        match ext {
                            "go" => {
                                languages.insert("go".to_string());
                            }
                            "rs" => {
                                languages.insert("rust".to_string());
                            }
                            "ts" | "tsx" | "js" | "jsx" => {
                                languages.insert("typescript".to_string());
                            }
                            _ => {}
                        }
                    }

                    // Check for config files
                    if let Some(file_name) = entry.path().file_name().and_then(|s| s.to_str()) {
                        match file_name {
                            "go.mod" => {
                                languages.insert("go".to_string());
                            }
                            "Cargo.toml" => {
                                languages.insert("rust".to_string());
                            }
                            "package.json" => {
                                languages.insert("typescript".to_string());
                            }
                            _ => {}
                        }
                    }
                } else if file_type.is_dir() {
                    // For directories, check if they look like source directories
                    if let Some(dir_name) = entry.path().file_name().and_then(|s| s.to_str())
                        && (dir_name == "go"
                            || dir_name == "src"
                            || dir_name == "lib"
                            || dir_name == "test")
                    {
                        // Do a deeper scan in these directories
                        scan_directory_for_languages(&entry.path(), &mut languages);
                    }
                }
            }
        }
    }

    // Also scan common source directories
    scan_directory_for_languages(&project_root.join("src"), &mut languages);
    scan_directory_for_languages(&project_root.join("lib"), &mut languages);
    scan_directory_for_languages(&project_root.join("cmd"), &mut languages);
    scan_directory_for_languages(&project_root.join("internal"), &mut languages);
    scan_directory_for_languages(&project_root.join("pkg"), &mut languages);
    scan_directory_for_languages(&project_root.join("tests"), &mut languages);
    scan_directory_for_languages(&project_root.join("test"), &mut languages);

    languages.into_iter().collect()
}

/// Scan a directory for language files
fn scan_directory_for_languages(dir: &Path, languages: &mut std::collections::HashSet<String>) {
    if !dir.exists() {
        return;
    }

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Ok(file_type) = entry.file_type() {
                if file_type.is_file() {
                    if let Some(ext) = entry.path().extension().and_then(|s| s.to_str()) {
                        match ext {
                            "go" => {
                                languages.insert("go".to_string());
                            }
                            "rs" => {
                                languages.insert("rust".to_string());
                            }
                            "ts" | "tsx" | "js" | "jsx" => {
                                languages.insert("typescript".to_string());
                            }
                            _ => {}
                        }
                    }
                } else if file_type.is_dir() {
                    // Recursively scan subdirectories, but limit depth to avoid performance issues
                    scan_directory_for_languages(&entry.path(), languages);
                }
            }
        }
    }
}

/// Run linters for a specific language
fn run_language_linter(executor: &TuiExecutor, ui: &mut TuiApp, language: &str) {
    ui.push_log(format!("\n--- Linting {} ---", language));

    let lint_configs = get_lint_configs();
    if let Some(config) = lint_configs.get(language) {
        for lint_cmd in &config.commands {
            match run_command(executor, ui, &lint_cmd.command, &lint_cmd.args) {
                Ok(_) => {
                    ui.push_log(format!(
                        "Successfully ran: {} {}",
                        lint_cmd.command,
                        lint_cmd.args.join(" ")
                    ));
                }
                Err(e) => {
                    ui.push_log(format!("Failed to run {}: {}", lint_cmd.command, e));

                    // If the lint command supports auto-fix and it failed, try running with auto-fix
                    if let Some(auto_fix_flag) = &lint_cmd.auto_fix_flag {
                        let mut fix_args = lint_cmd.args.clone();
                        fix_args.push(auto_fix_flag.clone());
                        match run_command(executor, ui, &lint_cmd.command, &fix_args) {
                            Ok(_) => {
                                ui.push_log(format!(
                                    "Successfully ran auto-fix: {} {}",
                                    lint_cmd.command,
                                    fix_args.join(" ")
                                ));
                            }
                            Err(e_fix) => {
                                ui.push_log(format!(
                                    "Auto-fix also failed: {} {}",
                                    lint_cmd.command, e_fix
                                ));
                            }
                        }
                    }
                }
            }
        }
    } else {
        ui.push_log(format!(
            "No linter configuration found for language: {}",
            language
        ));
    }
}

/// Run a shell command in the project directory
fn run_command(
    executor: &TuiExecutor,
    ui: &mut TuiApp,
    cmd: &str,
    args: &[String],
) -> Result<(), String> {
    let project_root = &executor.cfg.project_root;

    ui.push_log(format!("Running: {} {}", cmd, args.join(" ")));

    let output = StdCommand::new(cmd)
        .args(args)
        .current_dir(project_root)
        .output()
        .map_err(|e| format!("Failed to execute command: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        // Only log stderr if there's an error, as stdout might contain important lint results
        if !stderr.is_empty() {
            ui.push_log(format!("STDERR: {}", stderr));
        }
        if !stdout.is_empty() {
            ui.push_log(format!("STDOUT: {}", stdout));
        }
        return Err(format!("Command exited with status: {}", output.status));
    }

    if !stdout.is_empty() {
        ui.push_log(format!("Output:\n{}", stdout));
    }

    Ok(())
}

/// Get lint configurations for supported languages
fn get_lint_configs() -> HashMap<String, LintConfig> {
    let mut configs = HashMap::new();

    // Go linters
    configs.insert(
        "go".to_string(),
        LintConfig {
            commands: vec![
                LintCommand {
                    command: "golangci-lint".to_string(),
                    args: vec!["run".to_string(), "--fix".to_string()],
                    auto_fix_flag: None, // golangci-lint handles fixes internally
                },
                LintCommand {
                    command: "go".to_string(),
                    args: vec!["fmt".to_string(), "./...".to_string()],
                    auto_fix_flag: None, // go fmt fixes automatically
                },
            ],
        },
    );

    // Rust linters
    configs.insert(
        "rust".to_string(),
        LintConfig {
            commands: vec![
                LintCommand {
                    command: "cargo".to_string(),
                    args: vec!["fmt".to_string()],
                    auto_fix_flag: None, // cargo fmt fixes automatically
                },
                LintCommand {
                    command: "cargo".to_string(),
                    args: vec![
                        "clippy".to_string(),
                        "--fix".to_string(),
                        "--allow-dirty".to_string(),
                        "--allow-staged".to_string(),
                    ],
                    auto_fix_flag: None, // cargo clippy handles fixes internally
                },
            ],
        },
    );

    // TODO TypeScript/JavaScript linters biome etc
    configs.insert("typescript".to_string(), LintConfig { commands: vec![] });

    configs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_detect_rust_language() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = temp_dir.path();

        // Create a sample Rust file
        let rust_file = project_path.join("main.rs");
        let mut file = File::create(&rust_file).unwrap();
        writeln!(file, "fn main() {{ println!(\"Hello, world!\"); }}").unwrap();

        let detected = detect_project_languages(project_path);
        assert!(detected.contains(&"rust".to_string()));
    }

    #[test]
    fn test_detect_go_language() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = temp_dir.path();

        // Create a sample Go file
        let go_file = project_path.join("main.go");
        let mut file = File::create(&go_file).unwrap();
        writeln!(file, "package main").unwrap();
        writeln!(file, "import \"fmt\"").unwrap();
        writeln!(file, "func main() {{ fmt.Println(\"Hello, world!\") }}").unwrap();

        let detected = detect_project_languages(project_path);
        assert!(detected.contains(&"go".to_string()));
    }

    #[test]
    fn test_detect_typescript_language() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = temp_dir.path();

        // Create a sample TypeScript file
        let ts_file = project_path.join("index.ts");
        let mut file = File::create(&ts_file).unwrap();
        writeln!(file, "console.log('Hello, world!');").unwrap();

        let detected = detect_project_languages(project_path);
        assert!(detected.contains(&"typescript".to_string()));
    }

    #[test]
    fn test_detect_languages_with_config_files() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = temp_dir.path();

        // Create config files without source files
        File::create(project_path.join("Cargo.toml")).unwrap();
        File::create(project_path.join("go.mod")).unwrap();
        File::create(project_path.join("package.json")).unwrap();

        let detected = detect_project_languages(project_path);
        assert!(detected.contains(&"rust".to_string()));
        assert!(detected.contains(&"go".to_string()));
        assert!(detected.contains(&"typescript".to_string()));
    }
}
