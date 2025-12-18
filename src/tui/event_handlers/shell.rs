use anyhow::Result;
use ratatui::widgets::{Block, Borders};
use tui_textarea::{Input, TextArea};

use crate::tui::state::{InputMode, TuiApp, save_input_history};

/// Handle keys when in Shell input mode.
/// Handle keys when in Shell input mode.
pub fn handle_shell_mode_key<B: ratatui::backend::Backend>(
    app: &mut TuiApp,
    k: ratatui::crossterm::event::KeyEvent,
    terminal: &mut ratatui::Terminal<B>,
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
        ratatui::crossterm::event::KeyCode::Up => {
            if app.history_index > 0 {
                // If we are currently at the end (editing a new command), save draft
                if app.history_index == app.input_history.len() {
                    app.draft = app.textarea.lines().join("\n");
                }

                app.history_index -= 1;
                let history_item = app.input_history[app.history_index].clone();

                // Re-create textarea to reset cursor and content cleanly
                app.textarea = TextArea::default();
                app.textarea.set_block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Input (Shell Mode - Press ESC to exit)"),
                );
                app.textarea.set_placeholder_text("Enter your message...");
                app.textarea.insert_str(history_item);
                app.dirty = true;
            }
        }
        ratatui::crossterm::event::KeyCode::Down => {
            if app.history_index < app.input_history.len() {
                app.history_index += 1;

                let content = if app.history_index == app.input_history.len() {
                    // Restore draft
                    app.draft.clone()
                } else {
                    app.input_history[app.history_index].clone()
                };

                app.textarea = TextArea::default();
                app.textarea.set_block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Input (Shell Mode - Press ESC to exit)"),
                );
                app.textarea.set_placeholder_text("Enter your message...");
                app.textarea.insert_str(content);
                app.dirty = true;
            }
        }
        ratatui::crossterm::event::KeyCode::PageUp => {
            let visible_lines = terminal
                .size()
                .map(|s| s.height.saturating_sub(3) as usize)
                .unwrap_or(20);
            app.page_up(visible_lines);
        }
        ratatui::crossterm::event::KeyCode::PageDown => {
            let visible_lines = terminal
                .size()
                .map(|s| s.height.saturating_sub(3) as usize)
                .unwrap_or(20);
            app.page_down(visible_lines);
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
                app.draft.clear(); // Clear draft after successful run

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::TuiApp;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyCode;

    #[test]
    fn test_shell_history_navigation() {
        let mut app = TuiApp::new("test", None, "dark").unwrap();
        app.input_history = vec!["echo 1".to_string(), "echo 2".to_string()];
        app.history_index = 2; // Pointing to new input

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        // 1. Up arrow: should show "echo 2"
        handle_shell_mode_key(
            &mut app,
            ratatui::crossterm::event::KeyEvent::from(KeyCode::Up),
            &mut terminal,
        )
        .unwrap();
        assert_eq!(app.textarea.lines()[0], "echo 2");
        assert_eq!(app.history_index, 1);

        // 2. Up arrow: should show "echo 1"
        handle_shell_mode_key(
            &mut app,
            ratatui::crossterm::event::KeyEvent::from(KeyCode::Up),
            &mut terminal,
        )
        .unwrap();
        assert_eq!(app.textarea.lines()[0], "echo 1");
        assert_eq!(app.history_index, 0);

        // 3. Up arrow again: should stay at "echo 1"
        handle_shell_mode_key(
            &mut app,
            ratatui::crossterm::event::KeyEvent::from(KeyCode::Up),
            &mut terminal,
        )
        .unwrap();
        assert_eq!(app.textarea.lines()[0], "echo 1");
        assert_eq!(app.history_index, 0);

        // 4. Down arrow: should return to "echo 2"
        handle_shell_mode_key(
            &mut app,
            ratatui::crossterm::event::KeyEvent::from(KeyCode::Down),
            &mut terminal,
        )
        .unwrap();
        assert_eq!(app.textarea.lines()[0], "echo 2");
        assert_eq!(app.history_index, 1);

        // 5. Down arrow: should return to empty draft
        handle_shell_mode_key(
            &mut app,
            ratatui::crossterm::event::KeyEvent::from(KeyCode::Down),
            &mut terminal,
        )
        .unwrap();
        assert!(app.textarea.lines()[0].is_empty());
        assert_eq!(app.history_index, 2);
    }

    #[test]
    fn test_shell_scrolling() {
        let mut app = TuiApp::new("test", None, "dark").unwrap();

        // Fill log with enough lines to scroll
        for i in 0..100 {
            app.push_log(format!("line {}", i));
        }

        // Set window width for height calculation
        app.window_width = 80;
        app.recalculate_all_heights();

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        // Initially at bottom
        app.scroll_to_bottom();
        assert!(app.scroll_state.auto_scroll);
        assert_eq!(app.scroll_state.offset, 0);

        // PageUp
        handle_shell_mode_key(
            &mut app,
            ratatui::crossterm::event::KeyEvent::from(KeyCode::PageUp),
            &mut terminal,
        )
        .unwrap();
        assert!(!app.scroll_state.auto_scroll);
        assert!(app.scroll_state.offset > 0);

        let offset_after_page_up = app.scroll_state.offset;

        // PageDown
        handle_shell_mode_key(
            &mut app,
            ratatui::crossterm::event::KeyEvent::from(KeyCode::PageDown),
            &mut terminal,
        )
        .unwrap();
        // Should scroll back down, offset should be less
        assert!(app.scroll_state.offset < offset_after_page_up);
    }
}
