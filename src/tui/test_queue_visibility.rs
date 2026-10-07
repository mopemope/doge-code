use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use ratatui_textarea::TextArea;
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    jobs::{JobKind, JobManager, JobRunOutcome, JobScope, JobSpec, WorkspaceAccess},
    tui::{
        commands::core::CommandHandler,
        event_handlers::handle_normal_mode_key,
        state::{LogEntry, Status, TuiApp},
    },
};

struct Recorder {
    jobs: JobManager,
    calls: Arc<Mutex<Vec<String>>>,
    reject_once: bool,
}
impl CommandHandler for Recorder {
    fn handle(&mut self, line: &str, _: &mut TuiApp) {
        self.calls.lock().unwrap().push(line.into());
        if line == "/cancel" {
            self.jobs.cancel_foreground();
        }
    }
    fn foreground_busy(&self) -> bool {
        self.jobs.foreground_id().is_some()
    }
    fn handle_queued(&mut self, line: &str, ui: &mut TuiApp) -> bool {
        if self.reject_once {
            self.reject_once = false;
            return false;
        }
        self.handle(line, ui);
        true
    }
    fn get_custom_commands(&self) -> Vec<String> {
        vec![]
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
fn logs(ui: &TuiApp) -> String {
    ui.log
        .iter()
        .map(|entry| match entry {
            LogEntry::Plain(s) | LogEntry::Markdown(s) => s.as_str(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn enter(ui: &mut TuiApp, terminal: &mut Terminal<TestBackend>, line: &str) {
    ui.textarea = TextArea::from(line.split('\n'));
    // Keep this isolated test from persisting input history in the checkout.
    ui.input_history.push(line.into());
    handle_normal_mode_key(
        ui,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        terminal,
    )
    .unwrap();
}
fn screen(ui: &mut TuiApp, terminal: &mut Terminal<TestBackend>) -> String {
    terminal.draw(|frame| ui.view(frame, None)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect()
}

#[tokio::test]
async fn queued_submission_visibility_survives_cancel_cleanup_and_controls() {
    let jobs = JobManager::new();
    let (release, held) = tokio::sync::oneshot::channel();
    let (started, running) = tokio::sync::oneshot::channel();
    let id = jobs
        .spawn(
            JobSpec::new(
                JobKind::AgentTurn,
                JobScope::Foreground,
                WorkspaceAccess::None,
                "held cleanup",
            ),
            move |ctx| async move {
                started.send(()).unwrap();
                let _ = held.await;
                if ctx.cancellation.is_cancelled() {
                    JobRunOutcome::Cancelled
                } else {
                    JobRunOutcome::Completed
                }
            },
        )
        .unwrap();
    running.await.unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let mut ui = TuiApp::new_for_test("queue", None, "dark");
    ui.handler = Some(Box::new(Recorder {
        jobs: jobs.clone(),
        calls: calls.clone(),
        reject_once: false,
    }));
    ui.status = Status::Ready; // Ownership, rather than cosmetic status, keeps the queue held.
    ui.detailed_status =
        Some("A long detailed status that consumes the entire narrow header".into());
    let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
    enter(&mut ui, &mut terminal, "調査してください\nkeep details");
    assert!(logs(&ui).contains("[Queued: 1 waiting]"));
    assert!(logs(&ui).contains("調査してください"));
    assert!(screen(&mut ui, &mut terminal).starts_with("[1 queued]"));
    enter(&mut ui, &mut terminal, "next prompt");
    assert!(logs(&ui).contains("[Queued: 2 waiting]"));
    assert!(screen(&mut ui, &mut terminal).starts_with("[2 queued]"));
    enter(&mut ui, &mut terminal, "/jobs");
    enter(&mut ui, &mut terminal, "/cancel");
    assert_eq!(
        jobs.get_snapshot(id).unwrap().status,
        crate::jobs::JobStatus::Cancelling
    );
    ui.dispatch_pending_instruction();
    assert_eq!(
        ui.pending_instructions,
        ["調査してください\nkeep details", "next prompt"]
    );
    assert!(screen(&mut ui, &mut terminal).starts_with("[2 queued]"));
    assert_eq!(*calls.lock().unwrap(), ["/jobs", "/cancel"]);
    assert_eq!(logs(&ui).matches("[Queued:").count(), 2);
    release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while jobs.foreground_id().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    ui.dispatch_pending_instruction();
    assert!(screen(&mut ui, &mut terminal).starts_with("[1 queued]"));
    ui.dispatch_pending_instruction();
    assert!(!screen(&mut ui, &mut terminal).starts_with("[1 queued]"));
    assert!(ui.pending_instructions.is_empty());
    assert_eq!(
        *calls.lock().unwrap(),
        [
            "/jobs",
            "/cancel",
            "調査してください\nkeep details",
            "next prompt"
        ]
    );
}

#[test]
fn queue_visibility_preserves_count_on_rejected_dispatch() {
    let mut ui = TuiApp::new_for_test("queue", None, "light");
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    ui.handler = Some(Box::new(Recorder {
        jobs: JobManager::new(),
        calls: calls.clone(),
        reject_once: true,
    }));
    ui.pending_instructions.extend(["one".into(), "two".into()]);
    ui.dispatch_pending_instruction();
    assert!(screen(&mut ui, &mut terminal).starts_with("[2 queued]"));
    assert!(calls.lock().unwrap().is_empty());
    ui.dispatch_pending_instruction();
    assert!(screen(&mut ui, &mut terminal).starts_with("[1 queued]"));
    ui.dispatch_pending_instruction();
    assert!(ui.pending_instructions.is_empty());
    assert_eq!(*calls.lock().unwrap(), ["one", "two"]);
}

#[test]
fn queued_preview_is_bounded_but_original_multiline_input_is_preserved() {
    for content in [
        "👩‍💻e\u{301}".repeat(500),
        "a".repeat(500),
        format!("e{}", "\u{301}".repeat(500)),
    ] {
        let mut ui = TuiApp::new_for_test("queue", None, "dark");
        ui.status = Status::Thinking;
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let input = format!("\n\x1b[31m{content}\nsecond line");
        enter(&mut ui, &mut terminal, &input);
        assert_eq!(ui.pending_instructions, [input]);
        let text = logs(&ui);
        let preview = text
            .strip_prefix("[Queued: 1 waiting] ")
            .unwrap()
            .strip_suffix("… (+2 lines)")
            .unwrap();
        assert!(preview.len() <= 240);
        assert!(preview.graphemes(true).count() <= 60);
        assert!(!preview.chars().any(char::is_control));
        assert!(!text.contains("second line"));
    }
}

#[test]
fn blank_enter_does_not_inflate_queue_or_reset_draft() {
    let mut ui = TuiApp::new_for_test("queue", None, "dark");
    ui.status = Status::Thinking;
    ui.pending_instructions.push_back("kept prompt".into());
    let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
    let before = ui.log.len();
    let history = ui.input_history.clone();
    for input in ["", "   ", "\n\t\n"] {
        ui.textarea = TextArea::from(input.split('\n'));
        handle_normal_mode_key(
            &mut ui,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut terminal,
        )
        .unwrap();
        assert_eq!(ui.pending_instructions, ["kept prompt"]);
        assert_eq!(ui.log.len(), before);
        assert_eq!(ui.textarea.lines().join("\n"), input);
        assert_eq!(ui.input_history, history);
    }
}
