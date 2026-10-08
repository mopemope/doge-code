use crate::features::review_feedback::FeedbackBatch;
use crate::tui::state::DiffReviewFocus;
use crate::tui::{commands::CommandHandler, diff_review::DiffReviewState, state::TuiApp};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui_textarea::TextArea;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn payload() -> crate::diff_review::DiffReviewPayload {
    crate::diff_review::DiffReviewPayload {session_id:Some("session".into()),review_id:Some("review".into()),reject_reason:None,diff:"diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n@@ -8 +8 @@\n-before\n+after\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -4 +4 @@\n-古い\n+新しい\n".into(),files:vec!["a.rs".into(),"b.rs".into()],evidence:vec![],evidence_warnings:vec![]}
}
struct Handler {
    calls: Arc<AtomicUsize>,
    fail: bool,
}
impl CommandHandler for Handler {
    fn handle(&mut self, _: &str, _: &mut TuiApp) {
        panic!("feedback must never enter ordinary dispatch");
    }
    fn get_custom_commands(&self) -> Vec<String> {
        vec![]
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn validate_review_feedback(&self, batch: &FeedbackBatch) -> anyhow::Result<()> {
        batch.validate_structure()
    }
    fn submit_review_feedback(
        &mut self,
        _: FeedbackBatch,
        _: &mut TuiApp,
    ) -> anyhow::Result<crate::jobs::JobId> {
        if self.fail {
            anyhow::bail!("spawn failed");
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(crate::jobs::JobId(42))
    }
}
fn ui(fail: bool) -> (TuiApp, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut ui = TuiApp::new_for_test("feedback", None, "default");
    ui.diff_review = Some(DiffReviewState::from_payload(payload()));
    ui.diff_review_focus = DiffReviewFocus::Review;
    ui.textarea = TextArea::from(vec!["keep ordinary 日本語".to_string()]);
    ui.handler = Some(Box::new(Handler {
        calls: calls.clone(),
        fail,
    }));
    (ui, calls)
}
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn save(ui: &mut TuiApp, text: &str) {
    ui.start_comment();
    ui.handle_paste(text);
    assert!(ui.handle_feedback_key(key(KeyCode::Enter)));
}
#[test]
fn feedback_modal_three_japanese_comments_across_files_and_one_explicit_batch() {
    let (mut ui, calls) = ui(false);
    save(&mut ui, "境界条件を直してください");
    ui.move_hunk(1);
    save(&mut ui, "命名を整理してください");
    ui.diff_review.as_mut().unwrap().selected = 1;
    save(&mut ui, "日本語の表示を保ってください");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(ui.review_feedback.as_ref().unwrap().batch.comments.len(), 3);
    assert_eq!(ui.textarea.lines(), ["keep ordinary 日本語"]);
    ui.confirm_feedback();
    ui.handle_feedback_key(key(KeyCode::Esc));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    ui.confirm_feedback();
    ui.handle_feedback_key(key(KeyCode::Enter));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    ui.confirm_feedback();
    ui.handle_feedback_key(key(KeyCode::Enter));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(ui.review_feedback.as_ref().unwrap().batch.comments.len(), 3);
}
#[test]
fn feedback_edit_cancel_delete_paste_modified_keys_and_repeats_preserve_input() {
    let (mut ui, calls) = ui(false);
    save(&mut ui, "元コメント");
    ui.start_comment();
    ui.handle_paste("\r\n変更\u{1b}");
    ui.handle_feedback_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
    ui.handle_feedback_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    ui.handle_feedback_key(key(KeyCode::Esc));
    assert_eq!(
        ui.review_feedback.as_ref().unwrap().batch.comments[0].text,
        "元コメント"
    );
    ui.start_comment();
    ui.comment_editor.as_mut().unwrap().textarea = TextArea::from(vec!["編集済み".to_string()]);
    ui.handle_feedback_key(KeyEvent::new_with_kind(
        KeyCode::Enter,
        KeyModifiers::NONE,
        KeyEventKind::Repeat,
    ));
    assert!(ui.comment_editor.is_some());
    ui.handle_feedback_key(key(KeyCode::Enter));
    assert_eq!(
        ui.review_feedback.as_ref().unwrap().batch.comments[0].text,
        "編集済み"
    );
    ui.delete_comment();
    assert!(
        ui.review_feedback
            .as_ref()
            .unwrap()
            .batch
            .comments
            .is_empty()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(ui.textarea.lines(), ["keep ordinary 日本語"]);
}
#[test]
fn feedback_new_review_and_spawn_failure_keep_draft_and_show_reason() {
    let (mut ui, calls) = ui(true);
    save(&mut ui, "保持してください");
    ui.confirm_feedback();
    ui.submit_feedback();
    assert!(ui.feedback_error.as_ref().unwrap().contains("spawn failed"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let mut new = payload();
    new.review_id = Some("new-review".into());
    let new = DiffReviewState::from_payload(new);
    ui.feedback_review_arrived(&new);
    ui.diff_review = Some(new);
    ui.feedback_confirmation = false;
    ui.start_comment();
    assert!(ui.comment_editor.is_none());
    ui.confirm_feedback();
    assert!(
        ui.feedback_error
            .as_ref()
            .unwrap()
            .contains("Another review")
    );
    assert_eq!(
        ui.review_feedback.as_ref().unwrap().batch.comments[0].text,
        "保持してください"
    );
    ui.handle_feedback_key(key(KeyCode::Char('d')));
    assert!(ui.review_feedback.is_none());
}
#[test]
fn feedback_projection_wrap_resize_hunk_coordinates_and_selected_file_visible() {
    let (mut ui, _) = ui(false);
    save(&mut ui, "日本語の長いコメントを表示します\n二行目");
    ui.diff_viewport_width.set(8);
    let narrow = ui.diff_projection();
    ui.diff_viewport_width.set(80);
    let wide = ui.diff_projection();
    assert!(narrow.len() > wide.len());
    assert!(wide.iter().any(|r| r.content.contains("   1      -old")));
    for (width, height) in [(120, 35), (45, 15), (10, 5), (1, 1)] {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        ui.confirm_feedback();
        terminal.draw(|f| ui.view(f, None)).unwrap();
        ui.feedback_confirmation = false;
        terminal.draw(|f| ui.view(f, None)).unwrap();
    }
    assert_eq!(
        ui.review_feedback.as_ref().unwrap().batch.comments[0]
            .anchor
            .hunk
            .old
            .start,
        1
    );
}

#[test]
fn feedback_large_hunk_projection_has_correct_linear_coordinates() {
    let (mut ui, _) = ui(false);
    let mut payload = payload();
    payload.files = vec!["large.txt".into()];
    payload.diff = format!(
        "diff --git a/large.txt b/large.txt\n--- /dev/null\n+++ b/large.txt\n@@ -0,0 +1,10000 @@\n{}",
        (1..=10000).map(|n| format!("+行{n}\n")).collect::<String>()
    );
    ui.diff_review = Some(DiffReviewState::from_payload(payload));
    let projection = ui.diff_projection();
    assert_eq!(projection.len(), 10004);
    assert!(
        projection
            .last()
            .unwrap()
            .content
            .contains("10000 +行10000")
    );
}
