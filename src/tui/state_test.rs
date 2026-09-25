#[cfg(test)]
use crate::TuiApp;
use crate::tui::commands::core::CommandHandler;
use std::any::Any;

struct MockCommandHandler {
    custom_commands: Vec<String>,
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
}

#[test]
fn test_get_all_commands_with_custom_commands() {
    // モックのカスタムコマンドを準備
    let custom_commands = vec!["/test".to_string(), "/example".to_string()];

    // モックのコマンドハンドラを作成
    let mock_handler = Box::new(MockCommandHandler {
        custom_commands: custom_commands.clone(),
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
