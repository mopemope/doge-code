use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;
use tracing::debug;

/// Represents a single frame in a stack trace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackFrame {
    pub function_name: Option<String>,
    pub file_path: Option<String>,
    pub line_number: Option<u32>,
    pub column: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedTest {
    pub name: String,
    pub file_path: Option<String>,
    pub line_number: Option<u32>,
    pub message: String,
    pub expected: Option<String>,
    pub actual: Option<String>,
    /// Stack trace frames for this failure
    pub stack_trace: Option<Vec<StackFrame>>,
    /// Related source files that may be relevant to this failure
    pub related_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub failed_tests: Vec<FailedTest>,
}

#[derive(Debug, Clone)]
pub struct TestConfig {
    pub commands: Vec<TestCommand>,
}

#[derive(Debug, Clone)]
pub struct TestCommand {
    pub command: String,
    pub args: Vec<String>,
}

/// Detect project languages by checking for common file extensions and configuration files
pub fn detect_project_languages(project_root: &Path) -> Vec<String> {
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
pub fn get_test_configs(project_root: &Path) -> HashMap<String, TestConfig> {
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

/// Run a command and return the result
pub async fn run_test_command(
    project_root: &Path,
    cmd: &str,
    args: &[String],
    timeout_ms: u64,
) -> TestResult {
    debug!(
        "Running test command: {} {:?} (timeout: {}ms)",
        cmd, args, timeout_ms
    );

    let mut command = Command::new(cmd);
    command.args(args).current_dir(project_root);

    let output_future = command.output();
    let timeout_duration = Duration::from_millis(timeout_ms);

    let (stdout, stderr, success, exit_code) =
        match tokio::time::timeout(timeout_duration, output_future).await {
            Ok(Ok(out)) => (
                String::from_utf8_lossy(&out.stdout).to_string(),
                String::from_utf8_lossy(&out.stderr).to_string(),
                out.status.success(),
                out.status.code(),
            ),
            Ok(Err(e)) => (
                String::new(),
                format!("Failed to run tests: {}", e),
                false,
                None,
            ),
            Err(_) => (
                String::new(),
                format!("Test execution timed out after {}ms", timeout_ms),
                false,
                None,
            ),
        };

    TestResult {
        command: format!("{} {}", cmd, args.join(" ")),
        stdout,
        stderr,
        success,
        exit_code,
        failed_tests: Vec::new(),
    }
}

pub fn parse_test_output(result: &TestResult, language: &str) -> Vec<FailedTest> {
    // Determine the command name/binary from the full command string
    let command_binary = result.command.split_whitespace().next().unwrap_or("");

    match (command_binary, language) {
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
    let re_failed = Regex::new(r"(?m)^test\s+(\S+)\s+\.\.\.\s+FAILED").unwrap();
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
                    stack_trace: None,
                    related_files: Vec::new(),
                });
                current_message.clear();
            }
            current_test = captures.get(1).map(|m| m.as_str().to_string());
        } else if let Some(captures) = re_panic.captures(line) {
            current_file = captures.get(2).map(|m| m.as_str().to_string());
            current_line = captures.get(3).and_then(|m| m.as_str().parse().ok());
        }
    }

    // Check for assertion details
    if let Some(captures) = re_assertion.captures(&combined) {
        for test in &mut failed_tests {
            test.expected = captures.get(2).map(|m| m.as_str().to_string());
            test.actual = captures.get(3).map(|m| m.as_str().to_string());
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
            stack_trace: None,
            related_files: Vec::new(),
        });
    }

    failed_tests
}

fn parse_go_test_output(stdout: &str, stderr: &str) -> Vec<FailedTest> {
    let mut failed_tests = Vec::new();
    let combined = format!("{}\n{}", stdout, stderr);

    // Pattern: --- FAIL: TestName (duration)
    let re_fail = Regex::new(r"(?m)^---\s*FAIL:\s*(\S+)\s*\(").unwrap();
    // Pattern: file_test.go:123: error message
    let re_location = Regex::new(r"(?m)^\s*(\S+\.go):(\d+):\s*(.+)$").unwrap();

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
                    stack_trace: None,
                    related_files: Vec::new(),
                });
                current_message.clear();
            }
            current_test = captures.get(1).map(|m| m.as_str().to_string());
        } else if current_test.is_some()
            && let Some(captures) = re_location.captures(line)
        {
            current_file = captures.get(1).map(|m| m.as_str().to_string());
            current_line = captures.get(2).and_then(|m| m.as_str().parse().ok());
            current_message = captures
                .get(3)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
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
            stack_trace: None,
            related_files: Vec::new(),
        });
    }

    failed_tests
}

