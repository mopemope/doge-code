use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use std::process;

/// Delegate /quit to the dedicated handler.
/// This separation improves modularity by isolating command logic.
pub fn handle_quit(_executor: &mut TuiExecutor, ui: &mut TuiApp) {
    ui.push_log("[Exiting application]");
    // Graceful exit; in a real TUI, this might set a flag or send a quit signal.
    // For now, exit the process.
    process::exit(0);
}
