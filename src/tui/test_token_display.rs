#[cfg(test)]
mod tests {
    use crate::tui::state::{InputMode, Status, build_render_plan};

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

        let params = crate::tui::state::BuildRenderPlanParams {
            title,
            status,
            log: &log,
            textarea: &textarea,
            input_mode,
            width: w,
            height: h,
            main_content_height: h - 3,
            model,
            spinner_state,
            prompt_tokens: tokens_used,
            total_tokens: None,
            scroll_state: &crate::tui::state::ScrollState::default(),
            todo_list: &[],
            repomap_status: crate::tui::state::RepomapStatus::NotStarted,
        };
        let plan = build_render_plan(params);

        // Check that the token count is not included in the title when it's 0
        assert!(!plan.footer_lines[0].contains("tokens:"));
        // Check that the model is still included
        assert!(plan.footer_lines[0].contains("model:test-model"));
    }
}
