use crate::tui::{commands::core::TuiExecutor, state::TuiApp};
pub fn handle_models(executor: &mut TuiExecutor, ui: &mut TuiApp, query: &str) {
    match executor.ensure_model_selection_idle(ui) {
        Ok(()) => {
            match crate::features::model_selection::ModelPicker::new(executor.cfg.provider, query) {
                Ok(mut picker) => {
                    if query.is_empty() {
                        picker.selected = picker
                            .results()
                            .iter()
                            .position(|spec| spec.id == executor.cfg.model)
                            .unwrap_or(0);
                    }
                    ui.model_picker = Some(picker);
                    ui.dirty = true;
                }
                Err(error) => ui.push_log(error.to_string()),
            }
        }
        Err(error) => ui.push_log(format!("Cannot open models: {error}")),
    }
}
