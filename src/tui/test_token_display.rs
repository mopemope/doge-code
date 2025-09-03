#[cfg(test)]
mod tests {
    use crate::tui::state::{InputMode, Status, build_render_plan};

    #[test]
    fn test_token_display_in_title() {
        // Test that token usage is displayed in the title when tokens_used > 0
        let title = "Test Title";
        let status = Status::Idle;
        let log = vec![];
        let input_mode = InputMode::Normal;
        let w = 80;
        let h = 24;
        let model = Some("test-model");
        let spinner_state = 0;
        let tokens_used = 1234;
        let textarea = crate::tui::state::TuiApp::new("", None, "")
            .unwrap()
            .textarea;

        let plan = build_render_plan(
            title,
            status,
            &log,
            &textarea,
            input_mode,
            w,
            h,
            h - 3, // main_content_height
            model,
            spinner_state,
            tokens_used,
            None,
            &crate::tui::state::ScrollState::default(),
        );

        // Check that the token count is included in the title
        assert!(plan.footer_lines[0].contains("tokens:1234"));
        // Check that the model is still included
        assert!(plan.footer_lines[0].contains("model:test-model"));
    }

    #[test]
    fn test_no_token_display_when_zero() {
        // Test that token usage is not displayed when tokens_used = 0
        let title = "Test Title";
        let status = Status::Idle;
        let log = vec![];
        let input_mode = InputMode::Normal;
        let w = 80;
        let h = 24;
        let model = Some("test-model");
        let spinner_state = 0;
        let tokens_used = 0;
        let textarea = crate::tui::state::TuiApp::new("", None, "")
            .unwrap()
            .textarea;

        let plan = build_render_plan(
            title,
            status,
            &log,
            &textarea,
            input_mode,
            w,
            h,
            h - 3, // main_content_height
            model,
            spinner_state,
            tokens_used,
            None,
            &crate::tui::state::ScrollState::default(),
        );

        // Check that the token count is not included in the title when it's 0
        assert!(!plan.footer_lines[0].contains("tokens:"));
        // Check that the model is still included
        assert!(plan.footer_lines[0].contains("model:test-model"));
    }
}
