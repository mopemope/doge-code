use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;

/// Delegate /cancel to the dedicated handler.
/// This separation improves modularity by isolating command logic.
pub fn handle_cancel(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    if let Some(tx) = &executor.cancel_tx {
        let _ = tx.send(true);
        if let Some(tx) = &executor.ui_tx {
            let _ = tx.send("::status:cancelled".into());
        }
        ui.push_log("[Cancelled]");
        executor.cancel_tx = None;
    } else {
        ui.push_log("[no running task]");
    }
    ui.dirty = true;
}
