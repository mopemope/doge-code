use anyhow::{Context, Result};
use serde::Serialize;
use tokio::process::Command;
use tracing::{error, info};

use crate::exec::Executor;

#[derive(Serialize)]
struct FixReport {
    success: bool,
    attempts: Vec<FixAttempt>,
}

#[derive(Serialize)]
struct FixAttempt {
    attempt: usize,
    success: bool,
    command_output: String,
    ai_fix_summary: Option<String>,
}

/// Runs the auto-fix loop.
/// Executes the command, checking for failure.
/// If failed, sends the output to the LLM to fix the code, then retries.
pub async fn run_fix_loop(
    executor: &mut Executor,
    command: &str,
    max_retries: usize,
    json: bool,
) -> Result<bool> {
    info!("Starting auto-fix loop for command: '{}'", command);

    let mut attempts = Vec::new();
    let mut final_success = false;

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
            if !json {
                println!(
                    "✅ Command executed successfully on attempt {}/{}.",
                    attempt, max_retries
                );
            }
            attempts.push(FixAttempt {
                attempt,
                success: true,
                command_output: String::from_utf8_lossy(&output.stdout).to_string(),
                ai_fix_summary: None,
            });
            final_success = true;
            break;
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
            combined_output.clone()
        };

        if !json {
            println!(
                "❌ Command failed (Exit code: {:?}) on attempt {}/{}",
                output.status.code(),
                attempt,
                max_retries
            );
        }

        if attempt == max_retries {
            error!("Max retries reached. Stopping.");
            if !json {
                eprintln!("Detailed Error Output:\n{}", truncated_output);
            }
            attempts.push(FixAttempt {
                attempt,
                success: false,
                command_output: combined_output,
                ai_fix_summary: None,
            });
            break;
        }

        // Ask LLM to fix
        if !json {
            println!(
                "🤖 Asking AI to fix the issue... (Attempt {}/{})",
                attempt, max_retries
            );
        }

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

        let ai_summary = if let Err(e) = executor.run(&prompt, false).await {
            error!("Agent failed during fix attempt: {}", e);
            Some(format!("Agent error: {}", e))
        } else {
            Some("Agent attempted fix (see logs)".to_string())
        };

        attempts.push(FixAttempt {
            attempt,
            success: false, // It failed before fix attempt, we don't know if fix worked until next loop
            command_output: combined_output,
            ai_fix_summary: ai_summary,
        });
    }

    if json {
        let report = FixReport {
            success: final_success,
            attempts,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    }

    Ok(final_success)
}
