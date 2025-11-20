use crate::tui::{
    state::{LogEntry, ScrollState, build_render_plan},
    theme::Theme,
};

#[test]
fn test_build_render_plan_with_scroll() {
    // let mut app = TuiApp::new("Test Title", None, "dark").unwrap(); // 不要
    // let textarea = &app.textarea; // 不要
    // let input_mode = InputMode::Normal; // 不要
    let main_content_height = 8;
    // let scroll_state = ScrollState::default(); // 修正
    let scroll_state = ScrollState {
        offset: 2,
        auto_scroll: false,
        ..Default::default()
    };
    let log_lines: Vec<LogEntry> = vec![
        "Line 1", "Line 2", "Line 3", "Line 4", "Line 5", "Line 6", "Line 7", "Line 8", "Line 9",
        "Line 10",
    ]
    .into_iter()
    .map(|line| LogEntry::Plain(line.to_string()))
    .collect();
    let plan_items: Vec<crate::tui::state::PlanItem> = vec![];
    let theme = Theme::dark();

    let params = crate::tui::state::BuildRenderPlanParams {
        title: "Test Title",
        status: crate::tui::state::Status::Idle,
        log: &log_lines,
        width: 80,
        main_content_height, // height -> main_content_height
        model: None,
        spinner_state: 0,
        scroll_state: &scroll_state,
        plan_list: &plan_items,
        theme: &theme,
        // textarea: &textarea, // 削除
        // input_mode: crate::tui::state::InputMode::Normal, // 削除
        // height, // 削除
        // prompt_tokens: 0, // 削除
        // total_tokens: None, // 削除
        // repomap_status: crate::tui::state::RepomapStatus::NotStarted, // 削除
    };
    let plan = build_render_plan(params);

    // Verify that the log lines are correctly truncated and displayed
    assert_eq!(plan.log_lines.len(), main_content_height as usize);
    assert_eq!(plan.log_lines[0].text(), "Line 1");
    assert_eq!(plan.log_lines[7].text(), "Line 8");

    // Verify that scroll info is present and correct
    assert!(plan.scroll_info.is_some());
    let scroll_info = plan.scroll_info.unwrap();
    assert_eq!(scroll_info.current_line, 8); // 修正: 10 -> 8 (total_lines - offset)
    assert_eq!(scroll_info.total_lines, 10);
    assert!(scroll_info.is_scrolling); // これで成功するはず
    assert_eq!(scroll_info.new_messages, 0);
}
