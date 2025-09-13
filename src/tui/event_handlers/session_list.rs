use crate::tui::state::{InputMode, TuiApp};
use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent};

type TerminalType = Terminal<CrosstermBackend<std::io::Stdout>>;

/// Handle keys when in SessionList mode. Returns Ok(true) if the caller should exit the event loop.
pub fn handle_session_list_key(
    app: &mut TuiApp,
    k: KeyEvent,
    _terminal: &mut TerminalType,
) -> Result<bool> {
    // Make sure we're in session list mode and have session list state
    if app.input_mode != InputMode::SessionList || app.session_list_state.is_none() {
        return Ok(false);
    }

    let session_list_state = app.session_list_state.as_mut().unwrap();
    let session_count = session_list_state.sessions.len();

    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            // Exit session list mode
            app.input_mode = InputMode::Normal;
            app.session_list_state = None;
            app.dirty = true;
        }
        KeyCode::Up => {
            if session_count > 0 {
                session_list_state.selected_index =
                    session_list_state.selected_index.saturating_sub(1);
                app.dirty = true;
            }
        }
        KeyCode::Down => {
            if session_count > 0 {
                session_list_state.selected_index =
                    (session_list_state.selected_index + 1).min(session_count - 1);
                app.dirty = true;
            }
        }
        KeyCode::Enter => {
            if session_count > 0
                && session_list_state.selected_index < session_list_state.sessions.len()
            {
                let selected_session_id = session_list_state.sessions
                    [session_list_state.selected_index]
                    .id
                    .clone();
                // Switch to the selected session
                // This will be handled by the command handler
                app.pending_instructions
                    .push_back(format!("/session switch {}", selected_session_id));
                // Exit session list mode
                app.input_mode = InputMode::Normal;
                app.session_list_state = None;
                app.dirty = true;
            }
        }
        KeyCode::Char('d') => {
            if session_count > 0
                && session_list_state.selected_index < session_list_state.sessions.len()
            {
                let session_id = session_list_state.sessions[session_list_state.selected_index]
                    .id
                    .clone();
                // Delete the selected session
                // This will be handled by the command handler
                app.pending_instructions
                    .push_back(format!("/session delete {}", session_id));
                app.dirty = true;
            }
        }
        _ => {}
    }

    Ok(false)
}
