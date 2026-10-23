//! Exercise the same diagnostic call sites and input handlers as the terminal loop.
use super::{
    event_handlers::handle_normal_mode_key, event_loop::log_terminal_event, state::TuiApp,
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use ratatui_textarea::{CursorMove, TextArea};
use std::io::{Read, Seek, SeekFrom};

fn capture_debug(body: impl FnOnce()) -> String {
    let mut log = tempfile::tempfile().unwrap();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(log.try_clone().unwrap())
        .finish();
    tracing::subscriber::with_default(subscriber, body);
    log.seek(SeekFrom::Start(0)).unwrap();
    let mut contents = String::new();
    log.read_to_string(&mut contents).unwrap();
    contents
}

#[test]
fn input_logging_submission_keeps_multiline_text_without_logging_it() {
    let secret = "SUBMIT_SECRET_47d9";
    let input = format!("日本語 {secret}\nsecond line 🔑");
    let mut app = TuiApp::new_for_test("logging", None, "default");
    app.textarea = TextArea::from(input.split('\n').map(str::to_owned).collect::<Vec<_>>());
    // Already present: exercise submission without writing project input history.
    app.input_history = vec![input.clone()];
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let logs = capture_debug(|| {
        handle_normal_mode_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut terminal,
        )
        .unwrap();
    });
    assert_eq!(app.pending_instructions.front(), Some(&input));
    assert_eq!(app.textarea.lines(), [""]);
    assert!(!logs.is_empty(), "must capture actual debug events");
    assert!(!logs.contains(secret), "submission leaked input: {logs}");
}

#[test]
fn input_logging_completion_keeps_candidates_without_logging_text_or_paths() {
    let dir = tempfile::tempdir().unwrap();
    let secret = "COMPLETE_SECRET_83a2";
    let filename = format!("{secret}x.rs");
    std::fs::write(dir.path().join(&filename), "").unwrap();
    let mut app = TuiApp::new_for_test("logging", None, "default");
    app.cfg = Some(crate::config::AppConfig {
        project_root: dir.path().to_path_buf(),
        ..Default::default()
    });
    let input = format!("日本語 @{secret}");
    app.textarea = TextArea::from(vec!["keep first".into(), input.clone()]);
    app.textarea.move_cursor(CursorMove::Bottom);
    app.textarea.move_cursor(CursorMove::End);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let logs = capture_debug(|| {
        handle_normal_mode_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            &mut terminal,
        )
        .unwrap();
    });
    assert_eq!(app.textarea.lines(), ["keep first", &format!("{input}x")]);
    assert!(app.completion_active);
    assert_eq!(app.completion_candidates, [filename]);
    assert!(!logs.is_empty(), "must capture actual debug events");
    assert!(!logs.contains(secret), "completion leaked input: {logs}");
}

#[test]
fn input_logging_paste_keeps_text_without_logging_event_payload() {
    let secret = "PASTE_SECRET_e3b7";
    let input = format!("日本語 {secret}\nnext line 🔑");
    let mut app = TuiApp::new_for_test("logging", None, "default");
    let event = Event::Paste(input.clone());
    let logs = capture_debug(|| {
        // The terminal loop records the event before passing its payload to the handler.
        log_terminal_event(&event);
        if let Event::Paste(text) = &event {
            app.handle_paste(text);
        }
    });
    assert_eq!(app.textarea.lines().join("\n"), input);
    assert!(app.pending_instructions.is_empty());
    assert!(!logs.is_empty(), "must capture actual debug events");
    assert!(!logs.contains(secret), "paste event leaked input: {logs}");
}

#[test]
fn input_history_failure_keeps_submitted_input_and_warns_without_content() {
    let dir = tempfile::tempdir().expect("fixture");
    std::fs::write(dir.path().join(".doge"), "blocked directory").expect("block");
    let mut app = TuiApp::new_for_test("history", None, "default");
    app.cfg = Some(crate::config::AppConfig {
        project_root: dir.path().into(),
        ..Default::default()
    });
    let text = "HISTORY_PRIVATE_INPUT";
    app.textarea.insert_str(text);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    handle_normal_mode_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut terminal,
    )
    .expect("enter");
    assert_eq!(
        app.pending_instructions.front().map(String::as_str),
        Some(text)
    );
    assert_eq!(app.input_history, [text]);
    let messages = format!("{:?}", app.log);
    assert!(messages.contains("history][warning"));
    assert!(!messages.contains(text));
    assert_eq!(
        std::fs::read_to_string(dir.path().join(".doge")).expect("unchanged"),
        "blocked directory"
    );
}
