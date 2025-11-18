use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use regex::Regex;
use serde_json;
use std::collections::HashMap;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::mpsc::Sender;
use std::thread;

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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LintIssue {
    pub file_path: String,
    pub line_number: Option<u32>,
    pub severity: String, // "error", "warning", "note"
    pub message: String,
    pub code: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LintResult {
    pub command: String,
    pub issues: Vec<LintIssue>,
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

/// Run linting for Go, Rust, and TypeScript projects
pub fn handle_lint(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    ui.push_log("Running lint command...");

    let project_root = executor.cfg.project_root.clone();
    ui.push_log("Running lint in background with TUI spinner...");

    if let Some(ui_tx) = &executor.ui_tx {
        let _ = ui_tx.send("::status:shell_running".to_string());

        let ui_tx_clone = ui_tx.clone();
        let project_root_clone = project_root.clone();

        thread::spawn(move || lint_thread(project_root_clone, ui_tx_clone));
    } else {
        ui.push_log("UI channel unavailable - falling back to sync lint (TUI may freeze).");
        // fallback sync logic could be added here if needed
    }
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

fn lint_thread(project_root: PathBuf, ui_tx: Sender<String>) {
    let _ = ui_tx.send(format!(
        "::shell_output:Project root: {}",
        project_root.display()
    ));

    // Detect languages in the project
    let detected_languages = detect_project_languages(&project_root);

    let _ = ui_tx.send(format!(
        "::shell_output:Detected languages: {:?}",
        detected_languages
    ));

    if detected_languages.is_empty() {
        let _ = ui_tx.send(
            "::shell_output:No supported languages (Go, Rust, TypeScript) detected in the project."
                .to_string(),
        );
        let _ = ui_tx.send("::status:idle".to_string());
        return;
    }

    let mut all_issues = Vec::new();
    let mut all_command_outputs = Vec::new(); // Track all command outputs
    let mut has_any_warnings_or_errors = false; // Track if we found any issues

    // Run linters for each detected language
    for lang in detected_languages {
        let _ = ui_tx.send(format!("::shell_output:\\n--- Linting {} ---", lang));

        let lint_configs = get_lint_configs();
        if let Some(config) = lint_configs.get(&lang) {
            for lint_cmd in &config.commands {
                let result =
                    run_command_sync_with_output(&project_root, &lint_cmd.command, &lint_cmd.args);

                // Store the command output to send to LLM if there are warnings/errors
                all_command_outputs.push(format!(
                    "Command: {} {}\nExit code: {}\nSTDOUT:\n{}\nSTDERR:\n{}",
                    lint_cmd.command,
                    lint_cmd.args.join(" "),
                    if result.success { 0 } else { 1 },
                    result.stdout,
                    result.stderr
                ));

                // Check if output contains warnings or errors
                if !result.success
                    || result.stdout.to_lowercase().contains("warning")
                    || result.stdout.to_lowercase().contains("error")
                    || result.stderr.to_lowercase().contains("warning")
                    || result.stderr.to_lowercase().contains("error")
                {
                    has_any_warnings_or_errors = true;
                }

                // Send output to UI
                if result.success {
                    let _ = ui_tx.send(format!(
                        "::shell_output:Successfully ran: {} {}",
                        lint_cmd.command,
                        lint_cmd.args.join(" ")
                    ));
                } else {
                    let _ = ui_tx.send(format!(
                        "::shell_output:Failed to run {}: Command exited with status",
                        lint_cmd.command
                    ));
                }

                if !result.stdout.is_empty() {
                    let _ = ui_tx.send(format!("::shell_output:Output:\\n{}", result.stdout));
                }

                if !result.stderr.is_empty() {
                    let _ = ui_tx.send(format!("::shell_output:STDERR: {}", result.stderr));
                }

                // Parse lint issues
                let issues = parse_lint_output(&result, &lint_cmd.command, &lang);
                if !issues.is_empty() {
                    let _ = ui_tx.send(format!(
                        "::shell_output:Found {} issues from {}",
                        issues.len(),
                        lint_cmd.command
                    ));
                }

                all_issues.extend(issues);

                // If the lint command failed and supports auto-fix, try running with auto-fix
                if !result.success
                    && let Some(auto_fix_flag) = &lint_cmd.auto_fix_flag
                {
                    let mut fix_args = lint_cmd.args.clone();
                    fix_args.push(auto_fix_flag.clone());

                    let fix_result =
                        run_command_sync_with_output(&project_root, &lint_cmd.command, &fix_args);

                    if fix_result.success {
                        let _ = ui_tx.send(format!(
                            "::shell_output:Successfully ran auto-fix: {} {}",
                            lint_cmd.command,
                            fix_args.join(" ")
                        ));
                    } else {
                        let _ = ui_tx.send(format!(
                            "::shell_output:Auto-fix also failed: {} {}",
                            lint_cmd.command,
                            fix_args.join(" ")
                        ));
                    }

                    // Also check the auto-fix output for warnings/errors
                    if !fix_result.success
                        || fix_result.stdout.to_lowercase().contains("warning")
                        || fix_result.stdout.to_lowercase().contains("error")
                        || fix_result.stderr.to_lowercase().contains("warning")
                        || fix_result.stderr.to_lowercase().contains("error")
                    {
                        has_any_warnings_or_errors = true;
                    }
                }
            }
        } else {
            let _ = ui_tx.send(format!(
                "::shell_output:No linter configuration found for language: {}",
                lang
            ));
        }
    }

    // If there are issues, send them to LLM for fixing
    if !all_issues.is_empty() {
        let _ = ui_tx.send(format!(
            "::shell_output:\\nFound {} total issues. Sending to LLM for analysis and fixes...",
            all_issues.len()
        ));

        // Send a message to trigger LLM processing
        let _ = ui_tx.send(format!(
            "::lint_issues:{:}",
            serde_json::to_string(&all_issues).unwrap_or_default()
        ));
    }

    // If there are any warnings or errors in the output (regardless of parsed issues),
    // send all command outputs to the LLM for analysis and fixes
    if has_any_warnings_or_errors {
        let all_outputs = all_command_outputs.join("\n\n---\n\n");

        // Create a specific prompt for the LLM to analyze all outputs and fix issues
        let mut prompt = String::from(
            "Analyze the following lint command outputs and fix any warnings or errors detected:\n\n",
        );
        prompt.push_str(&all_outputs);
        prompt.push_str("\n\nPlease analyze the outputs above. Identify any warnings, errors, or issues in the codebase. For each issue detected, provide specific fixes with clear explanations. If you need to see the current content of any file, use the appropriate tool to read it first, then provide the corrected code.");

        let _ = ui_tx.send(
            "::shell_output:\\nSending full lint output to LLM for analysis and fixes..."
                .to_string(),
        );
        // Use the existing dispatch pattern by sending the prompt via the user input mechanism
        // This will trigger the LLM to process the full output
        let _ = ui_tx.send(format!("::lint_command_output_analysis:{}", prompt));
    }

    let _ = ui_tx.send("::shell_output:Linting completed.".to_string());
    let _ = ui_tx.send("::status:idle".to_string());
}

fn run_command_sync_with_output(project_root: &Path, cmd: &str, args: &[String]) -> LintResult {
    let output = std::process::Command::new(cmd)
        .args(args)
        .current_dir(project_root)
        .output()
        .unwrap_or_else(|e| std::process::Output {
            status: ExitStatus::from_raw(1),
            stdout: format!("Failed to execute command: {}", e).into_bytes(),
            stderr: Vec::new(),
        });

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let success = output.status.success();

    LintResult {
        command: format!("{} {}", cmd, args.join(" ")),
        issues: Vec::new(), // Will be populated by parse_lint_output
        stdout,
        stderr,
        success,
    }
}

fn parse_lint_output(result: &LintResult, command: &str, language: &str) -> Vec<LintIssue> {
    let mut issues = Vec::new();

    // Parse based on the command and language
    match (command, language) {
        ("golangci-lint", "go") => {
            issues.extend(parse_golangci_lint_output(&result.stdout, &result.stderr));
        }
        ("go", "go") => {
            if command.contains("fmt") {
                issues.extend(parse_go_fmt_output(&result.stdout, &result.stderr));
            }
        }
        ("cargo", "rust") => {
            if command.contains("clippy") {
                issues.extend(parse_cargo_clippy_output(&result.stdout, &result.stderr));
            } else if command.contains("fmt") {
                issues.extend(parse_cargo_fmt_output(&result.stdout, &result.stderr));
            }
        }
        _ => {
            // Generic parsing for other tools
            issues.extend(parse_generic_lint_output(&result.stdout, &result.stderr));
        }
    }

    issues
}

fn parse_golangci_lint_output(stdout: &str, stderr: &str) -> Vec<LintIssue> {
    let mut issues = Vec::new();
    let combined = format!("{}\n{}", stdout, stderr);

    // golangci-lint output format: filepath:line:column: message (linter)
    let re = Regex::new(r"^(.+\.go):(\d+):(\d+):\s+(.+?)\s+\[(.+)\]$").unwrap();

    for line in combined.lines() {
        if let Some(captures) = re.captures(line.trim()) {
            let file_path = captures.get(1).map_or("", |m| m.as_str()).to_string();
            let line_number = captures.get(2).map_or("", |m| m.as_str()).parse().ok();
            let message = captures.get(4).map_or("", |m| m.as_str()).to_string();
            let code = captures.get(5).map_or("", |m| m.as_str()).to_string();

            issues.push(LintIssue {
                file_path,
                line_number,
                severity: "error".to_string(),
                message,
                code: Some(code),
            });
        }
    }

    issues
}

fn parse_go_fmt_output(stdout: &str, stderr: &str) -> Vec<LintIssue> {
    let mut issues = Vec::new();
    let combined = format!("{}\n{}", stdout, stderr);

    // go fmt output format: filepath:line:column: message
    let re = Regex::new(r"^(.+\.go):(\d+):(\d+):\s+(.+)$").unwrap();

    for line in combined.lines() {
        if let Some(captures) = re.captures(line.trim()) {
            let file_path = captures.get(1).map_or("", |m| m.as_str()).to_string();
            let line_number = captures.get(2).map_or("", |m| m.as_str()).parse().ok();
            let message = captures.get(4).map_or("", |m| m.as_str()).to_string();

            issues.push(LintIssue {
                file_path,
                line_number,
                severity: "warning".to_string(),
                message,
                code: None,
            });
        }
    }

    issues
}

fn parse_cargo_clippy_output(stdout: &str, stderr: &str) -> Vec<LintIssue> {
    let mut issues = Vec::new();
    let combined = format!("{}\n{}", stdout, stderr);

    // clippy output format: warning: message
    //  --> filepath:line:column
    let re_warning = Regex::new(r"^warning:\s+(.+)$").unwrap();
    let re_location = Regex::new(r"^\s*-->\s+(.+):(\d+):(\d+)$").unwrap();

    let lines: Vec<&str> = combined.lines().collect();
    let mut current_file = String::new();
    let mut current_line = None;

    for line in lines.iter() {
        if let Some(captures) = re_location.captures(line.trim()) {
            current_file = captures.get(1).map_or("", |m| m.as_str()).to_string();
            current_line = captures.get(2).map_or("", |m| m.as_str()).parse().ok();
        } else if let Some(captures) = re_warning.captures(line.trim()) {
            let message = captures.get(1).map_or("", |m| m.as_str()).to_string();

            issues.push(LintIssue {
                file_path: current_file.clone(),
                line_number: current_line,
                severity: "warning".to_string(),
                message,
                code: None,
            });

            // Reset for next iteration
            current_file.clear();
            current_line = None;
        }
    }

    issues
}

fn parse_cargo_fmt_output(stdout: &str, stderr: &str) -> Vec<LintIssue> {
    let mut issues = Vec::new();
    let combined = format!("{}\n{}", stdout, stderr);

    // cargo fmt doesn't typically output structured errors, but we can look for file paths
    let re = Regex::new(r"^(.+\.rs)$").unwrap();

    for line in combined.lines() {
        let line = line.trim();
        if line.ends_with(".rs") && re.is_match(line) {
            issues.push(LintIssue {
                file_path: line.to_string(),
                line_number: None,
                severity: "warning".to_string(),
                message: "File needs formatting".to_string(),
                code: None,
            });
        }
    }

    issues
}

fn parse_generic_lint_output(stdout: &str, stderr: &str) -> Vec<LintIssue> {
    let mut issues = Vec::new();
    let combined = format!("{}\n{}", stdout, stderr);

    // Generic pattern: filepath:line: message
    let re = Regex::new(r"^(.+):(\d+):\s+(.+)$").unwrap();

    for line in combined.lines() {
        if let Some(captures) = re.captures(line.trim()) {
            let file_path = captures.get(1).map_or("", |m| m.as_str()).to_string();
            let line_number = captures.get(2).map_or("", |m| m.as_str()).parse().ok();
            let message = captures.get(3).map_or("", |m| m.as_str()).to_string();

            // Determine severity based on message content
            let severity = if message.to_lowercase().contains("error") {
                "error"
            } else if message.to_lowercase().contains("warning") {
                "warning"
            } else {
                "note"
            }
            .to_string();

            issues.push(LintIssue {
                file_path,
                line_number,
                severity,
                message,
                code: None,
            });
        }
    }

    issues
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
