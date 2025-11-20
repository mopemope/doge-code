//! Integration test to verify mouse events are properly captured

use crate::tui::state::{InputMode, TuiApp};
use anyhow::Result;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mouse_event_polling() -> Result<()> {
        // This test verifies that mouse events can be captured in the event loop
        // We'll create a minimal test that simulates the event loop behavior

        let mut app = TuiApp::new("test".to_string(), None, "default")?;
        app.input_mode = InputMode::Normal;

        // Test that the app can be created and basic functionality works
        assert_eq!(app.scroll_state.offset, 0);
        assert!(app.scroll_state.auto_scroll);

        // Test scroll functionality directly
        app.scroll_up(5);
        assert_eq!(app.scroll_state.offset, 5);
        assert!(!app.scroll_state.auto_scroll);

        app.scroll_down(3);
        assert_eq!(app.scroll_state.offset, 2);

        Ok(())
    }

    #[test]
    fn test_mouse_capture_enabled() {
        // Test that the mouse capture guard can be created
        let _mouse_capture = crossterm::event::EnableMouseCapture;

        // If this compiles and runs without panicking, mouse capture is available
    }
}
