#[cfg(test)]
mod tests {
    use crate::tui::state::{InputMode, ScrollState, Status, TuiApp, build_render_plan};
    use tui_textarea::TextArea;

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

    #[test]
    fn test_code_block_formatting() {
        let mut app = create_test_app();

        // Simulate finalize_and_append_llm_response with code block
        let content = "Here's some code:\n```rust\nfn main() {\n    println!(\"Hello!\");\n}\n```\nThat's it!";
        app.finalize_and_append_llm_response(content);

        // Check that code block was properly formatted
        assert!(
            app.log
                .iter()
                .any(|line| line.contains("Here's some code:"))
        );
        assert!(app.log.iter().any(|line| line.contains("```rust")));
        assert!(app.log.iter().any(|line| line.contains("fn main()")));
        assert!(app.log.iter().any(|line| line.contains("println!")));
        assert!(app.log.iter().any(|line| line == "  ```"));
        assert!(app.log.iter().any(|line| line.contains("That's it!")));
    }

    #[test]
    fn test_scroll_calculation_accuracy() {
        let mut app = create_test_app();

        // Add various types of log entries
        app.push_log("[tool] fs_read({\"path\": \"test.rs\"})");
        app.push_log("  LLM response line 1");
        app.push_log("  ```rust");
        app.push_log("    fn test() {}");
        app.push_log("  ```");
        app.push_log("  LLM response line 2");
        app.push_log("> User input");

        let total_lines = app.log.len();

        // Test scroll calculation with small viewport
        let scroll_state = &app.scroll_state;
        let textarea = TextArea::default();
        let plan = build_render_plan(
            "Test",
            crate::tui::state::Status::Idle,
            &app.log,
            &textarea,
            crate::tui::state::InputMode::Normal,
            80,
            8,
            8_u16.saturating_sub(3),
            None,
            0,
            0,
            None,
            scroll_state,
        );

        // Should show the most recent lines when auto-scrolling
        assert!(plan.log_lines.len() <= 5); // 8 - 3 = 5 max log rows
        assert!(plan.log_lines.contains(&"> User input".to_string()));

        // Scroll info should reflect actual total lines
        if let Some(scroll_info) = plan.scroll_info {
            assert_eq!(scroll_info.total_lines, total_lines);
        }
    }

    #[test]
    fn test_actual_display_area_usage() {
        let mut app = create_test_app();

        // Add many log entries to test scrolling
        for i in 0..20 {
            app.push_log(format!("Log line {}", i));
        }

        // Test with different screen sizes
        let test_cases: Vec<(u16, u16)> = vec![
            (80, 10),  // Small screen
            (80, 24),  // Standard screen
            (120, 40), // Large screen
        ];

        for (width, height) in test_cases {
            let main_content_height = height.saturating_sub(3); // header(2) + footer(1)
            let scroll_state = &app.scroll_state;

            let textarea = TextArea::default();
            let plan = build_render_plan(
                "Test",
                crate::tui::state::Status::Idle,
                &app.log,
                &textarea,
                crate::tui::state::InputMode::Normal,
                width,
                height,
                main_content_height,
                None,
                0,
                0,
                None,
                scroll_state,
            );

            // Log lines should not exceed the actual main content area height
            assert!(
                plan.log_lines.len() <= main_content_height as usize,
                "Screen {}x{}: log_lines.len()={} > main_content_height={}",
                width,
                height,
                plan.log_lines.len(),
                main_content_height
            );

            // Should show the most recent lines when auto-scrolling
            assert!(
                plan.log_lines.contains(&"Log line 19".to_string()),
                "Screen {}x{}: Should contain the most recent log line",
                width,
                height
            );
        }
    }

