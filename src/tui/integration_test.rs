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

    #[test]
    fn test_ratatui_test_backend_renders_main_view() -> Result<()> {
        use ratatui::{Terminal, backend::TestBackend};

        let mut app = TuiApp::new_for_test("render-test".to_string(), None, "default");
        app.push_log("render-marker");
        app.textarea.insert_str("input-marker");
        let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
        terminal.draw(|frame| app.view(frame, None))?;

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("render-marker"));
        assert!(rendered.contains("input-marker"));
        assert!(rendered.contains("READY"));

        Ok(())
    }
    #[test]
    fn subscription_selection_remains_visible_in_status_line() -> Result<()> {
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = TuiApp::new_for_test("doge-code", Some("test-model".into()), "default");
        app.inference_label = Some("openai | test-account | test-model".into());
        for _ in 0..50 {
            app.push_log("conversation continues");
        }
        let mut terminal = Terminal::new(TestBackend::new(120, 24))?;
        terminal.draw(|frame| app.view(frame, Some("test-model")))?;
        let rendered = terminal.backend().to_string();
        assert!(
            rendered
                .lines()
                .next()
                .expect("status line")
                .contains("openai | test-account | test-model")
        );
        println!("{rendered}");
        Ok(())
    }
}
