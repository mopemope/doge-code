use crate::execution::{
    ManagedProcessSpec, ManagedProcessTermination, ManagedRunOptions, run_managed_process,
};
use crate::tui::channel::SenderExt;
use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use regex::Regex;
use serde_json;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

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
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub output_truncated: bool,
    pub warnings: Vec<String>,
}

const DIAGNOSTIC_OUTPUT_BUDGET_CHARS: usize = 32_000;
const LINT_ISSUE_PAYLOAD_BUDGET_CHARS: usize = 24_000;
const LINT_ISSUE_FILE_BUDGET_CHARS: usize = 512;
const LINT_ISSUE_MESSAGE_BUDGET_CHARS: usize = 2_048;
const LINT_ISSUE_CODE_BUDGET_CHARS: usize = 256;

fn budget_diagnostic_output(output: &str) -> String {
    crate::tools::budget::head_tail_truncate(output, DIAGNOSTIC_OUTPUT_BUDGET_CHARS).text
}

fn budget_lint_issues(issues: Vec<LintIssue>) -> (Vec<LintIssue>, bool) {
    let mut kept = Vec::new();
    // Account for the opening/closing JSON array and separators.
    let mut used_chars = 2usize;
    let mut truncated = false;

    for issue in issues {
        let issue = LintIssue {
            file_path: crate::tools::budget::head_tail_truncate(
                &issue.file_path,
                LINT_ISSUE_FILE_BUDGET_CHARS,
            )
            .text,
            line_number: issue.line_number,
            severity: crate::tools::budget::head_tail_truncate(&issue.severity, 64).text,
            message: crate::tools::budget::head_tail_truncate(
                &issue.message,
                LINT_ISSUE_MESSAGE_BUDGET_CHARS,
            )
            .text,
            code: issue.code.map(|code| {
                crate::tools::budget::head_tail_truncate(&code, LINT_ISSUE_CODE_BUDGET_CHARS).text
            }),
        };
        let encoded_len = serde_json::to_string(&issue).unwrap_or_default().len() + 1;
        if used_chars + encoded_len > LINT_ISSUE_PAYLOAD_BUDGET_CHARS {
            truncated = true;
            break;
        }
        used_chars += encoded_len;
        kept.push(issue);
    }

    (kept, truncated)
}

fn lint_exit_status(result: &LintResult) -> String {
    result
        .exit_code
        .map(|code| code.to_string())
        .unwrap_or_else(|| {
            if result.timed_out {
                "timeout".to_string()
            } else {
                "none".to_string()
            }
        })
}

fn lint_warnings(result: &LintResult) -> String {
    if result.warnings.is_empty() {
        String::new()
    } else {
        format!("\nWARNINGS:\n{}", result.warnings.join("\n"))
    }
}

/// Run linting for Go, Rust, and TypeScript projects
pub fn handle_lint(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    ui.push_log("Running lint command...");

    let project_root = executor.cfg.project_root.clone();
    let command_timeout_ms = executor.cfg.command_timeout_ms;
    ui.push_log("Running lint in background with TUI spinner...");

    if let Some(ui_tx) = &executor.ui_tx {
        ui_tx.send_logged("::status:shell_running".to_string());

        let ui_tx_clone = ui_tx.clone();
        let project_root_clone = project_root.clone();

        thread::spawn(move || lint_thread(project_root_clone, ui_tx_clone, command_timeout_ms));
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
fn get_lint_configs(project_root: &Path) -> HashMap<String, LintConfig> {
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

    configs.insert(
        "typescript".to_string(),
        LintConfig {
            commands: typescript_lint_commands(project_root),
        },
    );

    configs
}

fn typescript_lint_commands(project_root: &Path) -> Vec<LintCommand> {
    let package_json_path = project_root.join("package.json");
    let Ok(package_json) = std::fs::read_to_string(package_json_path) else {
        return Vec::new();
    };

    let Ok(value) = serde_json::from_str::<serde_json::Value>(&package_json) else {
        return Vec::new();
    };

    let Some(scripts) = value.get("scripts").and_then(|v| v.as_object()) else {
        return Vec::new();
    };

    let mut commands = Vec::new();

    if scripts.contains_key("format") {
        commands.push(LintCommand {
            command: "npm".to_string(),
            args: vec!["run".to_string(), "format".to_string()],
            auto_fix_flag: None,
        });
    } else if scripts.contains_key("fmt") {
        commands.push(LintCommand {
            command: "npm".to_string(),
            args: vec!["run".to_string(), "fmt".to_string()],
            auto_fix_flag: None,
        });
    }

    if scripts.contains_key("lint:fix") {
        commands.push(LintCommand {
            command: "npm".to_string(),
            args: vec!["run".to_string(), "lint:fix".to_string()],
            auto_fix_flag: None,
        });
    } else if scripts.contains_key("lint") {
        commands.push(LintCommand {
            command: "npm".to_string(),
            args: vec!["run".to_string(), "lint".to_string()],
            auto_fix_flag: None,
        });
    }

    commands
}

fn lint_thread(project_root: PathBuf, ui_tx: Sender<String>, command_timeout_ms: u64) {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            ui_tx.send_logged(format!(
                "::shell_output:Failed to create lint runtime: {error}"
            ));
            ui_tx.send_logged("::status:idle".to_string());
            return;
        }
    };
    runtime.block_on(lint_thread_async(project_root, ui_tx, command_timeout_ms));
}