    #[test]
    fn test_long_content_scrollability() {
        let mut app = create_test_app();

        // Add a lot of content to ensure scrolling is needed
        for i in 0..100 {
            app.push_log(format!("Long content line {}", i));
        }

        let total_lines = app.log.len();
        let main_content_height = 10; // Simulate small screen

        // Test auto-scroll (should show latest)
        let scroll_state = &app.scroll_state;
        let textarea = TextArea::default();
        let plan = build_render_plan(
            "Test",
            crate::tui::state::Status::Idle,
            &app.log,
            &textarea,
            crate::tui::state::InputMode::Normal,
            80,
            13,
            main_content_height,
            None,
            0,
            0,
            None,
            scroll_state,
        );

        // Should show exactly main_content_height lines
        assert_eq!(plan.log_lines.len(), main_content_height as usize);

        // Should show the most recent lines
        assert!(plan.log_lines.contains(&"Long content line 99".to_string()));

        // Should have scroll info indicating scrolling is possible
        assert!(plan.scroll_info.is_some());
        let scroll_info = plan.scroll_info.unwrap();
        assert_eq!(scroll_info.total_lines, total_lines);
        assert!(scroll_info.total_lines > main_content_height as usize);

        // Test scrolled up
        let mut scroll_state = app.scroll_state.clone();
        scroll_state.offset = 20;
        scroll_state.auto_scroll = false;

        let textarea = TextArea::default();
        let plan = build_render_plan(
            "Test",
            crate::tui::state::Status::Idle,
            &app.log,
            &textarea,
            crate::tui::state::InputMode::Normal,
            80,
            13,
            main_content_height,
            None,
            0,
            0,
            None,
            &scroll_state,
        );

        // Should not show the latest line when scrolled up
        assert!(!plan.log_lines.contains(&"Long content line 99".to_string()));

        // Should show older content
        assert!(
            plan.log_lines
                .iter()
                .any(|line| line.contains("Long content line"))
        );

        // Should indicate scrolling
        assert!(plan.scroll_info.is_some());
        let scroll_info = plan.scroll_info.unwrap();
        assert!(scroll_info.is_scrolling);
    }

    #[test]
    fn test_debug_display_issue_reproduction() {
        let mut app = create_test_app();

        // Simulate tool output that might cause display issues
        app.push_log("[tool] fs_read({\"path\": \"large_file.rs\"})");

        // Add many lines of LLM response with margins
        for i in 0..50 {
            app.push_log(format!(
                "  This is LLM response line {} with some content that might be long",
                i
            ));
        }

        // Add code block
        app.push_log("  ```rust");
        for i in 0..20 {
            app.push_log(format!("    fn function_{}() {{", i));
            app.push_log("        // Some code here");
            app.push_log("    }");
        }
        app.push_log("  ```");

        // Add more response
        for i in 50..100 {
            app.push_log(format!(
                "  Final response line {} that should be visible",
                i
            ));
        }

        let total_log_lines = app.log.len();
        println!("Total log lines: {}", total_log_lines);

        // Test with small screen
        let main_content_height = 15;
        let scroll_state = &app.scroll_state;

        let textarea = TextArea::default();
        let plan = build_render_plan(
            "Test",
            crate::tui::state::Status::Idle,
            &app.log,
            &textarea,
            crate::tui::state::InputMode::Normal,
            80,
            18,
            main_content_height,
            None,
            0,
            0,
            None,
            scroll_state,
        );

        println!("Plan log_lines count: {}", plan.log_lines.len());
        println!("Last few lines in plan:");
        for (i, line) in plan.log_lines.iter().rev().take(5).enumerate() {
            println!("  -{}: {}", i, line);
        }

        // The last line should be visible
        assert!(
            plan.log_lines
                .iter()
                .any(|line| line.contains("Final response line 99")),
            "Last line should be visible in auto-scroll mode"
        );

        // Should not exceed main content height
        assert!(plan.log_lines.len() <= main_content_height as usize);
    }

