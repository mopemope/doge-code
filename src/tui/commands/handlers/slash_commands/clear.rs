use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;

/// Delegate /clear to the dedicated handler.
/// This separation improves modularity by isolating command logic.
pub fn handle_clear(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    ui.clear_log();
    // Clear conversation history
    if let Ok(mut history) = executor.conversation_history.lock() {
        history.clear();
    }

    // Create new session to reset tokens and metrics
    let mut sm = executor.session_manager.lock().unwrap();
    if let Err(e) = sm.create_session(None) {
        ui.push_log(format!("Failed to create new session: {}", e));
        return;
    }

    // Reset LLM client tokens
    if let Some(client) = &executor.client {
        client.set_tokens(0);
        client.set_prompt_tokens(0);
    }

    // Reset TUI token display
    ui.tokens_prompt_used = 0;
    ui.tokens_used = 0;
    ui.tokens_total_used = None;
    ui.dirty = true;

    ui.push_log("Cleared conversation history and started new session. Tokens reset to 0.");
}
