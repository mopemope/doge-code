use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;

/// Delegate /clear to the dedicated handler.
/// This separation improves modularity by isolating command logic.
pub fn handle_clear(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    ui.clear_log();
    // Fresh session with no carried-over conversation: the shared helper
    // clears the runtime buffer and turn metadata, then creates the session.
    if let Err(e) = executor.start_new_session(ui, None) {
        ui.push_log(format!("Failed to create new session: {}", e));
        return;
    }

    // Reset LLM client tokens
    if let Some(client) = &executor.client {
        client.set_tokens(0);
        client.set_prompt_tokens(0);
        client.set_reasoning_tokens(0);
        client.clear_totals();
    }

    // Reset TUI token display
    ui.tokens_prompt_used = 0;
    ui.tokens_used = 0;
    ui.tokens_total_used = None;
    ui.remaining_context_tokens = None;
    ui.dirty = true;

    ui.push_log("Cleared conversation history and started new session. Tokens reset to 0.");
}
