// use crate::tui::commands::core::TuiExecutor;
use crate::tui::theme::Theme;
use crate::tui::view::TuiApp;

/// Delegate /theme to the dedicated handler.
/// This separation improves modularity by isolating command logic.
pub fn handle_theme(line: &str, ui: &mut TuiApp) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::TuiApp;

    #[test]
    fn test_handle_theme_switch_to_light() {
        let mut app = TuiApp::new("Test App", None, "dark").unwrap();
        handle_theme("/theme light", &mut app);
        assert_eq!(app.theme.name, "light");
    }

    #[test]
    fn test_handle_theme_unknown() {
        let mut app = TuiApp::new("Test App", None, "dark").unwrap();
        handle_theme("/theme invalid", &mut app);
        assert_eq!(app.theme.name, "dark");
        // Verify log message
        match app.log.last().unwrap() {
            crate::tui::state::LogEntry::Plain(msg) => {
                assert!(msg.contains("Unknown theme"), "Log message was: {}", msg);
            }
            _ => panic!("Expected plain log entry"),
        }
    }
}
