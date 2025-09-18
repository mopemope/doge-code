use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;

/// Delegate /tokens to the dedicated handler.
/// This separation improves modularity by isolating command logic.
pub fn handle_tokens(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    if let Some(client) = &executor.client {
        let tokens_used = client.get_prompt_tokens_used();
        ui.push_log(format!("Total prompt tokens used: {}", tokens_used));
    } else {
        ui.push_log("No LLM client available.");
    }
}
