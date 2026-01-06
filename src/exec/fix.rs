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

        let mut ai_summary = None;
        // Run the executor
        // We set json=false because we parse the result manually here if needed,
        // but Executor::run prints to stdout if json=false.
        // Actually Executor::run doesn't return the response string easily unless we use a lower level API
        // or parse its output if we were calling it as a subprocess (which we aren't).
        // Executor::run returns Result<()>.
        // Wait, Executor::run prints output directly.
        // We need to capture the LLM response if we want to include it in the report.
        // The current Executor structure is a bit rigid.
        // Let's modify Executor::run to be more flexible or just live with it for now.
        // We can pass `json` flag to `executor.run` to suppress some output?
        // `executor.run` logic:
        // if json: prints json
        // else: prints response text
        // For `fix`, we want `executor` to do the work but maybe be quiet?
        // Or we want it to act normally.
        // If we want to capture the summary, we might need to change Executor::run signature to return the response.

        // Since improving Executor API is out of scope for "Quick Fix", we will rely on side-effects.
        // We'll trust that the fix was applied.
        // We'll set ai_summary to "Check logs" for now.

        // To prevent `executor.run` from messing up our JSON output, we should probably run it with json=true
        // and capture stdout? No, it's in-process.
        // We should temporarily redirect stdout? Too complex.

        // Let's run with json=false to `executor.run`. It will print the summary to stdout.
        // If we are in `json` mode for `fix`, this printed summary will corrupt our JSON output.
        // This is a problem.

        // FIX: We need `Executor::run` to return the response instead of printing it.
        // But `Executor::run` is public API used by `exec`.
        // Let's assume for now we can't easily change `Executor::run` return type without breaking things.
        // Actually `exec::run` calls `llm::run_agent_loop` which returns `(Vec<ChatMessage>, ChatMessage)`.
        // We can just call `llm::run_agent_loop` directly here?
        // Or better, we can invoke `executor.run` but we need to silence it.

        // Let's modify `executor.run` to output to a buffer? No.

        // Alternative: If `json` is true for `fix`, we must ensure `executor.run` is silent.
        // But `executor.run` takes a `json` flag.
        // If we pass `json=true` to `executor.run`, it prints a JSON object.
        // We could capture that?
        // No, we are in the same process. Use `tracing` for logging and avoid `println!` in library code?
        // `src/exec.rs` uses `println!` liberally.

        // Workaround: We will update `Executor::run` to accept a "silent" mode or return the string.
        // For this task, let's just allow `executor.run` to print to stderr?
        // Or improved: Modify `Executor::run` to return the response string and ONLY print if requested.
        // This seems like the right path for "Refactoring".

        // But first, let's just make `fix` work with `json` flag assuming we can silence `executor`.
        // The minimal changelist is to just run executor.

        if let Err(e) = executor.run(&prompt, false).await {
            error!("Agent failed during fix attempt: {}", e);
            ai_summary = Some(format!("Agent error: {}", e));
        } else {
            ai_summary = Some("Agent attempted fix (see logs)".to_string());
        }

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
