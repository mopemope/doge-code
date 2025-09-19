#[cfg(test)]
mod tests {
    use crate::tui::state::{InputMode, ScrollState, Status, TuiApp, build_render_plan};
    use tui_textarea::TextArea;

    fn create_test_app() -> TuiApp {
        TuiApp::new("test", None, "dark").unwrap()
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
            let params = crate::tui::state::BuildRenderPlanParams {
                title: "Test",
                status: crate::tui::state::Status::Idle,
                log: &app.log,
                textarea: &textarea,
                input_mode: crate::tui::state::InputMode::Normal,
                width,
                height,
                main_content_height,
                model: None,
                spinner_state: 0,
                prompt_tokens: 0,
                total_tokens: None,
                scroll_state,
                todo_list: &[],
                repomap_status: crate::tui::state::RepomapStatus::NotStarted,
            };
            let plan = build_render_plan(params);

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
        let params = crate::tui::state::BuildRenderPlanParams {
            title: "Test",
            status: crate::tui::state::Status::Idle,
            log: &app.log,
            textarea: &textarea,
            input_mode: crate::tui::state::InputMode::Normal,
            width: 80,
            height: 13,
            main_content_height,
            model: None,
            spinner_state: 0,
            prompt_tokens: 0,
            total_tokens: None,
            scroll_state,
            todo_list: &[],
            repomap_status: crate::tui::state::RepomapStatus::NotStarted,
        };
        let plan = build_render_plan(params);

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
        let params = crate::tui::state::BuildRenderPlanParams {
            title: "Test",
            status: crate::tui::state::Status::Idle,
            log: &app.log,
            textarea: &textarea,
            input_mode: crate::tui::state::InputMode::Normal,
            width: 80,
            height: 13,
            main_content_height,
            model: None,
            spinner_state: 0,
            prompt_tokens: 0,
            total_tokens: None,
            scroll_state: &scroll_state,
            todo_list: &[],
            repomap_status: crate::tui::state::RepomapStatus::NotStarted,
        };
        let plan = build_render_plan(params);

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
        let params = crate::tui::state::BuildRenderPlanParams {
            title: "Test",
            status: crate::tui::state::Status::Idle,
            log: &app.log,
            textarea: &textarea,
            input_mode: crate::tui::state::InputMode::Normal,
            width: 80,
            height: 18,
            main_content_height,
            model: None,
            spinner_state: 0,
            prompt_tokens: 0,
            total_tokens: None,
            scroll_state,
            todo_list: &[],
            repomap_status: crate::tui::state::RepomapStatus::NotStarted,
        };
        let plan = build_render_plan(params);

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
            let params = crate::tui::state::BuildRenderPlanParams {
                title: "Test",
                status: crate::tui::state::Status::Idle,
                log: &app.log,
                textarea: &textarea,
                input_mode: crate::tui::state::InputMode::Normal,
                width,
                height: total_height,
                main_content_height: expected_main_height,
                model: None,
                spinner_state: 0,
                prompt_tokens: 0,
                total_tokens: None,
                scroll_state,
                todo_list: &[],
                repomap_status: crate::tui::state::RepomapStatus::NotStarted,
            };
            let plan = build_render_plan(params);

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
        // let scroll_state = &app.scroll_state; // This variable is no longer used directly, as it's passed via params

        let textarea = TextArea::default();
        let params = crate::tui::state::BuildRenderPlanParams {
            title: "Test",
            status: crate::tui::state::Status::Idle,
            log: &app.log,
            textarea: &textarea,
            input_mode: crate::tui::state::InputMode::Normal,
            width: 80,
            height: 8,
            main_content_height,
            model: None,
            spinner_state: 0,
            prompt_tokens: 0,
            total_tokens: None,
            scroll_state: &app.scroll_state,
            todo_list: &[],
            repomap_status: crate::tui::state::RepomapStatus::NotStarted,
        };
        let plan = build_render_plan(params);

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
        let params = crate::tui::state::BuildRenderPlanParams {
            title,
            status,
            log: &log,
            textarea: &textarea,
            input_mode,
            width: w,
            height: h,
            main_content_height: h.saturating_sub(3),
            model,
            spinner_state,
            prompt_tokens: tokens_used,
            total_tokens: None,
            scroll_state: &scroll_state,
            todo_list: &[],
            repomap_status: crate::tui::state::RepomapStatus::NotStarted,
        };
        let plan = build_render_plan(params);

        // Should show the last few lines
        assert!(plan.log_lines.contains(&"line5".to_string()));

        // Test scrolled up
        let scroll_state = ScrollState {
            offset: 2,
            auto_scroll: false,
            ..Default::default()
        };

        let textarea = TextArea::default();
        let params = crate::tui::state::BuildRenderPlanParams {
            title,
            status,
            log: &log,
            textarea: &textarea,
            input_mode,
            width: w,
            height: h,
            main_content_height: h.saturating_sub(3),
            model,
            spinner_state,
            prompt_tokens: tokens_used,
            total_tokens: None,
            scroll_state: &scroll_state,
            todo_list: &[],
            repomap_status: crate::tui::state::RepomapStatus::NotStarted,
        };
        let plan = build_render_plan(params);

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
