use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;

pub fn handle_quit(_executor: &mut TuiExecutor, ui: &mut TuiApp) {
    ui.request_exit();
}
