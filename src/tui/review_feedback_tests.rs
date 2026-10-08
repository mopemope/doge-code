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
    fn validate_review_feedback_source(
        &self,
        source: &crate::diff_review::DiffReviewPayload,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.fail && source.review_id.is_some() && source.session_id.is_some(),
            "Source expired"
        );
        Ok(())
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

fn next_review(ui: &mut TuiApp, id: &str) {
    let mut source = payload();
    source.review_id = Some(id.into());
    let state = DiffReviewState::from_payload(source);
    ui.feedback_review_arrived(&state);
    ui.diff_review = Some(state);
}
#[test]
fn feedback_history_archives_unsent_and_new_comments_bind_only_current_review() {
    let (mut ui, calls) = ui(false);
    save(&mut ui, "以前の未送信コメント");
    let previous = ui.review_feedback.as_ref().unwrap().batch.clone();
    next_review(&mut ui, "repair-one");
    ui.start_fresh_feedback();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(ui.feedback_history.len(), 1);
    assert_eq!(
        ui.feedback_history.get(0).unwrap().batch.comments,
        previous.comments
    );
    assert!(
        ui.review_feedback
            .as_ref()
            .unwrap()
            .batch
            .comments
            .is_empty()
    );
    assert_ne!(ui.review_feedback.as_ref().unwrap().batch.id, previous.id);
    save(&mut ui, "次の修正コメント");
    assert_eq!(
        ui.review_feedback.as_ref().unwrap().batch.comments[0]
            .anchor
            .review_id,
        "repair-one"
    );
    assert_eq!(
        ui.feedback_history.get(0).unwrap().batch.comments[0]
            .anchor
            .review_id,
        "review"
    );
    let current = ui.diff_review.clone();
    ui.open_feedback_history();
    ui.handle_paste("history paste must not type");
    for code in [
        KeyCode::Char('v'),
        KeyCode::Char('c'),
        KeyCode::Char('s'),
        KeyCode::Enter,
        KeyCode::Char('r'),
        KeyCode::Char('n'),
    ] {
        ui.handle_feedback_key(key(code));
    }
    assert_eq!(ui.diff_review, current);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(ui.textarea.lines(), ["keep ordinary 日本語"]);
    ui.handle_feedback_key(key(KeyCode::Esc));
    assert!(ui.feedback_history_view.is_none());
    assert_eq!(ui.diff_review, current);
}
#[test]
fn feedback_history_full_keeps_active_text_and_confirmed_deletion_only() {
    let (mut ui, calls) = ui(false);
    save(&mut ui, "original 日本語");
    for i in 0..8 {
        next_review(&mut ui, &format!("repair-{i}"));
        ui.start_fresh_feedback();
        save(&mut ui, &format!("unsent-{i}"));
    }
    assert_eq!(ui.feedback_history.len(), 8);
    let active = ui.review_feedback.as_ref().unwrap().batch.clone();
    next_review(&mut ui, "ninth");
    ui.start_fresh_feedback();
    assert!(
        ui.feedback_error
            .as_ref()
            .unwrap()
            .contains("history is full")
    );
    assert_eq!(
        ui.review_feedback.as_ref().unwrap().batch.comments,
        active.comments
    );
    ui.open_feedback_history();
    ui.handle_feedback_key(KeyEvent::new_with_kind(
        KeyCode::Char('d'),
        KeyModifiers::NONE,
        KeyEventKind::Repeat,
    ));
    assert!(!ui.feedback_history_view.as_ref().unwrap().confirm_delete);
    ui.handle_feedback_key(key(KeyCode::Char('d')));
    ui.handle_feedback_key(KeyEvent::new_with_kind(
        KeyCode::Enter,
        KeyModifiers::NONE,
        KeyEventKind::Repeat,
    ));
    assert_eq!(ui.feedback_history.len(), 8);
    ui.handle_feedback_key(key(KeyCode::Esc));
    assert_eq!(ui.feedback_history.len(), 8);
    ui.handle_feedback_key(key(KeyCode::Char('d')));
    ui.handle_feedback_key(key(KeyCode::Enter));
    assert_eq!(ui.feedback_history.len(), 7);
    ui.handle_feedback_key(key(KeyCode::Esc));
    ui.start_fresh_feedback();
    assert!(ui.feedback_error.is_none());
    assert_eq!(ui.feedback_history.len(), 8);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[test]
fn feedback_history_missing_source_and_old_scoped_results_cannot_change_new_batch() {
    let (mut ui, _) = ui(false);
    save(&mut ui, "保存コメント");
    ui.review_feedback.as_mut().unwrap().job_id = Some(crate::jobs::JobId(1));
    let old = ui.review_feedback.as_ref().unwrap().batch.clone();
    next_review(&mut ui, "repair");
    ui.start_fresh_feedback();
    let current = ui.review_feedback.as_ref().unwrap().batch.id.clone();
    ui.latest_agent_job_id = Some(crate::jobs::JobId(2));
    let outcome = crate::features::review_feedback::FeedbackOutcome {
        batch_id: old.id,
        revision: old.revision,
        job_id: crate::jobs::JobId(1),
        outcome: "Partial: budget".into(),
    };
    let message = format!(
        "::feedback_outcome:{}",
        serde_json::to_string(&outcome).unwrap()
    );
    ui.archived_job_message(crate::jobs::JobId(2), &message);
    assert!(ui.feedback_history.get(0).unwrap().outcome.is_none());
    ui.archived_job_message(crate::jobs::JobId(1), &message);
    assert_eq!(
        ui.feedback_history.get(0).unwrap().outcome.as_deref(),
        Some("Partial: budget")
    );
    assert_eq!(ui.review_feedback.as_ref().unwrap().batch.id, current);
    assert!(ui.review_feedback.as_ref().unwrap().outcome.is_none());
    ui.diff_review = None;
    ui.start_fresh_feedback();
    assert_eq!(ui.review_feedback.as_ref().unwrap().batch.id, current);
    assert!(
        ui.feedback_error
            .as_ref()
            .unwrap()
            .contains("No current review")
    );
}
#[test]
fn feedback_history_mouse_resize_keeps_ordinary_scroll_and_source_view() {
    let (mut ui, _) = ui(false);
    save(&mut ui, "以前のコメント");
    next_review(&mut ui, "repair");
    ui.start_fresh_feedback();
    let before = ui.diff_review.clone();
    let offset = ui.scroll_state.offset;
    ui.open_feedback_history();
    assert!(ui.scroll_feedback_history(3));
    assert_eq!(ui.feedback_history_view.as_ref().unwrap().scroll, 3);
    assert_eq!(ui.scroll_state.offset, offset);
    for (w, h) in [(120, 32), (45, 15), (8, 4)] {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| ui.view(f, None)).unwrap();
    }
    ui.handle_feedback_key(key(KeyCode::Esc));
    assert_eq!(ui.diff_review, before);
}
