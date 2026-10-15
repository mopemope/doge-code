#[cfg(test)]
mod tests {
    use crate::tui::state::{LogEntry, TuiApp};

    fn create_test_app() -> TuiApp {
        TuiApp::new("test", None, "dark").unwrap()
    }

    fn log_texts(app: &TuiApp) -> Vec<String> {
        app.log
            .iter()
            .map(|entry| match entry {
                LogEntry::Plain(text) => text.clone(),
                LogEntry::Markdown(text) => text.clone(),
            })
            .collect()
    }

    #[test]
    fn test_push_log_with_auto_scroll() {
        let mut app = create_test_app();
        app.scroll_state.offset = 5;
        app.scroll_state.auto_scroll = true;

        app.push_log("new message");
        assert_eq!(app.scroll_state.offset, 0);
        assert!(app.scroll_state.auto_scroll);
        assert_eq!(app.scroll_state.new_messages, 0);
    }

    #[test]
    fn test_push_log_without_auto_scroll() {
        let mut app = create_test_app();
        app.scroll_state.offset = 5;
        app.scroll_state.auto_scroll = false;

        app.push_log("new message");
        assert_eq!(app.scroll_state.offset, 6); // Should increment by 1
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
    fn test_clear_log_resets_scroll() {
        let mut app = create_test_app();
        app.scroll_state.offset = 10;
        app.scroll_state.auto_scroll = false;

        app.clear_log();
        assert_eq!(app.scroll_state.offset, 0);
        assert!(app.scroll_state.auto_scroll);
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
        let texts = log_texts(&app);
        assert!(texts.contains(&"Line 1".to_string()));
        assert!(texts.contains(&"Line 2".to_string()));
        assert!(texts.contains(&"Line 3".to_string()));
    }

    #[test]
    fn test_margin_log_display() {
        let mut app = create_test_app();

        // Test that margin logs are properly added
        app.push_log("  Indented text");
        app.push_log("    Code block");

        let texts = log_texts(&app);
        assert!(texts.contains(&"  Indented text".to_string()));
        assert!(texts.contains(&"    Code block".to_string()));
    }

    #[test]
    fn test_streaming_log_integration() {
        let mut app = create_test_app();
        app.scroll_state.auto_scroll = false;
        app.scroll_state.offset = 5;

        app.log_width = 20;
        app.push_log("older message");
        let offset = app.scroll_state.offset;
        app.append_stream_token_structured("Hello");
        app.append_stream_token_structured(" 世界");
        app.append_stream_token_structured("!\nNext line");
        assert_eq!(log_texts(&app), ["older message", "Hello 世界!\nNext line"]);
        assert_eq!(app.scroll_state.new_messages, 2);
        assert_eq!(app.scroll_state.offset, offset + app.log_heights[1]);
        let partial = log_texts(&app);
        app.finalize_and_append_llm_response(""); // cancelled / EOF
        assert_eq!(log_texts(&app), partial);
        app.finalize_and_append_llm_response("Hello 世界!\nNext line");
        assert_eq!(log_texts(&app), partial); // repeated finalization
        app.push_log("important tool failure");
        app.append_stream_token_structured("new request");
        app.finalize_and_append_llm_response("new request");
        assert_eq!(
            log_texts(&app),
            [
                "older message",
                "Hello 世界!\nNext line",
                "important tool failure",
                "new request"
            ]
        );
        app.begin_llm_response();
        app.append_stream_token_structured("intermediate answer");
        app.append_inbox_message("intermediate answer"); // agent tool-call response repeats whole content
        app.finalize_and_append_llm_response(""); // tool starts
        assert_eq!(
            log_texts(&app)
                .iter()
                .filter(|s| *s == "intermediate answer")
                .count(),
            1
        );
        // Request boundaries, including identical answers, never deduplicate across requests.
        app.begin_llm_response();
        app.append_stream_token_structured(" ");
        app.append_stream_token_structured("new request");
        app.push_log("warning during stream");
        app.append_stream_token_structured("e\u{0301}🦀");
        app.finalize_and_append_llm_response(" new requeste\u{0301}🦀");
        assert!(log_texts(&app).ends_with(&[
            " new requeste\u{0301}🦀".into(),
            "warning during stream".into()
        ]));
        app.begin_llm_response();
        app.append_stream_token_structured(" new requeste\u{0301}🦀");
        app.finalize_and_append_llm_response("");
        assert_eq!(
            log_texts(&app)
                .iter()
                .filter(|s| *s == " new requeste\u{0301}🦀")
                .count(),
            2
        );
        // Eviction rebases the active entry instead of replacing a warning.
        app.max_log_lines = 3;
        app.begin_llm_response();
        app.append_stream_token_structured("partial");
        app.push_log("error after stream start");
        app.append_stream_token_structured(" answer");
        app.finalize_and_append_llm_response("");
        assert!(
            log_texts(&app)
                .ends_with(&["partial answer".into(), "error after stream start".into()])
        );
        assert_eq!(app.log.len(), app.log_heights.len());
        app.append_stream_token_structured("next partial");
        app.clear_log();
        app.push_log("keep user message");
        app.append_stream_token_structured("\u{1b}]0;hidden-control\u{7}");
        app.finalize_and_append_llm_response("");
        assert_eq!(log_texts(&app), ["keep user message"]);
        app.clear_log();
        app.append_stream_token_structured("after clear");
        assert_eq!(log_texts(&app), ["after clear"]);
    }
}
