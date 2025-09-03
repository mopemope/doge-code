#[cfg(test)]
mod tests {
    use crate::tui::state::TuiApp;

    fn create_test_app() -> TuiApp {
        TuiApp::new("test", None, "dark").unwrap()
    }

    #[test]
    fn test_push_log_with_auto_scroll() {
        let mut app = create_test_app();
        app.scroll_state.offset = 5;
        app.scroll_state.auto_scroll = true;

        app.push_log("new message");
        assert_eq!(app.scroll_state.offset, 0);
        assert!(app.scroll_state.auto_scroll);
    }

    #[test]
    fn test_push_log_without_auto_scroll() {
        let mut app = create_test_app();
        app.scroll_state.offset = 5;
        app.scroll_state.auto_scroll = false;

        app.push_log("new message");
        assert_eq!(app.scroll_state.offset, 5); // Should not change
        assert!(!app.scroll_state.auto_scroll);
    }

    #[test]
    fn test_new_message_counting() {
        let mut app = create_test_app();
        app.scroll_state.auto_scroll = false;
        app.scroll_state.offset = 5;

        // Add a message while scrolled up
        app.push_log("new message");
        assert_eq!(app.scroll_state.new_messages, 1);

        // Add another message
        app.push_log("another message");
        assert_eq!(app.scroll_state.new_messages, 2);

        // Scroll to bottom should reset count
        app.scroll_to_bottom();
        assert_eq!(app.scroll_state.new_messages, 0);
    }

    #[test]
    fn test_new_message_not_counted_when_auto_scroll() {
        let mut app = create_test_app();
        assert!(app.scroll_state.auto_scroll);

        app.push_log("message 1");
        assert_eq!(app.scroll_state.new_messages, 0);

        app.push_log("message 2");
        assert_eq!(app.scroll_state.new_messages, 0);
    }

    #[test]
    fn test_clear_log_resets_scroll() {
        let mut app = create_test_app();
        app.scroll_state.offset = 10;
        app.scroll_state.auto_scroll = false;

        app.clear_log();
        assert_eq!(app.scroll_state.offset, 0);
        assert!(app.scroll_state.auto_scroll);
    }

    #[test]
    fn test_tool_log_counting() {
        let mut app = create_test_app();
        app.scroll_state.auto_scroll = false;
        app.scroll_state.offset = 5;

        // Simulate tool execution log
        app.push_log("[tool] fs_read({\"path\": \"test.rs\"})");
        assert_eq!(app.scroll_state.new_messages, 1);

        // Simulate LLM response with multiple lines (like finalize_and_append_llm_response would do)
        app.push_log("  Here's the content of the file:");
        app.push_log("  ```rust");
        app.push_log("    fn main() {");
        app.push_log("        println!(\"Hello, world!\");");
        app.push_log("    }");
        app.push_log("  ```");

        // Should count all the lines added
        assert_eq!(app.scroll_state.new_messages, 7); // 1 tool + 6 response lines
    }

    #[test]
    fn test_multiline_log_counting() {
        let mut app = create_test_app();
        app.scroll_state.auto_scroll = false;
        app.scroll_state.offset = 5;

        // Test multiline log entry (like what push_log does with \n)
        app.push_log("Line 1\nLine 2\nLine 3");
        assert_eq!(app.scroll_state.new_messages, 3);

        // Verify the lines were actually added
        assert!(app.log.contains(&"Line 1".to_string()));
        assert!(app.log.contains(&"Line 2".to_string()));
        assert!(app.log.contains(&"Line 3".to_string()));
    }

    #[test]
    fn test_margin_log_display() {
        let mut app = create_test_app();

        // Test that margin logs are properly added
        app.push_log("  Indented text");
        app.push_log("    Code block");

        assert!(app.log.contains(&"  Indented text".to_string()));
        assert!(app.log.contains(&"    Code block".to_string()));
    }

    #[test]
    fn test_streaming_log_integration() {
        let mut app = create_test_app();
        app.scroll_state.auto_scroll = false;
        app.scroll_state.offset = 5;

        // Simulate streaming tokens being added immediately
        app.append_stream_token_structured("Hello");
        app.append_stream_token_structured(" world");
        app.append_stream_token_structured("!\nNext line");

        // Check that streaming content was added to log immediately
        assert!(app.log.iter().any(|line| line.contains("Hello")));
        assert!(app.log.iter().any(|line| line.contains("world")));
        assert!(app.log.iter().any(|line| line.contains("Next line")));

        // Check that new messages were counted
        assert!(app.scroll_state.new_messages > 0);
    }
}
