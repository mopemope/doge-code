use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::{Block, Borders};
use tokio::process::Command;
use tokio::spawn;
use tracing::debug;
use tui_textarea::{CursorMove, Input, TextArea};

use crate::tui::state::{InputMode, TuiApp, save_input_history};

type TerminalType = Terminal<CrosstermBackend<std::io::Stdout>>;

/// Handle keys when in Normal input mode. Returns Ok(true) if the caller should exit the event loop.
pub fn handle_normal_mode_key(
    app: &mut TuiApp,
    k: KeyEvent,
    terminal: &mut TerminalType,
) -> Result<bool> {
    match k {
        KeyEvent {
            code: KeyCode::Enter,
            modifiers: m,
            ..
        } if m.contains(KeyModifiers::ALT) => {
            app.textarea.insert_newline();
            app.dirty = true;
        }

        KeyEvent {
            code: KeyCode::Enter,
            ..
        } => {
            if app.completion_active {
                let completed_item = app.completion_candidates[app.completion_index].clone();
                let current_input = app.textarea.lines()[0].clone();

                let new_input = if current_input.starts_with('/') {
                    // Command completion
                    let mut parts: Vec<&str> = current_input.split_whitespace().collect();
                    if !parts.is_empty() {
                        parts[0] = &completed_item;
                    }
                    parts.join(" ") + " "
                } else if current_input.starts_with('@') {
                    // File path completion
                    format!("@{completed_item}")
                } else {
                    current_input
                };

                // Clear existing content and insert new_input
                app.textarea.delete_line_by_head();
                app.textarea.delete_line_by_end();
                app.textarea.insert_str(&new_input);
                app.textarea.move_cursor(CursorMove::End);
                app.completion_active = false;
                app.dirty = true;
            } else {
                // Submit message logic
                let line = app.textarea.lines().join("\n");
                if !line.trim().is_empty() {
                    if app.input_history.last().map(|s| s.as_str()) != Some(line.as_str()) {
                        app.input_history.push(line.clone());
                        save_input_history(&app.input_history);
                    }
                    app.history_index = app.input_history.len();
                    app.draft.clear();
                }

                if line.trim() == "/quit" {
                    return Ok(true);
                }

                app.pending_instructions.push_back(line);

                // Clear the textarea
                app.textarea = TextArea::default();
                app.textarea
                    .set_block(Block::default().borders(Borders::ALL).title("Input"));
                app.textarea.set_placeholder_text("Enter your message...");
                app.dirty = true;
                app.spinner_state = 0;
            }
        }

        // Shell mode switch
        KeyEvent {
            code: KeyCode::Char('!'),
            ..
        } if app.textarea.is_empty() => {
            app.input_mode = InputMode::Shell;
            app.textarea.set_block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Input (Shell Mode - Press ESC to exit)"),
            );
            app.dirty = true;
        }

        KeyEvent {
            code: KeyCode::Esc, ..
        } => {
            app.dispatch("/cancel");
            app.dirty = true;
        }

        KeyEvent {
            code: KeyCode::PageUp,
            ..
        } => {
            let visible_lines = terminal
                .size()
                .map(|s| s.height.saturating_sub(3) as usize)
                .unwrap_or(20);
            app.page_up(visible_lines);
        }

        KeyEvent {
            code: KeyCode::PageDown,
            ..
        } => {
            let visible_lines = terminal
                .size()
                .map(|s| s.height.saturating_sub(3) as usize)
                .unwrap_or(20);
            app.page_down(visible_lines);
        }

        KeyEvent {
            code: KeyCode::Home,
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_to_top();
        }

        KeyEvent {
            code: KeyCode::End,
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_to_bottom();
        }

        KeyEvent {
            code: KeyCode::Up,
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_up(1);
        }

        KeyEvent {
            code: KeyCode::Down,
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_down(1);
        }

        KeyEvent {
            code: KeyCode::Char('l'),
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_to_bottom();
        }

        KeyEvent {
            code: KeyCode::Up,
            modifiers: KeyModifiers::NONE,
            ..
        } => {
            if app.completion_active {
                app.completion_index = app.completion_index.saturating_sub(1);
                app.dirty = true;
            } else if !app.input_history.is_empty() && app.history_index > 0 {
                if app.history_index == app.input_history.len() {
                    app.draft = app.textarea.lines().join("\n");
                }
                app.history_index -= 1;
                app.textarea = TextArea::from(app.input_history[app.history_index].lines());
                app.textarea
                    .set_block(Block::default().borders(Borders::ALL).title("Input"));
                app.textarea.set_placeholder_text("Enter your message...");
                app.dirty = true;
            }
        }

        KeyEvent {
            code: KeyCode::Down,
            modifiers: KeyModifiers::NONE,
            ..
        } => {
            if app.completion_active {
                app.completion_index =
                    (app.completion_index + 1).min(app.completion_candidates.len() - 1);
                app.dirty = true;
            } else if !app.input_history.is_empty() && app.history_index < app.input_history.len() {
                app.history_index += 1;
                if app.history_index == app.input_history.len() {
                    app.textarea = TextArea::from(app.draft.lines());
                } else {
                    app.textarea = TextArea::from(app.input_history[app.history_index].lines());
                }
                app.textarea
                    .set_block(Block::default().borders(Borders::ALL).title("Input"));
                app.textarea.set_placeholder_text("Enter your message...");
                app.dirty = true;
            }
        }

        KeyEvent {
            code: KeyCode::Tab, ..
        } => {
            if app.completion_active {
                let completed_item = app.completion_candidates[app.completion_index].clone();
                let current_input = app.textarea.lines()[0].clone();

                let new_input = if current_input.starts_with('/') {
                    let mut parts: Vec<&str> = current_input.split_whitespace().collect();
                    if !parts.is_empty() {
                        parts[0] = &completed_item;
                    }
                    parts.join(" ") + " "
                } else if current_input.starts_with('@') {
                    format!("@{completed_item}")
                } else {
                    current_input
                };

                app.textarea.delete_line_by_head();
                app.textarea.delete_line_by_end();
                app.textarea.insert_str(&new_input);
                app.textarea.move_cursor(CursorMove::End);
                app.completion_active = false;
                app.dirty = true;
            }
        }

        KeyEvent {
            code: KeyCode::Left,
            ..
        } => {
            app.textarea.move_cursor(CursorMove::Back);
            app.dirty = true;
        }

        KeyEvent {
            code: KeyCode::Right,
            ..
        } => {
            app.textarea.move_cursor(CursorMove::Forward);
            app.dirty = true;
        }

        other => {
            debug!("Handling other key event: {:?}", other.code);
            let handled_by_textarea = app.textarea.input(Input::from(other));
            debug!("Handled by textarea: {}", handled_by_textarea);
            if handled_by_textarea {
                app.dirty = true;
                let input_str = app.textarea.lines()[0].clone();
                if input_str.starts_with('/') {
                    app.update_completion_candidates(&input_str);
                    if !app.completion_candidates.is_empty() {
                        app.completion_active = true;
                    }
                } else if input_str.starts_with('@') {
                    app.update_file_path_completion_candidates(&input_str);
                    if !app.completion_candidates.is_empty() {
                        app.completion_active = true;
                    }
                } else {
                    app.completion_active = false;
                }
            }
        }
    }

    Ok(false)
}

/// Handle keys when in Shell input mode.
pub fn handle_shell_mode_key(app: &mut TuiApp, k: KeyEvent) -> Result<()> {
    match k.code {
        KeyCode::Esc => {
            app.input_mode = InputMode::Normal;
            app.textarea.delete_line_by_head();
            app.textarea.delete_line_by_end();
            app.textarea
                .set_block(Block::default().borders(Borders::ALL).title("Input"));
            app.dirty = true;
        }
        KeyCode::Enter => {
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
