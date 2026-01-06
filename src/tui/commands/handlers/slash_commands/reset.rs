use crate::tui::state::Status;
use crate::tui::view::TuiApp;

/// Handle /reset command to force reset the TUI state.
pub fn handle_reset(ui: &mut TuiApp) {
    ui.status = Status::Idle;
    ui.pending_instructions.clear();

    // Reset other processing flags
    ui.is_llm_response_active = false;
    ui.spinner_state = 0;
    ui.current_stream_start = None;
    ui.last_llm_response_content = None;
    ui.llm_parsing_buffer.clear();

    ui.push_log("[SYSTEM] State forced reset to Idle. Instruction queue cleared.");
    ui.dirty = true;
}
