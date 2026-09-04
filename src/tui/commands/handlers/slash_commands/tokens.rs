use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;

/// Delegate /tokens to the dedicated handler.
/// This separation improves modularity by isolating command logic.
pub fn handle_tokens(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    if let Some(client) = &executor.client {
        let last_prompt = client.get_prompt_tokens_used();
        let session_prompt = client.get_total_prompt_tokens_used();
        let session_total = client.get_total_tokens_used();
        ui.push_log(format!(
            "Session totals: {} prompt tokens ({} total incl. completions)",
            session_prompt, session_total
        ));
        ui.push_log(format!("Last request prompt size: {} tokens", last_prompt));
        let window = executor.cfg.get_context_window_size();
        if let Some(window) = window {
            let remaining = window.saturating_sub(last_prompt);
            ui.push_log(format!(
                "Remaining context: ~{} tokens (window: {})",
                remaining, window
            ));
        }
    } else {
        ui.push_log("No LLM client available.");
    }
}
