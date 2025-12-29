use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use regex::Regex;
use std::collections::HashMap;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::mpsc::Sender;
use std::thread;

#[derive(Debug, Clone)]
pub struct TestConfig {
    pub commands: Vec<TestCommand>,
}

#[derive(Debug, Clone)]
pub struct TestCommand {
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FailedTest {
    pub name: String,
    pub file_path: Option<String>,
    pub line_number: Option<u32>,
    pub message: String,
    pub expected: Option<String>,
    pub actual: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TestResult {
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub failed_tests: Vec<FailedTest>,
}

/// Run tests for Go, Rust, TypeScript, and Python projects
pub fn handle_test(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    ui.push_log("Running test command...");

    let project_root = executor.cfg.project_root.clone();
    ui.push_log("Running tests in background with TUI spinner...");

    if let Some(ui_tx) = &executor.ui_tx {
        let _ = ui_tx.send("::status:shell_running".to_string());

        let ui_tx_clone = ui_tx.clone();
        let project_root_clone = project_root.clone();

        thread::spawn(move || test_thread(project_root_clone, ui_tx_clone));
    } else {
        ui.push_log("UI channel unavailable - falling back to sync test (TUI may freeze).");
    }
}

/// Detect project languages by checking for common file extensions and configuration files
fn detect_project_languages(project_root: &Path) -> Vec<String> {
    let mut languages = std::collections::HashSet::new();

    // Check for config files first (most reliable)
    if project_root.join("Cargo.toml").exists() {
        languages.insert("rust".to_string());
    }
    if project_root.join("go.mod").exists() {
        languages.insert("go".to_string());
    }
    if project_root.join("package.json").exists() {
        languages.insert("typescript".to_string());
    }
    if project_root.join("pyproject.toml").exists()
        || project_root.join("setup.py").exists()
        || project_root.join("requirements.txt").exists()
    {
        languages.insert("python".to_string());
    }

    // Also scan for source files
    scan_directory_for_languages(project_root, &mut languages, 0);

    languages.into_iter().collect()
}

/// Scan a directory for language files (with depth limit)
fn scan_directory_for_languages(
    dir: &Path,
    languages: &mut std::collections::HashSet<String>,
    depth: usize,
) {
    if depth > 3 || !dir.exists() {
        return;
    }

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Ok(file_type) = entry.file_type() {
                let path = entry.path();

                // Skip common non-source directories
                if let Some(name) = path.file_name().and_then(|s| s.to_str())
                    && matches!(
                        name,
                        "target"
                            | "node_modules"
                            | ".git"
                            | "vendor"
                            | "__pycache__"
                            | ".venv"
                            | "venv"
                    )
                {
                    continue;
                }

                if file_type.is_file() {
                    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
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
                            "py" => {
                                languages.insert("python".to_string());
                            }
                            _ => {}
                        }
                    }
                } else if file_type.is_dir() {
                    scan_directory_for_languages(&path, languages, depth + 1);
                }
            }
        }
    }
}

/// Get test configurations for supported languages
fn get_test_configs(project_root: &Path) -> HashMap<String, TestConfig> {
    let mut configs = HashMap::new();

    // Go tests
    configs.insert(
        "go".to_string(),
        TestConfig {
            commands: vec![TestCommand {
                command: "go".to_string(),
                args: vec!["test".to_string(), "-v".to_string(), "./...".to_string()],
            }],
        },
    );

    // Rust tests
    configs.insert(
        "rust".to_string(),
        TestConfig {
            commands: vec![TestCommand {
                command: "cargo".to_string(),
                args: vec!["test".to_string()],
            }],
        },
    );

    // TypeScript/JavaScript tests
    configs.insert(
        "typescript".to_string(),
        TestConfig {
            commands: typescript_test_commands(project_root),
        },
    );

    // Python tests
    configs.insert(
        "python".to_string(),
        TestConfig {
            commands: python_test_commands(project_root),
        },
    );

    configs
}

fn typescript_test_commands(project_root: &Path) -> Vec<TestCommand> {
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

    // Try common test script names
    for script_name in ["test", "test:unit", "test:all"] {
        if scripts.contains_key(script_name) {
            return vec![TestCommand {
                command: "npm".to_string(),
                args: vec!["run".to_string(), script_name.to_string()],
            }];
        }
    }

    Vec::new()
}

fn python_test_commands(project_root: &Path) -> Vec<TestCommand> {
    // Check if pytest is available
    if project_root.join("pytest.ini").exists()
        || project_root.join("pyproject.toml").exists()
        || project_root.join("setup.cfg").exists()
    {
        return vec![TestCommand {
            command: "pytest".to_string(),
            args: vec!["-v".to_string()],
        }];
    }

    // Fallback to python -m pytest
    vec![TestCommand {
        command: "python".to_string(),
        args: vec!["-m".to_string(), "pytest".to_string(), "-v".to_string()],
    }]
}

