//! TUI handler for /test-fix command
//!
//! Runs the test-fix loop in the background and reports progress to TUI.

use crate::tui::channel::SenderExt;
use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use std::sync::mpsc::Sender;
use std::thread;

/// Handle the /test-fix TUI command
pub fn handle_test_fix(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    ui.push_log("Starting test-fix loop...");

    if let Some(ui_tx) = &executor.ui_tx {
        let _ = ui_tx.send("::status:shell_running".to_string());

        let ui_tx_clone = ui_tx.clone();
        let cfg = executor.cfg.clone();

        // Extract current conversation history to share context
        let history_msgs = if let Ok(history) = executor.conversation_history.lock() {
            history.build_messages()
        } else {
            Vec::new()
        };

        thread::spawn(move || test_fix_thread(cfg, ui_tx_clone, history_msgs));
    } else {
        ui.push_log("UI channel unavailable - cannot run test-fix in background.");
    }
}

fn test_fix_thread(
    cfg: crate::config::AppConfig,
    ui_tx: Sender<String>,
    history_msgs: Vec<crate::llm::types::ChatMessage>,
) {
    ui_tx.send_logged("::shell_output:Test-fix loop starting...".to_string());

    // Create runtime for async execution
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            ui_tx.send_logged(format!("::shell_output:Failed to create runtime: {}", e));
            ui_tx.send_logged("::status:idle".to_string());
            return;
        }
    };

    let result = rt.block_on(async {
        let mut executor = match crate::exec::Executor::new(cfg.clone()).await {
            Ok(e) => e,
            Err(e) => {
                return Err(anyhow::anyhow!("Failed to create executor: {}", e));
            }
        };

        // Seed executor with history context
        if !history_msgs.is_empty() {
            let mut history_guard: tokio::sync::MutexGuard<'_, crate::llm::ChatHistory> =
                executor.conversation_history.lock().await;
            for msg in &history_msgs {
                history_guard.append_message(msg.clone());
            }
        }

        crate::features::test_fix::run_test_fix_loop(&cfg, &mut executor).await
    });

    match result {
        Ok(result) => {
            if result.success {
                ui_tx.send_logged(format!(
                    "::shell_output:✓ {} (iterations: {})",
                    result.message, result.iterations
                ));
            } else {
                ui_tx.send_logged(format!(
                    "::shell_output:✗ {} (iterations: {})",
                    result.message, result.iterations
                ));
            }
        }
        Err(e) => {
            ui_tx.send_logged(format!("::shell_output:Test-fix loop error: {}", e));
        }
    }

    ui_tx.send_logged("::status:idle".to_string());
}
