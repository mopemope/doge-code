use crate::tui::state::{InputMode, TuiApp};
use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Handle keys when in File Search mode.
pub fn handle_file_search_key(app: &mut TuiApp, k: KeyEvent) -> Result<()> {
    match k.code {
        KeyCode::Esc => {
            // Cancel search
            app.input_mode = InputMode::Normal;
            app.file_search_state = None;
            app.dirty = true;
        }
        KeyCode::Char('g') if k.modifiers.contains(KeyModifiers::CONTROL) => {
            // Cancel search (Ctrl+G)
            app.input_mode = InputMode::Normal;
            app.file_search_state = None;
            app.dirty = true;
        }
        KeyCode::Enter => {
            // Select file and open it
            if let Some(state) = &app.file_search_state
                && let Some(path) = state.results.get(state.selected_index)
            {
                let cmd = format!("/open {}", path);
                app.dispatch(&cmd);
            }
            app.input_mode = InputMode::Normal;
            app.file_search_state = None;
            app.dirty = true;
        }
        KeyCode::Up => {
            // Select previous
            if let Some(state) = &mut app.file_search_state
                && !state.results.is_empty()
            {
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
            if let Some(state) = &mut app.file_search_state
                && !state.results.is_empty()
            {
                state.selected_index = (state.selected_index + 1) % state.results.len();
            }
            app.dirty = true;
        }
        KeyCode::Backspace => {
            // Edit query
            if let Some(state) = &mut app.file_search_state {
                state.query.pop();
                app.update_file_search();
            }
            app.dirty = true;
        }
        KeyCode::Char(c) => {
            // Edit query
            if !k.modifiers.contains(KeyModifiers::CONTROL)
                && !k.modifiers.contains(KeyModifiers::ALT)
            {
                if let Some(state) = &mut app.file_search_state {
                    state.query.push(c);
                    app.update_file_search();
                }
                app.dirty = true;
            }
        }
        _ => {}
    }
    Ok(())
}