fn test_thread(project_root: PathBuf, ui_tx: Sender<String>) {
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
            "::shell_output:No supported languages (Go, Rust, TypeScript, Python) detected."
                .to_string(),
        );
        let _ = ui_tx.send("::status:idle".to_string());
        return;
    }

    let mut all_failed_tests = Vec::new();
    let mut all_command_outputs = Vec::new();
    let mut has_any_failures = false;

    let test_configs = get_test_configs(&project_root);

    // Run tests for each detected language
    for lang in detected_languages {
        let _ = ui_tx.send(format!("::shell_output:\n--- Running {} tests ---", lang));

        if let Some(config) = test_configs.get(&lang) {
            if config.commands.is_empty() {
                let _ = ui_tx.send(format!(
                    "::shell_output:No test commands configured for language '{}'.",
                    lang
                ));
                continue;
            }

            for test_cmd in &config.commands {
                let result =
                    run_command_sync_with_output(&project_root, &test_cmd.command, &test_cmd.args);

                // Store the command output
                all_command_outputs.push(format!(
                    "Command: {} {}\nExit code: {}\nSTDOUT:\n{}\nSTDERR:\n{}",
                    test_cmd.command,
                    test_cmd.args.join(" "),
                    if result.success { 0 } else { 1 },
                    result.stdout,
                    result.stderr
                ));

                // Check for failures
                if !result.success {
                    has_any_failures = true;
                }

                // Send output to UI
                if result.success {
                    let _ = ui_tx.send(format!(
                        "::shell_output:✓ All tests passed: {} {}",
                        test_cmd.command,
                        test_cmd.args.join(" ")
                    ));
                } else {
                    let _ = ui_tx.send(format!(
                        "::shell_output:✗ Tests failed: {} {}",
                        test_cmd.command,
                        test_cmd.args.join(" ")
                    ));
                }

                if !result.stdout.is_empty() {
                    let _ = ui_tx.send(format!("::shell_output:Output:\n{}", result.stdout));
                }

                if !result.stderr.is_empty() {
                    let _ = ui_tx.send(format!("::shell_output:STDERR:\n{}", result.stderr));
                }

                // Parse failed tests
                let failed_tests = parse_test_output(&result, &test_cmd.command, &lang);
                if !failed_tests.is_empty() {
                    let _ = ui_tx.send(format!(
                        "::shell_output:Found {} failed test(s)",
                        failed_tests.len()
                    ));
                }

                all_failed_tests.extend(failed_tests);
            }
        } else {
            let _ = ui_tx.send(format!(
                "::shell_output:No test configuration found for language: {}",
                lang
            ));
        }
    }

    // If there are failed tests, send them to LLM for analysis
    if has_any_failures {
        let all_outputs = all_command_outputs.join("\n\n---\n\n");

        let mut prompt = String::from(
            "The following test(s) have failed. Please analyze the failures and provide fixes:\n\n",
        );
        prompt.push_str(&all_outputs);

        if !all_failed_tests.is_empty() {
            prompt.push_str("\n\n--- Parsed Failed Tests ---\n");
            for (i, test) in all_failed_tests.iter().enumerate() {
                prompt.push_str(&format!("\n{}. Test: {}\n", i + 1, test.name));
                if let Some(file) = &test.file_path {
                    prompt.push_str(&format!("   File: {}\n", file));
                }
                if let Some(line) = test.line_number {
                    prompt.push_str(&format!("   Line: {}\n", line));
                }
                prompt.push_str(&format!("   Message: {}\n", test.message));
                if let Some(expected) = &test.expected {
                    prompt.push_str(&format!("   Expected: {}\n", expected));
                }
                if let Some(actual) = &test.actual {
                    prompt.push_str(&format!("   Actual: {}\n", actual));
                }
            }
        }

        prompt.push_str("\n\nPlease analyze the test failures above. For each failure:\n1. Identify the root cause\n2. Read the relevant source files if needed\n3. Provide specific code fixes\n\nFocus on fixing the actual code bugs, not modifying the tests (unless the tests themselves are incorrect).");

        let _ = ui_tx.send(
            "::shell_output:\nSending test failures to LLM for analysis and fixes...".to_string(),
        );
        let _ = ui_tx.send(format!("::test_failures_analysis:{}", prompt));
    } else {
        let _ = ui_tx.send("::shell_output:\n✓ All tests passed!".to_string());
    }

    let _ = ui_tx.send("::shell_output:Test run completed.".to_string());
    let _ = ui_tx.send("::status:idle".to_string());
}

