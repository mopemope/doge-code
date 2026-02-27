#[cfg(test)]
mod tests {
    use crate::tui::state::{CompletionType, TuiApp};

    fn create_test_app() -> TuiApp {
        // App creation requires title, model string, theme.
        TuiApp::new("test", None, "dark").unwrap()
    }

    #[test]
    fn test_update_completion_candidates_command() {
        let mut app = create_test_app();

        // Initial state should be inactive
        assert!(!app.completion_active);

        // Trigger generic command search
        app.update_completion_candidates("/");

        // Should activate and find commands like /help, /exit, etc.
        assert!(app.completion_active);
        assert!(!app.completion_candidates.is_empty());
        assert_eq!(app.completion_index, 0);
        assert_eq!(app.completion_scroll, 0);
        assert_eq!(app.completion_type, CompletionType::Command);
    }

    #[test]
    fn test_update_completion_candidates_no_match() {
        let mut app = create_test_app();

        // Trigger a fake command search
        app.update_completion_candidates("/this_command_does_not_exist_at_all");

        // State should properly deactivate
        assert!(!app.completion_active);
        assert!(app.completion_candidates.is_empty());
        assert_eq!(app.completion_type, CompletionType::None);
    }

    #[test]
    fn test_update_completion_candidates_resets_navigation_indices() {
        let mut app = create_test_app();

        // Intentionally pollute the navigation indices
        app.completion_index = 5;
        app.completion_scroll = 2;

        // Perform an update that yields results
        app.update_completion_candidates("/h"); // "help"

        // Verify indexes are forcefully reset to starting positions
        // to prevent out-of-bounds panics if candidate list length shrunk.
        assert_eq!(app.completion_index, 0);
        assert_eq!(app.completion_scroll, 0);
    }
}
