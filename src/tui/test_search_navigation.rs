use super::event_handlers::{handle_file_search_key, handle_history_search_key};
use super::state::{FileSearchState, InputMode, TuiApp};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn screen(app: &mut TuiApp, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| app.view(f, None)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .chunks(width as usize)
        .map(|row| {
            let mut text = String::new();
            let mut x = 0;
            while x < row.len() {
                let symbol = row[x].symbol();
                text.push_str(symbol);
                x += unicode_width::UnicodeWidthStr::width(symbol).max(1);
            }
            text
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn history_selection_stays_visible_after_navigation_and_resize() {
    let mut app = TuiApp::new_for_test("search", None, "dark");
    app.input_history = (0..50).map(|i| format!("history-{i:02}")).collect();
    app.enter_history_search();
    for _ in 0..30 {
        handle_history_search_key(&mut app, key(KeyCode::Down)).unwrap();
    }
    for (width, height) in [(120, 32), (40, 12), (24, 8), (120, 32)] {
        assert!(screen(&mut app, width, height).contains("> history-19"));
    }
    handle_history_search_key(&mut app, key(KeyCode::Enter)).unwrap();
    assert_eq!(app.textarea.lines(), &["history-19"]);
    assert!(app.pending_instructions.is_empty());
}

#[test]
fn multiline_history_has_one_row_preview_and_restores_complete_input() {
    let mut app = TuiApp::new_for_test("search", None, "dark");
    let raw = format!(
        "\n調査🙂 e\u{301}\n{}",
        "keep all original rows\n".repeat(40)
    );
    app.input_history = vec!["older-entry".into(), raw.clone()];
    app.textarea.insert_str("existing draft\n".repeat(15));
    let draft = app.textarea.lines().to_vec();
    app.enter_history_search();
    let rendered = screen(&mut app, 40, 12);
    assert!(rendered.contains("> 調査🙂 e\u{301}"), "{rendered}");
    assert!(rendered.contains("older-entry"), "{rendered}");
    assert!(!rendered.contains("keep all original rows"));
    handle_history_search_key(&mut app, key(KeyCode::Esc)).unwrap();
    assert_eq!(app.textarea.lines(), draft);
    app.enter_history_search();
    handle_history_search_key(&mut app, key(KeyCode::Enter)).unwrap();
    assert_eq!(app.textarea.lines().join("\n"), raw);
    assert!(app.pending_instructions.is_empty());
}

#[test]
fn file_selection_stays_visible_and_filtering_and_wrap_remain_correct() {
    let mut app = TuiApp::new_for_test("search", None, "dark");
    let files: Vec<String> = (0..50).map(|i| format!("file-{i:02}.rs")).collect();
    app.input_mode = InputMode::FileSearch;
    app.file_search_state = Some(FileSearchState {
        query: String::new(),
        all_files: files.clone(),
        results: files,
        selected_index: 0,
        loading: false,
    });
    handle_file_search_key(&mut app, key(KeyCode::Up)).unwrap();
    assert!(screen(&mut app, 40, 12).contains("> file-49.rs"));
    handle_file_search_key(&mut app, key(KeyCode::Down)).unwrap();
    assert!(screen(&mut app, 40, 12).contains("> file-00.rs"));
    for _ in 0..30 {
        handle_file_search_key(&mut app, key(KeyCode::Down)).unwrap();
    }
    assert!(screen(&mut app, 120, 32).contains("> file-30.rs"));
    for c in "49".chars() {
        handle_file_search_key(&mut app, key(KeyCode::Char(c))).unwrap();
    }
    let state = app.file_search_state.as_ref().unwrap();
    assert_eq!(state.selected_index, 0);
    assert_eq!(state.results, ["file-49.rs"]);
    assert!(screen(&mut app, 24, 8).contains("> file-49.rs"));
    handle_file_search_key(&mut app, key(KeyCode::Char('x'))).unwrap();
    assert!(screen(&mut app, 40, 12).contains("No matches"));
    let state = app.file_search_state.as_mut().unwrap();
    state.loading = true;
    assert!(screen(&mut app, 40, 12).contains("Scanning files"));
    for (width, height) in [(1, 1), (2, 2), (8, 4)] {
        screen(&mut app, width, height);
    }
}

#[test]
fn file_relative_preview_opens_original_path_and_cancel_preserves_draft() {
    use super::commands::CommandHandler;
    use std::sync::{Arc, Mutex};
    struct Recorder(Arc<Mutex<Vec<String>>>);
    impl CommandHandler for Recorder {
        fn handle(&mut self, line: &str, _: &mut TuiApp) {
            self.0.lock().unwrap().push(line.into());
        }
        fn get_custom_commands(&self) -> Vec<String> {
            vec![]
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }
    let root = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new_for_test("search", None, "dark");
    app.cfg = Some(crate::config::AppConfig {
        project_root: root.path().to_path_buf(),
        ..Default::default()
    });
    let path = root
        .path()
        .join("nested/日本語 file.rs")
        .display()
        .to_string();
    app.textarea.insert_str("preserved\ndraft");
    let draft = app.textarea.lines().to_vec();
    let calls = Arc::new(Mutex::new(Vec::new()));
    app.handler = Some(Box::new(Recorder(calls.clone())));
    for cancel in [true, false] {
        app.input_mode = InputMode::FileSearch;
        app.file_search_state = Some(FileSearchState {
            query: String::new(),
            all_files: vec![path.clone()],
            results: vec![path.clone()],
            selected_index: 0,
            loading: false,
        });
        let rendered = screen(&mut app, 40, 12);
        assert!(rendered.contains("> nested/日本語 file.rs"), "{rendered}");
        assert!(!rendered.contains(root.path().to_str().unwrap()));
        handle_file_search_key(
            &mut app,
            key(if cancel { KeyCode::Esc } else { KeyCode::Enter }),
        )
        .unwrap();
        assert_eq!(app.textarea.lines(), draft);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.file_search_state.is_none());
    }
    assert_eq!(*calls.lock().unwrap(), [format!("/open {path}")]);
}

#[test]
fn long_history_query_tail_tracks_typing_backspace_resize_and_repeated_search() {
    let mut app = TuiApp::new_for_test("query", None, "dark");
    let query = format!("{}日本語e\u{301}🙂終端", "prefix-".repeat(12));
    let original = format!("{query}\ncomplete original history");
    app.input_history = vec![original.clone()];
    app.textarea.insert_str("preserved\ndraft");
    let draft = app.textarea.lines().to_vec();
    for _ in 0..3 {
        app.enter_history_search();
        for c in query.chars() {
            handle_history_search_key(&mut app, key(KeyCode::Char(c))).unwrap();
        }
        for (width, height) in [(40, 12), (24, 8), (120, 32), (40, 12)] {
            let rendered = screen(&mut app, width, height);
            assert!(rendered.contains("日本語e\u{301}🙂終端_"), "{rendered}");
            assert_eq!(app.history_search_state.as_ref().unwrap().query, query);
            assert_eq!(
                app.history_search_state.as_ref().unwrap().results,
                std::slice::from_ref(&original)
            );
        }
        handle_history_search_key(&mut app, key(KeyCode::Backspace)).unwrap();
        assert!(screen(&mut app, 40, 12).contains("日本語e\u{301}🙂終_"));
        handle_history_search_key(&mut app, key(KeyCode::Char('端'))).unwrap();
        handle_history_search_key(&mut app, key(KeyCode::Esc)).unwrap();
        assert_eq!(app.textarea.lines(), draft);
    }
    app.enter_history_search();
    for c in query.chars() {
        handle_history_search_key(&mut app, key(KeyCode::Char(c))).unwrap();
    }
    handle_history_search_key(&mut app, key(KeyCode::Enter)).unwrap();
    assert_eq!(app.textarea.lines().join("\n"), original);
    assert!(app.pending_instructions.is_empty());
}

#[test]
fn long_file_query_tail_keeps_complete_filter_and_cancelled_draft() {
    let mut app = TuiApp::new_for_test("query", None, "dark");
    app.textarea.insert_str("file draft");
    let query = format!("{}日本語e\u{301}終端", "long-path-".repeat(10));
    let path = format!("{query}.rs");
    app.input_mode = InputMode::FileSearch;
    app.file_search_state = Some(FileSearchState {
        query: String::new(),
        all_files: vec![path.clone()],
        results: vec![path.clone()],
        selected_index: 0,
        loading: false,
    });
    for c in query.chars() {
        handle_file_search_key(&mut app, key(KeyCode::Char(c))).unwrap();
    }
    for (width, height) in [(40, 12), (24, 8), (120, 32), (40, 12)] {
        let rendered = screen(&mut app, width, height);
        assert!(rendered.contains("日本語e\u{301}終端_"), "{rendered}");
        assert_eq!(app.file_search_state.as_ref().unwrap().query, query);
        assert_eq!(
            app.file_search_state.as_ref().unwrap().results,
            std::slice::from_ref(&path)
        );
    }
    let cancel = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);
    handle_file_search_key(&mut app, cancel).unwrap();
    assert_eq!(app.textarea.lines(), &["file draft"]);
}
