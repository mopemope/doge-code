// src/tui/view.rs collects view-related logic for the TUI.
// The actual implementation is split across event_loop.rs, rendering.rs, and llm_response_handler.rs.
// This file simply re-exports those modules.

// Re-export TuiApp struct and related items
pub use crate::tui::state::TuiApp;
