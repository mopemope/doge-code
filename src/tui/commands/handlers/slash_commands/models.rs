use crate::{
    features::{openai_subscription::ProviderKind, opencode},
    tui::{commands::core::TuiExecutor, state::TuiApp},
};

pub fn handle_models(executor: &mut TuiExecutor, ui: &mut TuiApp, query: &str) {
    open_picker(executor, ui, executor.cfg.provider, query);
}

pub fn handle_provider(executor: &mut TuiExecutor, ui: &mut TuiApp, args: &str) {
    let (name, query) = args.split_once(' ').unwrap_or((args, ""));
    let provider = match name {
        "" => match executor.cfg.provider {
            ProviderKind::OpencodeGo | ProviderKind::OpencodeZen => executor.cfg.provider,
            _ => ProviderKind::OpencodeGo,
        },
        "go" | "opencode-go" => ProviderKind::OpencodeGo,
        "zen" | "opencode-zen" => ProviderKind::OpencodeZen,
        _ => {
            ui.push_log(
                "Usage: /provider [go|zen] [model search]; Enter starts a NEW conversation",
            );
            return;
        }
    };
    open_picker(executor, ui, provider, query.trim());
}

fn open_picker(executor: &TuiExecutor, ui: &mut TuiApp, provider: ProviderKind, query: &str) {
    let result = (|| {
        executor.ensure_model_selection_idle(ui)?;
        let mut picker = crate::features::model_selection::ModelPicker::new(provider, query)?;
        let cfg = opencode::selection_config(
            &executor.cfg,
            provider,
            &executor.cfg.model,
            opencode::key_store::available_key(),
        )?;
        picker.key_available = cfg.api_key.is_some();
        if query.is_empty() {
            let preferred = if opencode::api(provider, &cfg.model).is_ok() {
                &cfg.model
            } else {
                "gpt-6-luna"
            };
            picker.selected = picker
                .results()
                .iter()
                .position(|spec| spec.id == preferred)
                .unwrap_or(0);
        }
        Ok::<_, anyhow::Error>(picker)
    })();
    match result {
        Ok(picker) => {
            ui.model_picker = Some(picker);
            ui.dirty = true;
        }
        Err(error) => ui.push_log(format!("Cannot open models: {error}")),
    }
}