    #[test]
    fn test_header_footer_consideration() {
        let mut app = create_test_app();

        // Add exactly enough content to fill different screen sizes
        for i in 0..50 {
            app.push_log(format!("Content line {}", i));
        }

        // Test different screen configurations
        let test_cases = vec![
            (80, 10, 7),  // Small: total=10, header=2, footer=1, main=7
            (80, 24, 21), // Standard: total=24, header=2, footer=1, main=21
            (80, 30, 27), // Large: total=30, header=2, footer=1, main=27
        ];

        for (width, total_height, expected_main_height) in test_cases {
            println!("Testing screen {}x{}", width, total_height);

            let scroll_state = &app.scroll_state;
            let textarea = TextArea::default();
            let plan = build_render_plan(
                "Test",
                crate::tui::state::Status::Idle,
                &app.log,
                &textarea,
                crate::tui::state::InputMode::Normal,
                width,
                total_height,
                expected_main_height,
                None,
                0,
                0,
                None,
                scroll_state,
            );

            println!("  Expected main height: {}", expected_main_height);
            println!("  Plan log_lines count: {}", plan.log_lines.len());

            // Should not exceed the expected main content area height
            assert!(
                plan.log_lines.len() <= expected_main_height as usize,
                "Screen {}x{}: log_lines.len()={} > expected_main_height={}",
                width,
                total_height,
                plan.log_lines.len(),
                expected_main_height
            );

            // Should show the most recent content when auto-scrolling
            assert!(
                plan.log_lines
                    .iter()
                    .any(|line| line.contains("Content line 49")),
                "Screen {}x{}: Should show the most recent line",
                width,
                total_height
            );
        }
    }

    #[test]
    fn test_exact_line_count_control() {
        let mut app = create_test_app();

        // Add many lines to ensure scrolling
        for i in 0..100 {
            app.push_log(format!("Line {}", i));
        }

        // Test with very small main content area
        let main_content_height = 5;
        let scroll_state = &app.scroll_state;

        let textarea = TextArea::default();
        let plan = build_render_plan(
            "Test",
            crate::tui::state::Status::Idle,
            &app.log,
            &textarea,
            crate::tui::state::InputMode::Normal,
            80,
            8,
            main_content_height,
            None,
            0,
            0,
            None,
            scroll_state,
        );

        println!("Main content height: {}", main_content_height);
        println!("Plan log_lines count: {}", plan.log_lines.len());
        println!("Lines in plan:");
        for (i, line) in plan.log_lines.iter().enumerate() {
            println!("  {}: {}", i, line);
        }

        // Should show exactly the main content height or less
        assert!(
            plan.log_lines.len() <= main_content_height as usize,
            "log_lines.len()={} should be <= main_content_height={}",
            plan.log_lines.len(),
            main_content_height
        );

        // Should show exactly main_content_height lines when there's enough content
        assert_eq!(
            plan.log_lines.len(),
            main_content_height as usize,
            "Should show exactly {} lines when there's enough content",
            main_content_height
        );

        // Should show the most recent lines
        assert!(
            plan.log_lines.contains(&"Line 99".to_string()),
            "Should contain the most recent line"
        );
        assert!(
            plan.log_lines.contains(&"Line 95".to_string()),
            "Should contain line from 5 lines ago"
        );
    }

    #[test]
    fn test_build_render_plan_with_scroll() {
        let title = "Test";
        let status = Status::Idle;
        let log = vec![
            "line1".to_string(),
            "line2".to_string(),
            "line3".to_string(),
            "line4".to_string(),
            "line5".to_string(),
        ];

        let input_mode = InputMode::Normal;
        let w = 80;
        let h: u16 = 6; // Small height to force scrolling
        let model = None;
        let spinner_state = 0;
        let tokens_used = 0;

        // Test auto-scroll (show latest)
        let scroll_state = ScrollState::default();
        let textarea = TextArea::default();
        let plan = build_render_plan(
            title,
            status,
            &log,
            &textarea,
            input_mode,
            w,
            h,
            h.saturating_sub(3),
            model,
            spinner_state,
            tokens_used,
            None,
            &scroll_state,
        );

        // Should show the last few lines
        assert!(plan.log_lines.contains(&"line5".to_string()));

        // Test scrolled up
        let scroll_state = ScrollState {
            offset: 2,
            auto_scroll: false,
            ..Default::default()
        };

        let textarea = TextArea::default();
        let plan = build_render_plan(
            title,
            status,
            &log,
            &textarea,
            input_mode,
            w,
            h,
            h.saturating_sub(3),
            model,
            spinner_state,
            tokens_used,
            None,
            &scroll_state,
        );

        // Should not show the latest line
        assert!(!plan.log_lines.contains(&"line5".to_string()));

        // Should have scroll info
        assert!(plan.scroll_info.is_some());
        let scroll_info = plan.scroll_info.unwrap();
        assert!(scroll_info.is_scrolling);
        assert_eq!(scroll_info.total_lines, 5);
        assert_eq!(scroll_info.new_messages, 0);
    }
}
