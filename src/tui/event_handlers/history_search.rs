use crate::tui::state::{InputMode, TuiApp};
use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_textarea::TextArea;

/// Handle keys when in History Search mode.
pub fn handle_history_search_key(app: &mut TuiApp, k: KeyEvent) -> Result<()> {
    match k.code {
        KeyCode::Esc => {
            // Cancel search
            app.input_mode = InputMode::Normal;
            app.history_search_state = None;
            app.dirty = true;
        }
        KeyCode::Char('g') if k.modifiers.contains(KeyModifiers::CONTROL) => {
            // Cancel search (Ctrl+G)
            app.input_mode = InputMode::Normal;
            app.history_search_state = None;
            app.dirty = true;
        }
        KeyCode::Char('r') if k.modifiers.contains(KeyModifiers::CONTROL) => {
            // Cycle through results or refresh
            // For now, let's just keep it as is, maybe select next?
            if let Some(state) = &mut app.history_search_state
                && !state.results.is_empty() {
                    state.selected_index = (state.selected_index + 1) % state.results.len();
                }
            app.dirty = true;
        }
        KeyCode::Enter => {
            // Select command
            if let Some(state) = &app.history_search_state
                && let Some(cmd) = state.results.get(state.selected_index) {
                    // Set selected command to input
                    app.textarea = TextArea::default();
                    app.textarea.set_block(
                        ratatui::widgets::Block::default()
                            .borders(ratatui::widgets::Borders::ALL)
                            .title("Input"),
                    );
                    app.textarea.insert_str(cmd);
                    app.last_elapsed_time = None;
                }
            app.input_mode = InputMode::Normal;
            app.history_search_state = None;
            app.dirty = true;
        }
        KeyCode::Up => {
            // Select previous
            if let Some(state) = &mut app.history_search_state
                && !state.results.is_empty() {
                    if state.selected_index == 0 {
                        state.selected_index = state.results.len() - 1;
                    } else {
                        state.selected_index -= 1;
                    }
                }
            app.dirty = true;
        }
        KeyCode::Down => {
            // Select next
            if let Some(state) = &mut app.history_search_state
                && !state.results.is_empty() {
                    state.selected_index = (state.selected_index + 1) % state.results.len();
                }
            app.dirty = true;
        }
        KeyCode::Backspace => {
            // Edit query
            if let Some(state) = &mut app.history_search_state {
                state.query.pop();
                app.update_history_search();
            }
            app.dirty = true;
        }
        KeyCode::Char(c) => {
            // Edit query
            if !k.modifiers.contains(KeyModifiers::CONTROL)
                && !k.modifiers.contains(KeyModifiers::ALT)
            {
                if let Some(state) = &mut app.history_search_state {
                    state.query.push(c);
                    app.update_history_search();
                }
                app.dirty = true;
            }
        }
        _ => {}
    }
    Ok(())
}