fn run_command_sync_with_output(project_root: &Path, cmd: &str, args: &[String]) -> TestResult {
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

    TestResult {
        command: format!("{} {}", cmd, args.join(" ")),
        stdout,
        stderr,
        success,
        failed_tests: Vec::new(),
    }
}

fn parse_test_output(result: &TestResult, command: &str, language: &str) -> Vec<FailedTest> {
    match (command, language) {
        ("cargo", "rust") => parse_rust_test_output(&result.stdout, &result.stderr),
        ("go", "go") => parse_go_test_output(&result.stdout, &result.stderr),
        ("npm" | "npx", "typescript") => {
            parse_javascript_test_output(&result.stdout, &result.stderr)
        }
        ("pytest" | "python", "python") => parse_pytest_output(&result.stdout, &result.stderr),
        _ => Vec::new(),
    }
}

fn parse_rust_test_output(stdout: &str, stderr: &str) -> Vec<FailedTest> {
    let mut failed_tests = Vec::new();
    let combined = format!("{}\n{}", stdout, stderr);

    // Pattern: test module::test_name ... FAILED
    let re_failed = Regex::new(r"^test\s+(\S+)\s+\.\.\.\s+FAILED").unwrap();
    // Pattern: thread 'test_name' panicked at file:line:col
    let re_panic = Regex::new(r"thread '([^']+)' panicked at (.+):(\d+):(\d+)").unwrap();
    // Pattern for assertion: assertion `left == right` failed
    let re_assertion =
        Regex::new(r"assertion `(.+)` failed\s*\n\s*left:\s*`([^`]+)`\s*\n\s*right:\s*`([^`]+)`")
            .unwrap();

    let mut current_test: Option<String> = None;
    let mut current_file: Option<String> = None;
    let mut current_line: Option<u32> = None;
    let mut current_message = String::new();

    for line in combined.lines() {
        if let Some(captures) = re_failed.captures(line) {
            // Save previous test if exists
            if let Some(name) = current_test.take() {
                failed_tests.push(FailedTest {
                    name,
                    file_path: current_file.take(),
                    line_number: current_line.take(),
                    message: current_message.clone(),
                    expected: None,
                    actual: None,
                });
                current_message.clear();
            }
            current_test = Some(captures.get(1).unwrap().as_str().to_string());
        } else if let Some(captures) = re_panic.captures(line) {
            current_file = Some(captures.get(2).unwrap().as_str().to_string());
            current_line = captures.get(3).unwrap().as_str().parse().ok();
        }
    }

    // Check for assertion details
    if let Some(captures) = re_assertion.captures(&combined) {
        for test in &mut failed_tests {
            test.expected = Some(captures.get(2).unwrap().as_str().to_string());
            test.actual = Some(captures.get(3).unwrap().as_str().to_string());
        }
    }

    // Add last test if exists
    if let Some(name) = current_test {
        failed_tests.push(FailedTest {
            name,
            file_path: current_file,
            line_number: current_line,
            message: current_message,
            expected: None,
            actual: None,
        });
    }

    failed_tests
}

fn parse_go_test_output(stdout: &str, stderr: &str) -> Vec<FailedTest> {
    let mut failed_tests = Vec::new();
    let combined = format!("{}\n{}", stdout, stderr);

    // Pattern: --- FAIL: TestName (duration)
    let re_fail = Regex::new(r"---\s*FAIL:\s*(\S+)\s*\(").unwrap();
    // Pattern: file_test.go:123: error message
    let re_location = Regex::new(r"^\s*(\S+\.go):(\d+):\s*(.+)$").unwrap();

    let mut current_test: Option<String> = None;
    let mut current_file: Option<String> = None;
    let mut current_line: Option<u32> = None;
    let mut current_message = String::new();

    for line in combined.lines() {
        if let Some(captures) = re_fail.captures(line) {
            // Save previous test
            if let Some(name) = current_test.take() {
                failed_tests.push(FailedTest {
                    name,
                    file_path: current_file.take(),
                    line_number: current_line.take(),
                    message: current_message.clone(),
                    expected: None,
                    actual: None,
                });
                current_message.clear();
            }
            current_test = Some(captures.get(1).unwrap().as_str().to_string());
        } else if current_test.is_some()
            && let Some(captures) = re_location.captures(line)
        {
            current_file = Some(captures.get(1).unwrap().as_str().to_string());
            current_line = captures.get(2).unwrap().as_str().parse().ok();
            current_message = captures.get(3).unwrap().as_str().to_string();
        }
    }

    // Add last test
    if let Some(name) = current_test {
        failed_tests.push(FailedTest {
            name,
            file_path: current_file,
            line_number: current_line,
            message: current_message,
            expected: None,
            actual: None,
        });
    }

    failed_tests
}

