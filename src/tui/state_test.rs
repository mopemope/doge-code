#[cfg(test)]
use crate::TuiApp;
use crate::tui::commands::core::CommandHandler;
use std::any::Any;

struct MockCommandHandler {
    custom_commands: Vec<String>,
    pub internal_calls: Vec<String>,
    pub retry_calls: Vec<(String, String, Option<String>)>,
    pub augmented_calls: Vec<(String, String)>,
}

impl CommandHandler for MockCommandHandler {
    fn handle(&mut self, _line: &str, _ui: &mut TuiApp) {
        // モック実装
    }

    fn get_custom_commands(&self) -> Vec<String> {
        self.custom_commands.clone()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn handle_internal_followup(&mut self, content: &str, _ui: &mut TuiApp) {
        self.internal_calls.push(content.to_string());
    }

    fn handle_retry_turn(
        &mut self,
        display: &str,
        content: &str,
        directive_id: Option<String>,
        _ui: &mut TuiApp,
    ) {
        self.retry_calls
            .push((display.to_string(), content.to_string(), directive_id));
    }

    fn handle_augmented_user_prompt(&mut self, raw: &str, effective: &str, _ui: &mut TuiApp) {
        self.augmented_calls
            .push((raw.to_string(), effective.to_string()));
    }
}

fn mock_app() -> TuiApp {
    let handler = MockCommandHandler {
        custom_commands: Vec::new(),
        internal_calls: Vec::new(),
        retry_calls: Vec::new(),
        augmented_calls: Vec::new(),
    };
    let app = TuiApp::new("Test App", None, "dark").unwrap();
    app.with_handler(Box::new(handler))
}

type MockCalls = (
    Vec<String>,
    Vec<(String, String, Option<String>)>,
    Vec<(String, String)>,
);

fn mock_calls(app: &TuiApp) -> MockCalls {
    let mock = app
        .handler
        .as_ref()
        .expect("handler")
        .as_any()
        .downcast_ref::<MockCommandHandler>()
        .expect("mock handler");
    (
        mock.internal_calls.clone(),
        mock.retry_calls.clone(),
        mock.augmented_calls.clone(),
    )
}

#[test]
fn test_get_all_commands_with_custom_commands() {
    // モックのカスタムコマンドを準備
    let custom_commands = vec!["/test".to_string(), "/example".to_string()];

    // モックのコマンドハンドラを作成
    let mock_handler = Box::new(MockCommandHandler {
        custom_commands: custom_commands.clone(),
        internal_calls: Vec::new(),
        retry_calls: Vec::new(),
        augmented_calls: Vec::new(),
    });

    // TuiAppを作成し、モックハンドラを設定
    let mut app = TuiApp::new("Test App", None, "dark").unwrap();
    app = app.with_handler(mock_handler);

    // get_all_commandsを呼び出し、結果を検証
    let all_commands = app.get_all_commands();

    // 組み込みのコマンドが含まれていることを確認
    assert!(all_commands.contains(&"/help".to_string()));
    assert!(all_commands.contains(&"/map".to_string()));
    assert!(all_commands.contains(&"/tools".to_string()));
    assert!(all_commands.contains(&"/clear".to_string()));
    assert!(all_commands.contains(&"/open".to_string()));
    assert!(all_commands.contains(&"/quit".to_string()));
    assert!(all_commands.contains(&"/theme".to_string()));
    assert!(all_commands.contains(&"/session".to_string()));
    assert!(all_commands.contains(&"/rebuild-repomap".to_string()));
    assert!(all_commands.contains(&"/tokens".to_string()));
    assert!(all_commands.contains(&"/git-worktree".to_string()));

    assert!(all_commands.contains(&"/cancel".to_string()));
    assert!(all_commands.contains(&"/jobs".to_string()));
    assert!(all_commands.contains(&"/compact".to_string()));

    // カスタムコマンドが含まれていることを確認
    for custom_cmd in &custom_commands {
        assert!(all_commands.contains(custom_cmd));
    }
}

#[test]
fn test_plan_list_completed_hides_on_next_dispatch() {
    let mut app = TuiApp::new("Test App", None, "dark").unwrap();

    let plan = vec![super::PlanItem {
        id: "step-1".to_string(),
        parent_id: None,
        content: "Done".to_string(),
        status: "completed".to_string(),
    }];

    app.apply_plan_list_update(plan.clone());
    assert_eq!(app.plan_list, plan);
    assert!(app.hide_plan_on_next_instruction);

    app.dispatch("next");
    assert!(app.plan_list.is_empty());
    assert!(!app.hide_plan_on_next_instruction);
}

#[test]
fn test_textarea_crossterm_input_compatibility() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
    use ratatui_textarea::{CursorMove, Input, Key, TextArea};

    let key = |code, modifiers| KeyEvent::new(code, modifiers);
    let mut textarea = TextArea::default();

    assert!(textarea.input(key(KeyCode::Char('a'), KeyModifiers::NONE)));
    assert!(textarea.input(key(KeyCode::Char('b'), KeyModifiers::NONE)));
    assert!(textarea.input(key(KeyCode::Backspace, KeyModifiers::NONE)));
    assert_eq!(textarea.lines(), ["a"]);

    textarea.move_cursor(CursorMove::Head);
    textarea.input(key(KeyCode::Delete, KeyModifiers::NONE));
    assert_eq!(textarea.lines(), [""]);

    assert!(textarea.input(key(KeyCode::Enter, KeyModifiers::NONE)));
    assert!(textarea.input(key(KeyCode::Char('x'), KeyModifiers::NONE)));
    assert_eq!(textarea.lines(), ["", "x"]);

