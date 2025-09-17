#[cfg(test)]
mod tests {
    use crate::tui::state::{TuiApp, build_render_plan};
    use tui_textarea::TextArea;

    fn create_test_app() -> TuiApp {
        TuiApp::new("test", None, "dark").unwrap()
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
            &[],
            crate::tui::state::RepomapStatus::NotStarted, // Add repomap_status parameter
        );

        // Should show the most recent lines when auto-scrolling
        assert!(plan.log_lines.len() <= 5); // 8 - 3 = 5 max log rows
        assert!(plan.log_lines.contains(&"> User input".to_string()));

        // Scroll info should reflect actual total lines
        if let Some(scroll_info) = plan.scroll_info {
            assert_eq!(scroll_info.total_lines, total_lines);
        }
    }
}