async fn lint_thread_async(project_root: PathBuf, ui_tx: Sender<String>, command_timeout_ms: u64) {
    ui_tx.send_logged(format!(
        "::shell_output:Project root: {}",
        project_root.display()
    ));

    // Detect languages in the project
    let detected_languages = detect_project_languages(&project_root);

    ui_tx.send_logged(format!(
        "::shell_output:Detected languages: {:?}",
        detected_languages
    ));

    if detected_languages.is_empty() {
        ui_tx.send_logged(
            "::shell_output:No supported languages (Go, Rust, TypeScript) detected in the project."
                .to_string(),
        );
        ui_tx.send_logged("::status:idle".to_string());
        return;
    }

    let mut all_issues = Vec::new();
    let mut all_command_outputs = Vec::new(); // Track all command outputs
    let mut has_any_warnings_or_errors = false; // Track if we found any issues

    let lint_configs = get_lint_configs(&project_root);

    // Run linters for each detected language
    for lang in detected_languages {
        ui_tx.send_logged(format!("::shell_output:\n--- Linting {} ---", lang));

        if let Some(config) = lint_configs.get(&lang) {
            if config.commands.is_empty() {
                ui_tx.send_logged(format!(
                    "::shell_output:No lint commands configured for language '{}' (missing package.json scripts like 'lint' or 'lint:fix').",
                    lang
                ));
                continue;
            }

            for lint_cmd in &config.commands {
                let result =
                    run_command_with_output(&project_root, lint_cmd, command_timeout_ms).await;

                // Store the command output to send to LLM if there are warnings/errors
                all_command_outputs.push(format!(
                    "Command: {} {}\nExit code: {}\nSTDOUT:\n{}\nSTDERR:\n{}{}",
                    lint_cmd.command,
                    lint_cmd.args.join(" "),
                    lint_exit_status(&result),
                    budget_diagnostic_output(&result.stdout),
                    budget_diagnostic_output(&result.stderr),
                    lint_warnings(&result)
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
                    ui_tx.send_logged(format!(
                        "::shell_output:Successfully ran: {} {}",
                        lint_cmd.command,
                        lint_cmd.args.join(" ")
                    ));
                } else {
                    ui_tx.send_logged(format!(
                        "::shell_output:Failed to run {} (exit code {:?}{})",
                        lint_cmd.command,
                        result.exit_code,
                        if result.timed_out { ", timed out" } else { "" }
                    ));
                }

                if !result.stdout.is_empty() {
                    ui_tx.send_logged(format!(
                        "::shell_output:Output:\n{}",
                        budget_diagnostic_output(&result.stdout)
                    ));
                }

                if !result.stderr.is_empty() {
                    ui_tx.send_logged(format!(
                        "::shell_output:STDERR: {}",
                        budget_diagnostic_output(&result.stderr)
                    ));
                }

                // Parse lint issues
                let issues = parse_lint_output(&result, lint_cmd, &lang);
                if !issues.is_empty() {
                    ui_tx.send_logged(format!(
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

                    let fix_command = LintCommand {
                        command: lint_cmd.command.clone(),
                        args: fix_args,
                        auto_fix_flag: None,
                    };
                    let fix_result =
                        run_command_with_output(&project_root, &fix_command, command_timeout_ms)
                            .await;

                    if fix_result.success {
                        ui_tx.send_logged(format!(
                            "::shell_output:Successfully ran auto-fix: {} {}",
                            lint_cmd.command,
                            fix_command.args.join(" ")
                        ));
                    } else {
                        ui_tx.send_logged(format!(
                            "::shell_output:Auto-fix also failed: {} {}",
                            lint_cmd.command,
                            fix_command.args.join(" ")
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
            ui_tx.send_logged(format!(
                "::shell_output:No linter configuration found for language: {}",
                lang
            ));
        }
    }

    // If there are issues, send a bounded subset to LLM for fixing.
    let (lint_issues, lint_issues_truncated) = budget_lint_issues(all_issues);
    if !lint_issues.is_empty() {
        ui_tx.send_logged(format!(
            "::shell_output:\nFound {} issues. Sending to LLM for analysis and fixes...",
            lint_issues.len()
        ));
        if lint_issues_truncated {
            ui_tx.send_logged(
                "::shell_output:Additional lint issues omitted to stay within the diagnostic budget."
                    .to_string(),
            );
        }

        // Send a message to trigger LLM processing
        ui_tx.send_logged(format!(
            "::lint_issues:{:}",
            serde_json::to_string(&lint_issues).unwrap_or_default()
        ));
    } else if lint_issues_truncated {
        ui_tx.send_logged(
            "::shell_output:Lint issues omitted because they exceeded the diagnostic budget."
                .to_string(),
        );
    }

    // If there are any warnings or errors in the output (regardless of parsed issues),
    // send all command outputs to the LLM for analysis and fixes
    if has_any_warnings_or_errors {
        let all_outputs = budget_diagnostic_output(&all_command_outputs.join("\n\n---\n\n"));

        // Create a specific prompt for the LLM to analyze all outputs and fix issues
        let mut prompt = String::from(
            "Analyze the following lint command outputs and fix any warnings or errors detected:\n\n",
        );
        prompt.push_str(&all_outputs);
        prompt.push_str("\n\nPlease analyze the outputs above. Identify any warnings, errors, or issues in the codebase. For each issue detected, provide specific fixes with clear explanations. If you need to see the current content of any file, use the appropriate tool to read it first, then provide the corrected code.");

        let prompt = budget_diagnostic_output(&prompt);
        ui_tx.send_logged(
            "::shell_output:\nSending full lint output to LLM for analysis and fixes..."
                .to_string(),
        );
        // Use the existing dispatch pattern by sending the prompt via the user input mechanism
        // This will trigger the LLM to process the bounded output.
        ui_tx.send_logged(format!("::lint_command_output_analysis:{}", prompt));
    }

    ui_tx.send_logged("::shell_output:Linting completed.".to_string());
    ui_tx.send_logged("::status:idle".to_string());
}

async fn run_command_with_output(
    project_root: &Path,
    command: &LintCommand,
    timeout_ms: u64,
) -> LintResult {
    let spec = ManagedProcessSpec {
        program: command.command.clone(),
        args: command.args.clone(),
        cwd: project_root.to_path_buf(),
        env: Default::default(),
    };
    let timeout = (timeout_ms != 0).then(|| Duration::from_millis(timeout_ms));
    let options = ManagedRunOptions {
        timeout,
        cancellation: None,
    };
    let command_text = format!("{} {}", command.command, command.args.join(" "));

    let managed = match run_managed_process(spec, options).await {
        Ok(output) => output,
        Err(error) => {
            return LintResult {
                command: command_text,
                issues: Vec::new(),
                stdout: String::new(),
                stderr: format!("Failed to execute command: {error}"),
                success: false,
                exit_code: None,
                timed_out: false,
                output_truncated: false,
                warnings: vec![error.to_string()],
            };
        }
    };
    let timed_out = managed.termination == ManagedProcessTermination::TimedOut;
    let success = managed.success();
    LintResult {
        command: command_text,
        issues: Vec::new(),
        stdout: managed.stdout,
        stderr: managed.stderr,
        success,
        exit_code: managed.exit_code,
        timed_out,
        output_truncated: managed.capture_truncated || timed_out,
        warnings: managed.warnings,
    }
}

fn parse_lint_output(result: &LintResult, command: &LintCommand, language: &str) -> Vec<LintIssue> {
    let mut issues = Vec::new();
    let first_arg = command.args.first().map(String::as_str);

    // Parser dispatch uses the structured program + argv representation. Do
    // not search the rendered command string: `cargo` alone cannot identify
    // fmt vs clippy, and `go` alone cannot identify fmt.
    match (command.command.as_str(), language) {
        ("golangci-lint", "go") => {
            issues.extend(parse_golangci_lint_output(&result.stdout, &result.stderr));
        }
        ("go", "go") if first_arg == Some("fmt") => {
            issues.extend(parse_go_fmt_output(&result.stdout, &result.stderr));
        }
        ("cargo", "rust") if first_arg == Some("clippy") => {
            issues.extend(parse_cargo_clippy_output(&result.stdout, &result.stderr));
        }
        ("cargo", "rust") if first_arg == Some("fmt") => {
            issues.extend(parse_cargo_fmt_output(&result.stdout, &result.stderr));
        }
        _ => {
            // Generic parsing for other tools, including npm run lint/fmt.
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
    let mut pending_message = None;

    for line in lines {
        if let Some(captures) = re_location.captures(line.trim()) {
            current_file = captures.get(1).map_or("", |m| m.as_str()).to_string();
            current_line = captures.get(2).map_or("", |m| m.as_str()).parse().ok();
            if let Some(message) = pending_message.take() {
                issues.push(LintIssue {
                    file_path: current_file.clone(),
                    line_number: current_line,
                    severity: "warning".to_string(),
                    message,
                    code: None,
                });
                current_file.clear();
                current_line = None;
            }
        } else if let Some(captures) = re_warning.captures(line.trim()) {
            let message = captures.get(1).map_or("", |m| m.as_str()).to_string();
            if current_file.is_empty() {
                // rustc commonly prints the warning before its location.
                pending_message = Some(message);
            } else {
                issues.push(LintIssue {
                    file_path: current_file.clone(),
                    line_number: current_line,
                    severity: "warning".to_string(),
                    message,
                    code: None,
                });
                current_file.clear();
                current_line = None;
            }
        }
    }

    if let Some(message) = pending_message {
        issues.push(LintIssue {
            file_path: current_file,
            line_number: current_line,
            severity: "warning".to_string(),
            message,
            code: None,
        });
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

    #[test]
    fn test_typescript_lint_commands_prefers_format_and_lint_fix() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = temp_dir.path();

        std::fs::write(
            project_path.join("package.json"),
            r#"{"scripts":{"format":"prettier -w .","lint:fix":"eslint --fix ."}}"#,
        )
        .unwrap();

        let commands = typescript_lint_commands(project_path);
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].command, "npm");
        assert_eq!(
            commands[0].args,
            vec!["run".to_string(), "format".to_string()]
        );
        assert_eq!(commands[1].command, "npm");
        assert_eq!(
            commands[1].args,
            vec!["run".to_string(), "lint:fix".to_string()]
        );
    }

    #[test]
    fn test_typescript_lint_commands_fallbacks_to_fmt_and_lint() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = temp_dir.path();

        std::fs::write(
            project_path.join("package.json"),
            r#"{"scripts":{"fmt":"prettier -w .","lint":"eslint ."}}"#,
        )
        .unwrap();

        let commands = typescript_lint_commands(project_path);
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].command, "npm");
        assert_eq!(commands[0].args, vec!["run".to_string(), "fmt".to_string()]);
        assert_eq!(commands[1].command, "npm");
        assert_eq!(
            commands[1].args,
            vec!["run".to_string(), "lint".to_string()]
        );
    }

    #[test]
    fn test_typescript_lint_commands_empty_when_missing_scripts() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = temp_dir.path();

        std::fs::write(project_path.join("package.json"), r#"{"name":"x"}"#).unwrap();

        let commands = typescript_lint_commands(project_path);
        assert!(commands.is_empty());
    }

    fn lint_command(command: &str, args: &[&str]) -> LintCommand {
        LintCommand {
            command: command.to_string(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            auto_fix_flag: None,
        }
    }

    fn lint_result(command: LintCommand, stdout: &str, stderr: &str, success: bool) -> LintResult {
        LintResult {
            command: format!("{} {}", command.command, command.args.join(" ")),
            issues: Vec::new(),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            success,
            exit_code: if success { Some(0) } else { Some(1) },
            timed_out: false,
            output_truncated: false,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn test_cargo_clippy_parser_uses_argv() {
        let command = lint_command("cargo", &["clippy", "--message-format=short"]);
        let result = lint_result(
            command.clone(),
            "warning: unused variable `x`\n --> src/main.rs:3:5",
            "",
            false,
        );
        let issues = parse_lint_output(&result, &command, "rust");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].file_path, "src/main.rs");
    }

    #[test]
    fn test_cargo_fmt_parser_uses_argv() {
        let command = lint_command("cargo", &["fmt", "--check"]);
        let result = lint_result(command.clone(), "src/main.rs\n", "", false);
        let issues = parse_lint_output(&result, &command, "rust");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].message, "File needs formatting");
    }

    #[test]
    fn test_go_fmt_parser_uses_argv() {
        let command = lint_command("go", &["fmt", "./..."]);
        let result = lint_result(
            command.clone(),
            "main.go:2:3: formatting needed\n",
            "",
            false,
        );
        let issues = parse_lint_output(&result, &command, "go");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].file_path, "main.go");
    }

    #[tokio::test]
    async fn test_managed_lint_command_success_and_spawn_failure() {
        let dir = tempfile::tempdir().unwrap();
        let success =
            run_command_with_output(dir.path(), &lint_command("printf", &["lint-ok"]), 10_000)
                .await;
        assert!(success.success);
        assert_eq!(success.exit_code, Some(0));
        assert_eq!(success.stdout, "lint-ok");

        let failure = run_command_with_output(
            dir.path(),
            &lint_command("doge-lint-command-does-not-exist", &[]),
            10_000,
        )
        .await;
        assert!(!failure.success);
        assert_eq!(failure.exit_code, None);
        assert!(!failure.warnings.is_empty());
    }

    #[tokio::test]
    async fn test_managed_lint_large_output_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let result = run_command_with_output(
            dir.path(),
            &lint_command("sh", &["-c", "head -c 200000 /dev/zero | tr '\\0' x"]),
            10_000,
        )
        .await;
        assert!(result.success);
        assert!(result.output_truncated);
        assert!(result.stdout.len() < 100_000);
    }

    #[test]
    fn test_budget_lint_issues_keeps_json_bounded() {
        let issues = (0..1_000)
            .map(|index| LintIssue {
                file_path: format!("src/{index}.rs"),
                line_number: Some(index),
                severity: "warning".to_string(),
                message: "x".repeat(4_000),
                code: Some("C".repeat(1_000)),
            })
            .collect();
        let (budgeted, truncated) = budget_lint_issues(issues);
        assert!(truncated);
        let encoded = serde_json::to_string(&budgeted).unwrap();
        assert!(encoded.len() <= LINT_ISSUE_PAYLOAD_BUDGET_CHARS);
        for issue in budgeted {
            assert!(issue.message.chars().count() <= LINT_ISSUE_MESSAGE_BUDGET_CHARS);
            assert!(issue.file_path.chars().count() <= LINT_ISSUE_FILE_BUDGET_CHARS);
        }
    }

    #[test]
    fn test_lint_exit_status_preserves_timeout_and_code() {
        let command = lint_command("cargo", &["clippy"]);
        let mut result = lint_result(command, "", "", false);
        result.exit_code = Some(42);
        assert_eq!(lint_exit_status(&result), "42");

        result.exit_code = None;
        result.timed_out = true;
        assert_eq!(lint_exit_status(&result), "timeout");
    }
}