    let mut shortcut_textarea = TextArea::from(vec!["abc".to_string()]);
    shortcut_textarea.move_cursor(CursorMove::End);
    assert_eq!(shortcut_textarea.cursor(), (0, 3));
    shortcut_textarea.input(key(KeyCode::Char('a'), KeyModifiers::CONTROL));
    assert_eq!(shortcut_textarea.cursor(), (0, 0));

    let input = Input::from(Event::Key(key(KeyCode::Char('q'), KeyModifiers::SHIFT)));
    assert_eq!(
        input,
        Input {
            key: Key::Char('q'),
            ctrl: false,
            alt: false,
            shift: true,
        }
    );

    let mouse = MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    };
    assert!(matches!(
        Input::from(Event::Mouse(mouse)),
        Input {
            key: Key::MouseScrollUp,
            ..
        }
    ));

    // Non-key events remain safe to pass through the shared input adapter.
    assert_eq!(
        Input::from(Event::Paste("clipboard".into())),
        Input::default()
    );
    let _ = Event::Resize(80, 24);
}

#[test]
fn test_plan_list_in_progress_does_not_hide() {
    let mut app = TuiApp::new("Test App", None, "dark").unwrap();

    let plan = vec![super::PlanItem {
        id: "step-1".to_string(),
        parent_id: None,
        content: "Working".to_string(),
        status: "in_progress".to_string(),
    }];

    app.apply_plan_list_update(plan.clone());
    assert_eq!(app.plan_list, plan);
    assert!(!app.hide_plan_on_next_instruction);
}

#[test]
fn test_internal_followup_bypasses_string_routing() {
    let mut app = mock_app();
    let synthetic = "Please analyze and fix the following lint issues";
    app.dispatch_internal_followup(synthetic);
    let (internal, retry, augmented) = mock_calls(&app);
    assert_eq!(internal, vec![synthetic.to_string()]);
    assert!(retry.is_empty());
    assert!(augmented.is_empty());
    // Synthetic turns never become the retry source.
    assert_eq!(app.last_user_input, None);
}

#[test]
fn test_retry_after_compact_inherits_matching_directive_only() {
    let mut app = mock_app();
    app.last_user_input = Some("real instruction".to_string());
    app.last_observed_raw_input = Some("real instruction".to_string());
    app.last_observed_effective_input = Some("real instruction".to_string());
    app.last_observed_seq = 3;
    app.last_observed_directive_id = Some("dir-1".to_string());
    app.dispatch_retry_after_compact("real instruction");
    let (_, retry, _) = mock_calls(&app);
    assert_eq!(
        retry,
        vec![(
            "real instruction".to_string(),
            "real instruction".to_string(),
            Some("dir-1".to_string())
        )]
    );

    // Stale id with mismatched input falls back to none(), never a fresh
    // observation.
    let mut app = mock_app();
    app.last_user_input = Some("new input".to_string());
    app.last_observed_raw_input = Some("old input".to_string());
    app.last_observed_effective_input = Some("old expanded".to_string());
    app.last_observed_seq = 5;
    app.last_observed_directive_id = Some("dir-old".to_string());
    app.dispatch_retry_after_compact("new input");
    let (_, retry, _) = mock_calls(&app);
    assert_eq!(
        retry,
        vec![("new input".to_string(), "new input".to_string(), None)]
    );
}

#[test]
fn test_retry_after_compact_replays_custom_expansion_not_raw_slash() {
    // A custom command observes raw="/cmd arg" but hands the expanded body
    // to the agent. Compact retry must replay the expansion (with the
    // original directive id), not the literal slash string.
    let mut app = mock_app();
    app.last_user_input = Some("/fix-cache arg".to_string());
    app.last_observed_raw_input = Some("/fix-cache arg".to_string());
    app.last_observed_effective_input = Some("Expanded: fix the cache with arg".to_string());
    app.last_observed_seq = 7;
    app.last_observed_directive_id = Some("dir-9".to_string());
    app.dispatch_retry_after_compact("/fix-cache arg");
    let (_, retry, _) = mock_calls(&app);
    assert_eq!(
        retry,
        vec![(
            "/fix-cache arg".to_string(),
            "Expanded: fix the cache with arg".to_string(),
            Some("dir-9".to_string())
        )]
    );
}

#[test]
fn test_augmented_prompt_keeps_raw_and_effective_separate() {
    let mut app = mock_app();
    let raw = "continue with the fix";
    let effective = "[SYSTEM NOTE] reverted\n\ncontinue with the fix";
    app.dispatch_augmented_user_prompt(raw, effective);
    let (_, _, augmented) = mock_calls(&app);
    assert_eq!(augmented, vec![(raw.to_string(), effective.to_string())]);
}

#[test]
fn test_note_directive_observed_pairs_with_tracked_raw() {
    let mut app = mock_app();
    // No tracked raw: stray ids are ignored.
    app.note_directive_observed(1, "dir-stray");
    assert_eq!(app.last_observed_directive_id, None);
    app.last_observed_raw_input = Some("typed".to_string());
    app.last_observed_seq = 4;
    app.note_directive_observed(4, "dir-1");
    assert_eq!(app.last_observed_directive_id.as_deref(), Some("dir-1"));
}

#[test]
fn test_note_directive_observed_rejects_stale_sequence() {
    let mut app = mock_app();
    // A newer turn (seq 6) is tracked; a late id from the older turn
    // (seq 5, e.g. cancelled job recording late) must not attach to it.
    app.last_observed_raw_input = Some("new typed".to_string());
    app.last_observed_seq = 6;
    app.note_directive_observed(5, "dir-stale");
    assert_eq!(app.last_observed_directive_id, None);
    app.note_directive_observed(6, "dir-current");
    assert_eq!(
        app.last_observed_directive_id.as_deref(),
        Some("dir-current")
    );
}
