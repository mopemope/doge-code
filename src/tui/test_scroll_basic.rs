#[cfg(test)]
mod tests {
    use crate::tui::state::{ScrollState, TuiApp};

    fn create_test_app() -> TuiApp {
        TuiApp::new("test", None, "dark").unwrap()
    }

    #[test]
    fn test_scroll_state_default() {
        let scroll_state = ScrollState::default();
        assert_eq!(scroll_state.offset, 0);
        assert!(scroll_state.auto_scroll);
        assert_eq!(scroll_state.new_messages, 0);
    }

    #[test]
    fn test_scroll_up() {
        let mut app = create_test_app();
        app.scroll_up(5);
        assert_eq!(app.scroll_state.offset, 5);
        assert!(!app.scroll_state.auto_scroll);
        assert!(app.dirty);
    }

    #[test]
    fn test_scroll_down() {
        let mut app = create_test_app();
        app.scroll_state.offset = 10;
        app.scroll_state.auto_scroll = false;

        app.scroll_down(3);
        assert_eq!(app.scroll_state.offset, 7);
        assert!(!app.scroll_state.auto_scroll);

        // Scroll down to bottom should enable auto_scroll
        app.scroll_down(10);
        assert_eq!(app.scroll_state.offset, 0);
        assert!(app.scroll_state.auto_scroll);
    }

    #[test]
    fn test_scroll_to_top() {
        let mut app = create_test_app();
        app.push_log("line1");
        app.push_log("line2");
        app.push_log("line3");

        app.scroll_to_top();
        assert_eq!(app.scroll_state.offset, app.log.len());
        assert!(!app.scroll_state.auto_scroll);
        assert!(app.dirty);
    }

    #[test]
    fn test_scroll_to_bottom() {
        let mut app = create_test_app();
        app.scroll_state.offset = 10;
        app.scroll_state.auto_scroll = false;

        app.scroll_to_bottom();
        assert_eq!(app.scroll_state.offset, 0);
        assert!(app.scroll_state.auto_scroll);
        assert!(app.dirty);
    }

    #[test]
    fn test_page_up_down() {
        let mut app = create_test_app();
        let visible_lines = 20;

        app.page_up(visible_lines);
        assert_eq!(app.scroll_state.offset, 19); // visible_lines - 1
        assert!(!app.scroll_state.auto_scroll);

        app.page_down(visible_lines);
        assert_eq!(app.scroll_state.offset, 0);
        assert!(app.scroll_state.auto_scroll);
    }
}
