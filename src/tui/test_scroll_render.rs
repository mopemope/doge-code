#[cfg(test)]
mod tests {
    use crate::tui::state::{TuiApp, build_render_plan};

    fn create_test_app() -> TuiApp {
        TuiApp::new("test", None, "dark").unwrap()
    }

    fn extract_texts(plan: &crate::tui::state::RenderPlan) -> Vec<String> {
        plan.log_lines.iter().map(|line| line.text()).collect()
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

            let params = crate::tui::state::BuildRenderPlanParams {
                title: "Test",
                status: crate::tui::state::Status::Ready,
                log: &app.log,
                width,
                main_content_height,
                model: None,
                spinner_state: 0,
                scroll_state,
                theme: &app.theme,
                log_heights: &app.log_heights,
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
            let texts = extract_texts(&plan);
            assert!(
                texts.contains(&"Log line 19".to_string()),
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
        let params = crate::tui::state::BuildRenderPlanParams {
            title: "Test",
            status: crate::tui::state::Status::Ready,
            log: &app.log,
            width: 80,
            main_content_height,
            model: None,
            spinner_state: 0,
            scroll_state,
            theme: &app.theme,
            log_heights: &app.log_heights,
        };
        let plan = build_render_plan(params);

        // Should show exactly main_content_height lines
        assert_eq!(plan.log_lines.len(), main_content_height as usize);

        // Should show the most recent lines
        let texts = extract_texts(&plan);
        assert_eq!(
            texts.first().map(String::as_str),
            Some("Long content line 90")
        );
        assert_eq!(
            texts.last().map(String::as_str),
            Some("Long content line 99")
        );

        // Should have scroll info indicating scrolling is possible
        assert!(plan.scroll_info.is_some());
        let scroll_info = plan.scroll_info.unwrap();
        assert_eq!(scroll_info.total_lines, total_lines);
        assert!(scroll_info.total_lines > main_content_height as usize);

        // Test scrolled up
        let mut scroll_state = app.scroll_state.clone();
        scroll_state.offset = 20;
        scroll_state.auto_scroll = false;

        let params = crate::tui::state::BuildRenderPlanParams {
            title: "Test",
            status: crate::tui::state::Status::Ready,
            log: &app.log,
            width: 80,
            main_content_height,
            model: None,
            spinner_state: 0,
            scroll_state: &scroll_state,
            theme: &app.theme,
            log_heights: &app.log_heights,
        };
        let plan = build_render_plan(params);

        // Should not show the latest line when scrolled up
        let texts = extract_texts(&plan);
        assert!(!texts.contains(&"Long content line 99".to_string()));

        // Should show older content
        assert!(texts.iter().any(|line| line.contains("Long content line")));

        // Should indicate scrolling
        assert!(plan.scroll_info.is_some());
        let scroll_info = plan.scroll_info.unwrap();
        assert!(scroll_info.is_scrolling);
    }
}
