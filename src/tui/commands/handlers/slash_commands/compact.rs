use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;

/// Delegate /compact to the dedicated handler.
/// This separation improves modularity by isolating command logic.
pub fn handle_compact(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    executor.handle_compact_command(ui);
}
