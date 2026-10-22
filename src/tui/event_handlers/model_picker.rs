use crate::features::openrouter::catalog::CatalogPolicy;
use crate::tui::state::TuiApp;
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

pub(crate) fn close_model_picker(app: &mut TuiApp) {
    if let Some(mut handler) = app.handler.take() {
        handler.cancel_model_catalog(app);
        app.handler = Some(handler);
    }
    app.model_picker = None;
    app.dirty = true;
}
pub(crate) fn handle_model_picker_key(app: &mut TuiApp, key: KeyEvent) -> Result<()> {
    if key.kind != KeyEventKind::Press || app.model_picker.is_none() {
        return Ok(());
    }
    if key.code == KeyCode::Esc
        || (matches!(key.code, KeyCode::Char('c' | 'g'))
            && key.modifiers.contains(KeyModifiers::CONTROL))
    {
        close_model_picker(app);
        return Ok(());
    }
    if matches!(key.code, KeyCode::Tab | KeyCode::BackTab)
        || (key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL))
    {
        if let Some(mut handler) = app.handler.take() {
            handler.cancel_model_catalog(app);
            if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                let picker = app.model_picker.as_mut().unwrap();
                picker.cycle_provider(
                    key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT),
                );
                if let Some(cfg) = &app.cfg {
                    picker.key_available = if picker.provider
                        == crate::features::openai_subscription::ProviderKind::Openrouter
                    {
                        if cfg.provider == picker.provider {
                            cfg.api_key.is_some()
                        } else {
                            std::env::var("OPENROUTER_API_KEY")
                                .is_ok_and(|key| !key.trim().is_empty())
                        }
                    } else {
                        crate::features::opencode::selection_config(
                            cfg,
                            picker.provider,
                            &cfg.model,
                            crate::features::opencode::key_store::available_key(),
                        )
                        .is_ok_and(|cfg| cfg.api_key.is_some())
                    };
                }
            }
            let policy = if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                CatalogPolicy::PreferCache
            } else {
                CatalogPolicy::Refresh
            };
            if (app.model_picker.as_ref().is_some_and(|picker| {
                picker.provider == crate::features::openai_subscription::ProviderKind::Openrouter
            }) || matches!(policy, CatalogPolicy::Refresh))
                && let Err(error) = handler.start_model_catalog(app, policy)
            {
                app.model_picker.as_mut().unwrap().error = Some(error.to_string());
            }
            app.handler = Some(handler);
        } else if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            app.model_picker
                .as_mut()
                .unwrap()
                .cycle_provider(key.code == KeyCode::BackTab);
        }
        app.dirty = true;
        return Ok(());
    }
    let picker = app.model_picker.as_mut().unwrap();
    match key.code {
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
        KeyCode::Enter => {
            if !picker.can_select() {
                picker.error = Some("Catalog loading; press Enter again after completion".into());
            } else {
                match picker.results().get(picker.selected).cloned().cloned() {
                    None => {
                        picker.error =
                            Some("No matching models; edit search or Esc to cancel".into())
                    }
                    Some(entry) if entry.unavailable_reason.is_some() => {
                        picker.error = entry.unavailable_reason
                    }
                    Some(entry) => {
                        if let Some(mut handler) = app.handler.take() {
                            let result = handler.select_model_for_new_session(
                                entry.provider,
                                &entry.id,
                                app,
                            );
                            app.handler = Some(handler);
                            match result {
                                Ok(_) => close_model_picker(app),
                                Err(error) => {
                                    app.model_picker.as_mut().unwrap().error =
                                        Some(error.to_string())
                                }
                            }
                        } else {
                            picker.error = Some("Model selection handler unavailable".into());
                        }
                    }
                }
            }
        }
        _ => {}
    }
    app.dirty = true;
    Ok(())
}
