#[cfg(test)]
mod tests {
    use crate::config::AppConfig;
    use crate::tui::state::{LogEntry, Status, TuiApp, build_render_plan};

    #[test]
    fn test_no_token_display_when_zero() {
        // Test that token usage is not displayed when tokens_used = 0
        let title = "Test Title";
        let status = Status::Idle;
        let log: Vec<LogEntry> = Vec::new();
        // let input_mode = InputMode::Normal; // 不要
        let w = 80;
        let h = 24;
        let model = Some("test-model");
        let spinner_state = 0;
        // let tokens_used = 0; // 不要
        // let textarea = crate::tui::state::TuiApp::new("", None, "")
        //     .unwrap()
        //     .textarea; // 不要

        let theme = crate::tui::theme::Theme::dark();
        let params = crate::tui::state::BuildRenderPlanParams {
            title,
            status,
            log: &log,
            width: w,
            main_content_height: h - 3,
            model,
            spinner_state,
            scroll_state: &crate::tui::state::ScrollState::default(),
            plan_list: &[],
            theme: &theme,
        };
        let plan = build_render_plan(params);

        // Check that the token count is not included in the title when it's 0
        assert!(!plan.footer_lines[0].contains("tokens:"));
        // Check that the model is still included
        assert!(plan.footer_lines[0].contains("model:test-model"));
    }

    #[test]
    fn test_remaining_context_tokens_update() {
        // Test that remaining context tokens are updated when prompt tokens change
        let mut app = TuiApp::new("Test", None, "").unwrap();

        // Set initial values
        app.tokens_prompt_used = 100;
        app.auto_compact_prompt_token_threshold = 1000;

        // Mock config with context window size
        let mut cfg = AppConfig::default();
        cfg.llm.context_window_size = Some(4096);

        // Test update_remaining_context_tokens method
        app.update_remaining_context_tokens(cfg.get_context_window_size());

        // Check that remaining tokens are calculated correctly
        assert_eq!(app.remaining_context_tokens, Some(4096 - 100));
        assert!(app.dirty); // Check that dirty flag is set

        // Test with exceeded context
        app.tokens_prompt_used = 5000;
        app.update_remaining_context_tokens(cfg.get_context_window_size());
        assert_eq!(app.remaining_context_tokens, Some(0)); // Should be 0 when exceeded

        // Test with unknown context size
        app.tokens_prompt_used = 100;
        app.update_remaining_context_tokens(None);
        assert_eq!(app.remaining_context_tokens, None);
    }
}
