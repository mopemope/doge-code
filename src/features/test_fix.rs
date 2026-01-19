//! Test-Fix Loop: Run tests, analyze failures with LLM, apply fixes, and retry.
//!
//! This module implements a self-healing test loop that:
//! 1. Runs project tests
//! 2. On failure, sends errors to LLM for analysis and fix
//! 3. Re-runs tests to verify the fix
//! 4. Repeats until success or max iterations reached

use crate::config::AppConfig;
use crate::exec::Executor;
use crate::features::test_gen;
use crate::features::testing::{self, TestResult};
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;
use tracing::{info, warn};

/// Result of a test-fix loop execution
#[derive(Debug, Clone, Serialize)]
pub struct TestFixResult {
    pub success: bool,
    pub iterations: usize,
    pub final_stdout: String,
    pub final_stderr: String,
    pub message: String,
}

/// Detect the project language
fn detect_language(project_root: &Path) -> String {
    let languages = testing::detect_project_languages(project_root);
    // Prioritize Rust, then Go, then Python, then TypeScript
    if languages.contains(&"rust".to_string()) {
        "rust".to_string()
    } else if languages.contains(&"go".to_string()) {
        "go".to_string()
    } else if languages.contains(&"python".to_string()) {
        "python".to_string()
    } else if languages.contains(&"typescript".to_string()) {
        "typescript".to_string()
    } else {
        "rust".to_string() // default fallback
    }
}

/// Build a prompt for the LLM to analyze and fix test failures
fn build_fix_prompt(test_result: &TestResult, iteration: usize) -> String {
    // Truncate output if too long to avoid token limit issues (approx 10KB)
    let max_len = 10000;
    let truncate = |s: &str| {
        if s.len() > max_len {
            format!("...(truncated)\n{}", &s[s.len() - max_len..])
        } else {
            s.to_string()
        }
    };

    let stdout = truncate(&test_result.stdout);
    let stderr = truncate(&test_result.stderr);

    let mut prompt = format!(
        r#"<TEST_FAILURE_ANALYSIS iteration="{iteration}">
The following test(s) have failed. Please analyze the failures and provide fixes.

## Test Output

**Command**: {}
**Exit Code**: {}
**STDOUT**:
```
{}
```

**STDERR**:
```
{}
```
"#,
        test_result.command,
        test_result.exit_code.unwrap_or(-1),
        stdout,
        stderr,
    );

    // Add parsed failed tests if available
    if !test_result.failed_tests.is_empty() {
        prompt.push_str("\n## Parsed Failed Tests\n");
        for (i, test) in test_result.failed_tests.iter().enumerate() {
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

            // Add stack trace information if available
            if let Some(ref stack_trace) = test.stack_trace {
                if !stack_trace.is_empty() {
                    prompt.push_str("   Stack Trace:\n");
                    for (j, frame) in stack_trace.iter().take(5).enumerate() {
                        if let Some(ref file) = frame.file_path {
                            let line_info = frame
                                .line_number
                                .map(|l| format!(":{}", l))
                                .unwrap_or_default();
                            let func_info = frame
                                .function_name
                                .as_ref()
                                .map(|f| format!(" in {}", f))
                                .unwrap_or_default();
                            prompt.push_str(&format!(
                                "     {}. {}{}{}\n",
                                j + 1,
                                file,
                                line_info,
                                func_info
                            ));
                        }
                    }
                }
            }

            // Add related files
            if !test.related_files.is_empty() {
                prompt.push_str("   Related Files:\n");
                for file in &test.related_files {
                    prompt.push_str(&format!("     - {}\n", file));
                }
            }
        }

        // Collect all related files for explicit mention
        let related_files = testing::extract_related_files(&test_result.failed_tests);
        if !related_files.is_empty() {
            prompt.push_str("\n## Files to Review\n");
            prompt.push_str("The following files are likely relevant to the failures. Read them using `fs_read` for context:\n");
            for file in &related_files {
                prompt.push_str(&format!("- `{}`\n", file));
            }
        }
    }

    prompt.push_str(
        r#"
## Instructions

1. Analyze the test failures above carefully
2. **Read the relevant source files listed above** using `fs_read` to understand the context
3. Identify the root cause of each failure
4. Apply fixes using `edit` or `apply_patch` tools
5. Focus on fixing the actual code bugs, NOT the tests (unless tests are incorrect)

IMPORTANT: After making changes, the tests will be automatically re-run. Make sure your fixes are complete.
</TEST_FAILURE_ANALYSIS>"#,
    );
    prompt
}

