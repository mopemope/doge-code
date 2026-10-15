#[cfg(test)]
mod tests {
    use crate::config::AppConfig;
    use crate::tui::state::TuiApp;
    use crate::tui::state_render::truncate_display;

    #[test]
    fn normal_status_omits_usage_but_explicit_dashboard_keeps_it() {
        let mut app = TuiApp::new("Test", None, "dark").unwrap();
        app.model = Some("test-model API_KEY=MODEL_SECRET".into());
        app.tokens_prompt_used = 12345;
        app.plan_list = vec![crate::tui::state::PlanItem {
            id: "one".into(),
            content: "detail-plan-marker API_KEY=PLAN_SECRET".into(),
            status: "in_progress".into(),
            parent_id: None,
        }];
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|frame| app.view(frame, Some("test-model")))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("test-model"));
        assert!(
            !text.contains("12345") && !text.contains("tokens"),
            "{text}"
        );
        assert!(!text.contains("detail-plan-marker"), "{text}");
        app.view_mode = crate::tui::state::ViewMode::Dashboard;
        terminal
            .draw(|frame| app.view(frame, Some("test-model")))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("Tokens Used: 12345"), "{text}");
        assert!(text.contains("detail-plan-marker"), "{text}");
        assert!(
            !text.contains("MODEL_SECRET") && !text.contains("PLAN_SECRET"),
            "{text}"
        );
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

    #[test]
    fn test_truncate_display() {
        assert_eq!(truncate_display("hello", 3), "hel");
        assert_eq!(truncate_display("hello", 5), "hello");
        assert_eq!(truncate_display("hello", 10), "hello");
        // Emoji test: 👨‍👩‍👧‍👦 is a single grapheme cluster but display width might be 2 or more depending on terminal.
        // In unicode-width, it might be width 2.
        // Let's test basic CJK.
        assert_eq!(truncate_display("こんにちは", 4), "こん");
        assert_eq!(truncate_display("こんにちは", 3), "こ"); // 'ん' is width 2, so 2+2=4 > 3, stop at 'こ'
    }
}
