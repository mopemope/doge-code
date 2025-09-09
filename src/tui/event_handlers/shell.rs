use anyhow::Result;
use ratatui::widgets::{Block, Borders};
use tokio::process::Command;
use tokio::spawn;
use tui_textarea::{Input, TextArea};

use crate::tui::state::{InputMode, TuiApp, save_input_history};

/// Handle keys when in Shell input mode.
pub fn handle_shell_mode_key(
    app: &mut TuiApp,
    k: ratatui::crossterm::event::KeyEvent,
) -> Result<()> {
    match k.code {
        ratatui::crossterm::event::KeyCode::Esc => {
            app.input_mode = InputMode::Normal;
            app.textarea.delete_line_by_head();
            app.textarea.delete_line_by_end();
            app.textarea
                .set_block(Block::default().borders(Borders::ALL).title("Input"));
            app.dirty = true;
        }
        ratatui::crossterm::event::KeyCode::Enter => {
            let command = app.textarea.lines().join("\n");
            if !command.trim().is_empty() {
                app.push_log(format!("[shell]$ {}", command));
                if app.input_history.last().map(|s| s.as_str()) != Some(command.as_str()) {
                    app.input_history.push(command.clone());
                    save_input_history(&app.input_history);
                }
                app.history_index = app.input_history.len();
                app.draft.clear();

                let tx = app.inbox_tx.clone().unwrap();
                spawn(async move {
                    tx.send("::status:shell_running".to_string()).ok();
                    let command_with_redirect = format!("{} 2>&1", command);
                    let output = Command::new("bash")
                        .arg("-c")
                        .arg(&command_with_redirect)
                        .output()
                        .await;
                    tx.send("::status:done".to_string()).ok();

                    match output {
                        Ok(output) => {
                            if !output.stdout.is_empty() {
                                let output_str =
                                    String::from_utf8_lossy(&output.stdout).to_string();
                                tx.send(format!("::shell_output:{}", output_str)).ok();
                            }
                        }
                        Err(e) => {
                            tx.send(format!("::shell_output:Failed to execute command: {}", e))
                                .ok();
                        }
                    }
                });
            }

            // Clear the textarea
            app.textarea = TextArea::default();
            app.textarea.set_block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Input (Shell Mode - Press ESC to exit)"),
            );
            app.textarea.set_placeholder_text("Enter your message...");
            app.dirty = true;
        }
        _ => {
            if app.textarea.input(Input::from(k)) {
                app.dirty = true;
            }
        }
    }

    Ok(())
}
