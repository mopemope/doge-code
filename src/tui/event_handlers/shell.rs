use anyhow::Result;
use ratatui::widgets::{Block, Borders};
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
        ratatui::crossterm::event::KeyCode::Char('r')
            if k.modifiers
                .contains(ratatui::crossterm::event::KeyModifiers::CONTROL) =>
        {
            app.enter_history_search();
            app.dirty = true;
        }
        ratatui::crossterm::event::KeyCode::Enter => {
            let command = app.textarea.lines().join("\n");
            // If the command is empty, we still send a newline to the PTY
            // But if it's not empty, we might want to check for exit
            if command.trim() == "exit" {
                app.input_mode = InputMode::Normal;
                app.textarea = TextArea::default();
            } else {
                app.input_history.push(command.clone());
                save_input_history(&app.input_history);
                app.history_index = app.input_history.len();

                if let Some(session) = app.shell_session.as_mut() {
                    // Write command + newline to PTY
                    session.write(&format!("{}\n", command)).ok();
                }
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