fn parse_javascript_test_output(stdout: &str, stderr: &str) -> Vec<FailedTest> {
    let mut failed_tests = Vec::new();
    let combined = format!("{}\n{}", stdout, stderr);

    // Jest/Vitest pattern: ✕ test name (duration)
    let re_fail = Regex::new(r"(?m)[✕✗×]\s+(.+?)\s*(?:\(\d+\s*ms\))?$").unwrap();
    // At path:line pattern
    let re_location = Regex::new(r"at\s+\S+\s+\((.+):(\d+):(\d+)\)").unwrap();

    for line in combined.lines() {
        if let Some(captures) = re_fail.captures(line) {
            let name = captures.get(1).map(|m| m.as_str().to_string());

            // Try to find location in nearby lines
            let mut file_path = None;
            let mut line_number = None;

            if let Some(loc_captures) = re_location.captures(&combined) {
                file_path = loc_captures.get(1).map(|m| m.as_str().to_string());
                line_number = loc_captures.get(2).and_then(|m| m.as_str().parse().ok());
            }

            failed_tests.push(FailedTest {
                name: name.unwrap_or_else(|| "Unknown Test".to_string()),
                file_path,
                line_number,
                message: String::new(),
                expected: None,
                actual: None,
                stack_trace: None,
                related_files: Vec::new(),
            });
        }
    }

    failed_tests
}

fn parse_pytest_output(stdout: &str, stderr: &str) -> Vec<FailedTest> {
    let mut failed_tests = Vec::new();
    let combined = format!("{}\n{}", stdout, stderr);

    // Pattern: FAILED test_file.py::test_name - AssertionError
    let re_fail = Regex::new(r"(?m)^FAILED\s+(\S+)::(\S+)\s*(?:-\s*(.+))?$").unwrap();

    for line in combined.lines() {
        if let Some(captures) = re_fail.captures(line) {
            let file_path = captures.get(1).map(|m| m.as_str().to_string());
            let name = captures.get(2).map(|m| m.as_str().to_string());
            let message = captures
                .get(3)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();

            failed_tests.push(FailedTest {
                name: name.unwrap_or_else(|| "Unknown Test".to_string()),
                file_path,
                line_number: None,
                message,
                expected: None,
                actual: None,
                stack_trace: None,
                related_files: Vec::new(),
            });
        }
    }

    failed_tests
}

/// Parse Rust stack trace from panic output
pub fn parse_rust_stack_trace(output: &str) -> Vec<StackFrame> {
    let mut frames = Vec::new();

    // Pattern: at path/to/file.rs:line:col
    let re_frame = Regex::new(r"at\s+(.+\.rs):(\d+):(\d+)").unwrap();
    // Pattern for function names in backtrace
    let re_func = Regex::new(r"^\s*\d+:\s+(?:0x[a-fA-F0-9]+\s+-\s+)?(.+)$").unwrap();

    let mut current_func: Option<String> = None;

    for line in output.lines() {
        // Check for function name
        if let Some(captures) = re_func.captures(line) {
            let func_name = captures.get(1).unwrap().as_str().to_string();
            // Filter out std library frames
            if !func_name.contains("std::")
                && !func_name.contains("core::")
                && !func_name.contains("panic")
                && !func_name.contains("test::")
            {
                current_func = Some(func_name);
            }
        }

        // Check for file location
        if let Some(captures) = re_frame.captures(line) {
            let file_path = captures.get(1).unwrap().as_str().to_string();
            // Skip standard library and test harness files
            if !file_path.contains("/rustc/")
                && !file_path.contains(".cargo/registry")
                && !file_path.starts_with("/usr/")
            {
                frames.push(StackFrame {
                    function_name: current_func.take(),
                    file_path: Some(file_path),
                    line_number: captures.get(2).unwrap().as_str().parse().ok(),
                    column: captures.get(3).unwrap().as_str().parse().ok(),
                });
            }
        }
    }

    frames
}

/// Extract related source files from failed tests
/// This collects unique file paths that should be included in the LLM context
pub fn extract_related_files(failed_tests: &[FailedTest]) -> Vec<String> {
    let mut files = std::collections::HashSet::new();

    for test in failed_tests {
        // Add the test file itself
        if let Some(ref file_path) = test.file_path {
            files.insert(file_path.clone());
        }

        // Add files from stack trace
        if let Some(ref stack_trace) = test.stack_trace {
            for frame in stack_trace {
                if let Some(ref file_path) = frame.file_path {
                    files.insert(file_path.clone());
                }
            }
        }

        // Add explicitly marked related files
        for file in &test.related_files {
            files.insert(file.clone());
        }
    }

    files.into_iter().collect()
}

/// Enhance failed tests with stack trace information
pub fn enhance_failed_tests_with_stack_trace(
    tests: &mut [FailedTest],
    stdout: &str,
    stderr: &str,
    language: &str,
) {
    let combined = format!("{}\n{}", stdout, stderr);

    if language == "rust" {
        let frames = parse_rust_stack_trace(&combined);
        if !frames.is_empty() {
            // Distribute stack frames to relevant tests
            for test in tests.iter_mut() {
                // Find frames that match this test's file location
                let relevant_frames: Vec<StackFrame> = frames
                    .iter()
                    .filter(|f| {
                        if let (Some(test_file), Some(frame_file)) = (&test.file_path, &f.file_path)
                        {
                            frame_file.contains(test_file) || test_file.contains(frame_file)
                        } else {
                            true // Include if we can't determine relevance
                        }
                    })
                    .cloned()
                    .collect();

                if !relevant_frames.is_empty() {
                    test.stack_trace = Some(relevant_frames.clone());
                    // Add related files
                    for frame in relevant_frames {
                        if let Some(file_path) = frame.file_path
                            && !test.related_files.contains(&file_path)
                        {
                            test.related_files.push(file_path);
                        }
                    }
                }
            }
        }
    }
}
