//! Test for scroll functionality

use crate::tui::state::{InputMode, TuiApp};
use anyhow::Result;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scroll_up_basic() -> Result<()> {
        let mut app = TuiApp::new("test".to_string(), None, "default")?;
        app.input_mode = InputMode::Normal;

        // Initial state
        assert_eq!(app.scroll_state.offset, 0);
        assert!(app.scroll_state.auto_scroll);

        // Test scroll up by 3 lines
        app.scroll_up(3);

        // After scroll up, auto_scroll should be disabled and offset should increase
        assert!(!app.scroll_state.auto_scroll);
        assert_eq!(app.scroll_state.offset, 3);

        Ok(())
    }

    #[test]
    fn test_scroll_down_basic() -> Result<()> {
        let mut app = TuiApp::new("test".to_string(), None, "default")?;
        app.input_mode = InputMode::Normal;

        // Set up initial scrolled state
        app.scroll_up(5);
        assert_eq!(app.scroll_state.offset, 5);
        assert!(!app.scroll_state.auto_scroll);

        // Test scroll down by 3 lines
        app.scroll_down(3);

        // After scroll down, offset should decrease
        assert_eq!(app.scroll_state.offset, 2); // 5 - 3 = 2
        assert!(!app.scroll_state.auto_scroll);

        Ok(())
    }

    #[test]
    fn test_scroll_down_to_bottom() -> Result<()> {
        let mut app = TuiApp::new("test".to_string(), None, "default")?;
        app.input_mode = InputMode::Normal;

        // Set up scrolled state close to bottom
        app.scroll_up(2); // offset = 2
        assert_eq!(app.scroll_state.offset, 2);
        assert!(!app.scroll_state.auto_scroll);

        // Test scroll down that should reach bottom
        app.scroll_down(3);

        // Should reach bottom and enable auto_scroll
        assert_eq!(app.scroll_state.offset, 0);
        assert!(app.scroll_state.auto_scroll);
        assert_eq!(app.scroll_state.new_messages, 0);

        Ok(())
    }

    #[test]
    fn test_scroll_to_top_bottom() -> Result<()> {
        let mut app = TuiApp::new("test".to_string(), None, "default")?;

        // Add some log entries to scroll
        for i in 0..10 {
            app.push_log(format!("Log line {}", i));
        }

        // Test scroll to top
        app.scroll_to_top();
        assert_eq!(app.scroll_state.offset, 10);
        assert!(!app.scroll_state.auto_scroll);

        // Test scroll to bottom
        app.scroll_to_bottom();
        assert_eq!(app.scroll_state.offset, 0);
        assert!(app.scroll_state.auto_scroll);
        assert_eq!(app.scroll_state.new_messages, 0);

        Ok(())
    }

    #[test]
    fn test_page_up_down() -> Result<()> {
        let mut app = TuiApp::new("test".to_string(), None, "default")?;

        // Add some log entries
        for i in 0..20 {
            app.push_log(format!("Log line {}", i));
        }

        // Test page up (should scroll up by visible_lines - 1)
        app.page_up(20); // Simulate 20 visible lines
        assert!(!app.scroll_state.auto_scroll);
        assert_eq!(app.scroll_state.offset, 19); // 20 - 1 = 19

        // Test page down
        app.page_down(20);
        assert_eq!(app.scroll_state.offset, 0); // Should reach bottom
        assert!(app.scroll_state.auto_scroll);

        Ok(())
    }
}
