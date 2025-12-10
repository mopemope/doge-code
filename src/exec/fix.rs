use anyhow::{Context, Result};
use tokio::process::Command;
use tracing::{error, info};

use crate::exec::Executor;

/// Runs the auto-fix loop.
/// Executes the command, checking for failure.
/// If failed, sends the output to the LLM to fix the code, then retries.
pub async fn run_fix_loop(
    executor: &mut Executor,
    command: &str,
    max_retries: usize,
) -> Result<bool> {
    info!("Starting auto-fix loop for command: '{}'", command);

    for attempt in 1..=max_retries {
        info!("Attempt {}/{}", attempt, max_retries);

        // Run the command
        let output = if cfg!(target_os = "windows") {
            Command::new("cmd")
                .args(["/C", command])
                .output()
                .await
                .with_context(|| format!("Failed to execute command: {}", command))?
        } else {
            Command::new("sh")
                .arg("-c")
                .arg(command)
                .output()
                .await
                .with_context(|| format!("Failed to execute command: {}", command))?
        };

        if output.status.success() {
            info!("Command executed successfully!");
            println!(
                "✅ Command executed successfully on attempt {}/{}.",
                attempt, max_retries
            );
            return Ok(true);
        }

        // Command failed
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Truncate output if too long to avoid token limit issues, keeping the end which usually has the error
        let max_len = 10000;
        let combined_output = format!("STDOUT:\n{}\n\nSTDERR:\n{}", stdout, stderr);
        let truncated_output = if combined_output.len() > max_len {
            format!(
                "... (truncated)\n{}",
                &combined_output[combined_output.len() - max_len..]
            )
        } else {
            combined_output
        };

        println!(
            "❌ Command failed (Exit code: {:?}) on attempt {}/{}",
            output.status.code(),
            attempt,
            max_retries
        );

        if attempt == max_retries {
            error!("Max retries reached. Stopping.");
            eprintln!("Detailed Error Output:\n{}", truncated_output);
            return Ok(false);
        }

        // Ask LLM to fix
        println!(
            "🤖 Asking AI to fix the issue... (Attempt {}/{})",
            attempt, max_retries
        );

        let prompt = format!(
            "The command `{}` failed to execute.\n\n\
            Here is the output (truncated if too long):\n\
            ```\n\
            {}\n\
            ```\n\
            Please analyze the error and modify the code to fix the issue.\n\
            Use the available tools (edit, apply_patch, etc.) to apply the fix.\n\
            IMPORTANT: Focus ONLY on fixing the error shown above. Do not refactor unrelated code.\n\
            After you have applied the fix, simply reply with a summary of what you fixed.",
            command, truncated_output
        );

        // Run the executor
        // We set json=false because we want human-readable output in the CLI
        if let Err(e) = executor.run(&prompt, false).await {
            error!("Agent failed during fix attempt: {}", e);
            // We continue to next attempt? Or stop?
            // If the agent completely fails (e.g. network error), we probably should stop or retry the AGENT call.
            // But here we count it as a fix attempt.
        }
    }

    Ok(false)
}
