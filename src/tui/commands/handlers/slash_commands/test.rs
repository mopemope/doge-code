use crate::features::testing;
use crate::tui::channel::SenderExt;
use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;

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

// Logic moved to src/features/testing.rs

fn test_thread(project_root: PathBuf, ui_tx: Sender<String>) {
    ui_tx.send_logged(format!(
        "::shell_output:Project root: {}",
        project_root.display()
    ));

    // Detect languages in the project
    let detected_languages = testing::detect_project_languages(&project_root);

    ui_tx.send_logged(format!(
        "::shell_output:Detected languages: {:?}",
        detected_languages
    ));

    if detected_languages.is_empty() {
        ui_tx.send_logged(
            "::shell_output:No supported languages (Go, Rust, TypeScript, Python) detected.",
        );
        ui_tx.send_logged("::status:idle");
        return;
    }

    let mut all_failed_tests = Vec::new();
    let mut all_command_outputs = Vec::new();
    let mut has_any_failures = false;

    let test_configs = testing::get_test_configs(&project_root);

    // Run tests for each detected language
    for lang in detected_languages {
        ui_tx.send_logged(format!("::shell_output:\n--- Running {} tests ---", lang));

        if let Some(config) = test_configs.get(&lang) {
            if config.commands.is_empty() {
                ui_tx.send_logged(format!(
                    "::shell_output:No test commands configured for language '{}'.",
                    lang
                ));
                continue;
            }

            for test_cmd in &config.commands {
                // Run command async using runtime
                let rt = tokio::runtime::Runtime::new().unwrap();
                let mut result = rt.block_on(async {
                    testing::run_test_command(
                        &project_root,
                        &test_cmd.command,
                        &test_cmd.args,
                        60000,
                    )
                    .await
                });

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
                    ui_tx.send_logged(format!(
                        "::shell_output:✓ All tests passed: {} {}",
                        test_cmd.command,
                        test_cmd.args.join(" ")
                    ));
                } else {
                    ui_tx.send_logged(format!(
                        "::shell_output:✗ Tests failed: {} {}",
                        test_cmd.command,
                        test_cmd.args.join(" ")
                    ));
                }

                if !result.stdout.is_empty() {
                    ui_tx.send_logged(format!("::shell_output:Output:\n{}", result.stdout));
                }

                if !result.stderr.is_empty() {
                    ui_tx.send_logged(format!("::shell_output:STDERR:\n{}", result.stderr));
                }

                // Parse failed tests
                let failed_tests = testing::parse_test_output(&result, &lang);
                result.failed_tests = failed_tests.clone();
                if !failed_tests.is_empty() {
                    ui_tx.send_logged(format!(
                        "::shell_output:Found {} failed test(s)",
                        failed_tests.len()
                    ));
                }

                all_failed_tests.extend(failed_tests);
            }
        } else {
            ui_tx.send_logged(format!(
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

        ui_tx
            .send_logged("::shell_output:\nSending test failures to LLM for analysis and fixes...");
        ui_tx.send_logged(format!("::test_failures_analysis:{}", prompt));
    } else {
        ui_tx.send_logged("::shell_output:\n✓ All tests passed!");
    }

    ui_tx.send_logged("::shell_output:Test run completed.");
    ui_tx.send_logged("::status:idle");
}