fn parse_javascript_test_output(stdout: &str, stderr: &str) -> Vec<FailedTest> {
    let mut failed_tests = Vec::new();
    let combined = format!("{}\n{}", stdout, stderr);

    // Jest/Vitest pattern: ✕ test name (duration)
    let re_fail = Regex::new(r"[✕✗×]\s+(.+?)\s*(?:\(\d+\s*ms\))?$").unwrap();
    // At path:line pattern
    let re_location = Regex::new(r"at\s+\S+\s+\((.+):(\d+):(\d+)\)").unwrap();

    for line in combined.lines() {
        if let Some(captures) = re_fail.captures(line) {
            let name = captures.get(1).unwrap().as_str().to_string();

            // Try to find location in nearby lines
            let mut file_path = None;
            let mut line_number = None;

            if let Some(loc_captures) = re_location.captures(&combined) {
                file_path = Some(loc_captures.get(1).unwrap().as_str().to_string());
                line_number = loc_captures.get(2).unwrap().as_str().parse().ok();
            }

            failed_tests.push(FailedTest {
                name,
                file_path,
                line_number,
                message: String::new(),
                expected: None,
                actual: None,
            });
        }
    }

    failed_tests
}

fn parse_pytest_output(stdout: &str, stderr: &str) -> Vec<FailedTest> {
    let mut failed_tests = Vec::new();
    let combined = format!("{}\n{}", stdout, stderr);

    // Pattern: FAILED test_file.py::test_name - AssertionError
    let re_fail = Regex::new(r"FAILED\s+(\S+)::(\S+)\s*(?:-\s*(.+))?$").unwrap();

    for line in combined.lines() {
        if let Some(captures) = re_fail.captures(line) {
            let file_path = captures.get(1).unwrap().as_str().to_string();
            let name = captures.get(2).unwrap().as_str().to_string();
            let message = captures
                .get(3)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();

            failed_tests.push(FailedTest {
                name,
                file_path: Some(file_path),
                line_number: None,
                message,
                expected: None,
                actual: None,
            });
        }
    }

    failed_tests
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::TempDir;

    #[test]
    fn test_detect_rust_project() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = temp_dir.path();

        File::create(project_path.join("Cargo.toml")).unwrap();

        let detected = detect_project_languages(project_path);
        assert!(detected.contains(&"rust".to_string()));
    }

    #[test]
    fn test_detect_go_project() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = temp_dir.path();

        File::create(project_path.join("go.mod")).unwrap();

        let detected = detect_project_languages(project_path);
        assert!(detected.contains(&"go".to_string()));
    }

    #[test]
    fn test_detect_python_project() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = temp_dir.path();

        File::create(project_path.join("pyproject.toml")).unwrap();

        let detected = detect_project_languages(project_path);
        assert!(detected.contains(&"python".to_string()));
    }

    #[test]
    fn test_typescript_test_commands_with_test_script() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = temp_dir.path();

        std::fs::write(
            project_path.join("package.json"),
            r#"{"scripts":{"test":"jest"}}"#,
        )
        .unwrap();

        let commands = typescript_test_commands(project_path);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command, "npm");
        assert_eq!(
            commands[0].args,
            vec!["run".to_string(), "test".to_string()]
        );
    }

    #[test]
    fn test_typescript_test_commands_empty_when_no_test_script() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = temp_dir.path();

        std::fs::write(
            project_path.join("package.json"),
            r#"{"scripts":{"build":"tsc"}}"#,
        )
        .unwrap();

        let commands = typescript_test_commands(project_path);
        assert!(commands.is_empty());
    }

    #[test]
    fn test_parse_rust_test_output() {
        let stdout = r#"
running 2 tests
test tests::test_success ... ok
test tests::test_failure ... FAILED

failures:

---- tests::test_failure stdout ----
thread 'tests::test_failure' panicked at src/lib.rs:10:5:
assertion `left == right` failed
  left: `1`
 right: `2`
"#;
        let failed = parse_rust_test_output(stdout, "");
        assert!(!failed.is_empty());
        assert!(failed.iter().any(|t| t.name.contains("test_failure")));
    }

    #[test]
    fn test_parse_go_test_output() {
        let stdout = r#"
=== RUN   TestAdd
--- FAIL: TestAdd (0.00s)
    math_test.go:10: expected 3, got 2
FAIL
"#;
        let failed = parse_go_test_output(stdout, "");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].name, "TestAdd");
        assert_eq!(failed[0].file_path, Some("math_test.go".to_string()));
    }

    #[test]
    fn test_parse_pytest_output() {
        let stdout = r#"
FAILED test_math.py::test_add - AssertionError: assert 2 == 3
"#;
        let failed = parse_pytest_output(stdout, "");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].name, "test_add");
        assert_eq!(failed[0].file_path, Some("test_math.py".to_string()));
    }
}
