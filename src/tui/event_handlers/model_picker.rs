use crate::tui::state::TuiApp;
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

pub(crate) fn handle_model_picker_key(app: &mut TuiApp, key: KeyEvent) -> Result<()> {
    if key.kind != KeyEventKind::Press {
        return Ok(());
    }
    let Some(picker) = app.model_picker.as_mut() else {
        return Ok(());
    };
    match key.code {
        KeyCode::Esc => {
            app.model_picker = None;
        }
        KeyCode::Char('c' | 'g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.model_picker = None;
        }
        KeyCode::Up => picker.move_selection(false),
        KeyCode::Down => picker.move_selection(true),
        KeyCode::Backspace => {
            picker.query.pop();
            picker.refresh();
        }
        KeyCode::Char(ch)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            picker.query.push(ch);
            picker.refresh();
        }
        KeyCode::Enter => match picker.results().get(picker.selected).copied() {
            None => {
                picker.error = Some("No matching models; edit search or Esc to cancel".into());
            }
            Some(spec) if spec.api.adapter().is_none() => {
                picker.error = Some(format!("{}: {}", spec.api.name(), spec.api.reason()));
            }
            Some(spec) => {
                if let Some(mut handler) = app.handler.take() {
                    let result = handler.select_model_for_new_session(spec.id, app);
                    app.handler = Some(handler);
                    match result {
                        Ok(_) => app.model_picker = None,
                        Err(error) => {
                            if let Some(picker) = app.model_picker.as_mut() {
                                picker.error = Some(error.to_string());
                            }
                        }
                    }
                } else {
                    picker.error = Some("Model selection handler unavailable".into());
                }
            }
        },
        _ => {}
    }
    app.dirty = true;
    Ok(())
}
