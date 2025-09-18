use crate::tui::commands::core::TuiExecutor;
use crate::tui::theme::Theme;
use crate::tui::view::TuiApp;

/// Delegate /theme to the dedicated handler.
/// This separation improves modularity by isolating command logic.
pub fn handle_theme(_executor: &mut TuiExecutor, line: &str, ui: &mut TuiApp) {
    let theme_name = line[7..].trim(); // Skip "/theme "
    match theme_name.to_lowercase().as_str() {
        "dark" => {
            ui.theme = Theme::dark();
            ui.push_log("[Theme switched to dark]");
        }
        "light" => {
            ui.theme = Theme::light();
            ui.push_log("[Theme switched to light]");
        }
        _ => {
            ui.push_log(format!(
                "[Unknown theme: {}. Available themes: dark, light]",
                theme_name
            ));
        }
    }
    ui.dirty = true; // Trigger redraw
}