/// Run the test-fix loop
///
/// This function:
/// 1. Runs tests for the detected language
/// 2. If tests fail, sends the output to the LLM for analysis
/// 3. LLM applies fixes via tools
/// 4. Re-runs tests
/// 5. Repeats until success or max_iterations reached
pub async fn run_test_fix_loop(cfg: &AppConfig, executor: &mut Executor) -> Result<TestFixResult> {
    let max_iterations = cfg.test_fix.max_iterations;
    let timeout_ms = cfg.test_fix.test_timeout_ms;
    let project_root = &cfg.project_root;

    info!(
        "Starting test-fix loop (max {} iterations, timeout {}ms)",
        max_iterations, timeout_ms
    );

    let language = detect_language(project_root);
    info!("Detected language: {}", language);

    let test_configs = testing::get_test_configs(project_root);
    let Some(config) = test_configs.get(&language) else {
        return Err(anyhow::anyhow!(
            "No test configuration found for language: {}",
            language
        ));
    };

    if config.commands.is_empty() {
        return Err(anyhow::anyhow!(
            "No test commands configured for language: {}",
            language
        ));
    }

    // For now, we take the first command. In a truly multi-language proj, this might need more logic.
    let test_cmd = &config.commands[0];

    let mut iteration = 0;
    let mut last_result =
        testing::run_test_command(project_root, &test_cmd.command, &test_cmd.args, timeout_ms)
            .await;
    last_result.failed_tests = testing::parse_test_output(&last_result, &language);
    // Enhance failed tests with stack trace information
    testing::enhance_failed_tests_with_stack_trace(
        &mut last_result.failed_tests,
        &last_result.stdout,
        &last_result.stderr,
        &language,
    );

    // Initial test run
    if last_result.success {
        info!("All tests passed on initial run!");
        return Ok(TestFixResult {
            success: true,
            iterations: 0,
            final_stdout: last_result.stdout,
            final_stderr: last_result.stderr,
            message: "All tests passed on initial run.".to_string(),
        });
    }

    // Test-fix loop
    while iteration < max_iterations {
        iteration += 1;
        info!("Test-fix iteration {}/{}", iteration, max_iterations);

        // Build prompt for LLM
        let prompt = build_fix_prompt(&last_result, iteration);

        // Send to executor for LLM analysis and fix
        info!("Sending test failures to LLM for analysis...");
        executor
            .run(&prompt, false)
            .await
            .context("Failed to run LLM analysis")?;

        // Re-run tests
        info!("Re-running tests after LLM fix...");
        last_result =
            testing::run_test_command(project_root, &test_cmd.command, &test_cmd.args, timeout_ms)
                .await;
        last_result.failed_tests = testing::parse_test_output(&last_result, &language);
        // Enhance failed tests with stack trace information
        testing::enhance_failed_tests_with_stack_trace(
            &mut last_result.failed_tests,
            &last_result.stdout,
            &last_result.stderr,
            &language,
        );

        if last_result.success {
            info!("Tests passed after {} iteration(s)!", iteration);

            // Generate regression test if enabled
            if cfg.test_fix.auto_gen_regression_test {
                if let Err(e) = test_gen::generate_regression_test(cfg, executor, &language).await {
                    warn!("Failed to generate regression test: {}", e);
                }
            }

            return Ok(TestFixResult {
                success: true,
                iterations: iteration,
                final_stdout: last_result.stdout,
                final_stderr: last_result.stderr,
                message: format!("Tests passed after {} iteration(s).", iteration),
            });
        }

        warn!(
            "Tests still failing after iteration {}. Exit code: {:?}",
            iteration, last_result.exit_code
        );
    }

    // Max iterations reached
    warn!(
        "Test-fix loop reached max iterations ({}) without success",
        max_iterations
    );
    Ok(TestFixResult {
        success: false,
        iterations: max_iterations,
        final_stdout: last_result.stdout,
        final_stderr: last_result.stderr,
        message: format!(
            "Tests still failing after {} iterations. Manual intervention required.",
            max_iterations
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_detect_language_rust() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("Cargo.toml"), "").unwrap();
        assert_eq!(detect_language(temp_dir.path()), "rust");
    }

    #[test]
    fn test_detect_language_go() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("go.mod"), "").unwrap();
        assert_eq!(detect_language(temp_dir.path()), "go");
    }

    #[test]
    fn test_detect_language_python() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("pyproject.toml"), "").unwrap();
        assert_eq!(detect_language(temp_dir.path()), "python");
    }

    #[test]
    fn test_detect_language_typescript() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("package.json"), "{}").unwrap();
        assert_eq!(detect_language(temp_dir.path()), "typescript");
    }

    #[test]
    fn test_build_fix_prompt() {
        let result = TestResult {
            command: "cargo test".to_string(),
            stdout: "test output".to_string(),
            stderr: "error message".to_string(),
            success: false,
            exit_code: Some(1),
            failed_tests: Vec::new(),
        };
        let prompt = build_fix_prompt(&result, 1);
        assert!(prompt.contains("TEST_FAILURE_ANALYSIS"));
        assert!(prompt.contains("test output"));
        assert!(prompt.contains("error message"));
    }
}
