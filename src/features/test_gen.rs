use crate::config::AppConfig;
use crate::exec::Executor;
use anyhow::{Context, Result};
use tracing::info;

/// Generates a regression test for the fixed code.
///
/// # Arguments
/// * `config` - The application configuration.
/// * `executor` - The LLM executor.
/// * `language` - The programming language of the file.
pub async fn generate_regression_test(
    _config: &AppConfig,
    executor: &mut Executor,
    language: &str,
) -> Result<()> {
    info!("Generating and saving regression test based on previous context");

    let prompt = format!(
        "The bug fix verified successfully.
Now, please generate a regression test case (unit test in {}) to ensure this bug does not reappear.
Please write the test code to the appropriate test file using available tools (e.g., `fs_write` or `edit`).
If an existing test file matches the fixed code, append the test there. Otherwise create a new test file following standard conventions (e.g., `_test.go`, `tests/*.rs`, `test_*.py`, `*.test.ts`).",
        language
    );

    executor
        .run(&prompt, false)
        .await
        .context("Failed to generate regression test")?;

    Ok(())
}
