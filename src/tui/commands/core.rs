use crate::analysis::RepoMap;
use crate::hooks::HookManager;
use crate::jobs::JobManager;
use crate::llm::OpenAIClient;

use crate::llm::types::ChatMessage;
use crate::session::SessionManager;
use crate::tools::{FsTools, plan};
use crate::tui::commands::handlers::custom::CustomCommand;
use crate::tui::view::TuiApp;
use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

pub trait CommandHandler {
    fn handle(&mut self, line: &str, ui: &mut TuiApp);
    fn foreground_job_id(&self) -> Option<crate::jobs::JobId> {
        None
    }
    fn foreground_busy(&self) -> bool {
        false
    }
    fn handle_queued(&mut self, line: &str, ui: &mut TuiApp) -> bool {
        self.handle(line, ui);
        true
    }
    fn review_payload(&self, _id: &str) -> Option<crate::diff_review::DiffReviewPayload> {
        None
    }
    fn validate_review_feedback_source(
        &self,
        _source: &crate::diff_review::DiffReviewPayload,
    ) -> anyhow::Result<()> {
        anyhow::bail!("Feedback source validation is unavailable.")
    }
    fn validate_review_feedback(
        &self,
        _batch: &crate::features::review_feedback::FeedbackBatch,
    ) -> anyhow::Result<()> {
        anyhow::bail!("Feedback is unavailable in this handler.")
    }
    fn submit_review_feedback(
        &mut self,
        _batch: crate::features::review_feedback::FeedbackBatch,
        _ui: &mut TuiApp,
    ) -> anyhow::Result<crate::jobs::JobId> {
        anyhow::bail!("Feedback is unavailable in this handler.")
    }
    fn dismiss_review(&self, _id: &str) {}
    fn reject_review(&mut self, _id: &str, _ui: &mut TuiApp) {}

    /// Return true only after jobs released ownership and the checkpoint was flushed.
    fn prepare_exit(&mut self, _ui: &mut TuiApp) -> anyhow::Result<bool> {
        Ok(true)
    }
    fn get_custom_commands(&self) -> Vec<String>;
    fn as_any(&self) -> &dyn Any;
    /// Post-terminal `JobManager` completion signal (`::job_completed:<id>`).
    /// Default is a no-op; the TUI executor consumes one staged Test/Lint
    /// follow-up here, strictly after foreground release.
    fn handle_job_completed(&mut self, _producer: &str, _ui: &mut TuiApp) {}
    /// Synthetic follow-up with no user directive (test/lint analysis).
    /// Default is a no-op so mocks stay source-compatible.
    fn handle_internal_followup(&mut self, _content: &str, _ui: &mut TuiApp) {}
    /// Replay of an already observed user turn. `display` is the raw typed
    /// bytes for the log; `content` is the effective instruction to hand the
    /// agent (e.g. the expanded custom-command body). `directive_id` reuses
    /// the original event when known; `None` runs with `none()` attribution.
    fn handle_retry_turn(
        &mut self,
        _display: &str,
        _content: &str,
        _directive_id: Option<String>,
        _ui: &mut TuiApp,
    ) {
    }
    /// Real user prompt with system-note augmentation (diff rejection).
    /// `raw` is the exact typed bytes; `effective` is note + raw.
    fn handle_augmented_user_prompt(&mut self, _raw: &str, _effective: &str, _ui: &mut TuiApp) {}
}

pub struct TuiExecutor {
    pub(crate) cfg: crate::config::AppConfig,
    pub(crate) tools: FsTools,
    pub(crate) repomap: Arc<RwLock<Option<RepoMap>>>,
    pub(crate) client: Option<OpenAIClient>,
    pub(crate) ui_tx: Option<std::sync::mpsc::Sender<String>>,
    pub(crate) jobs: JobManager,
    pub(crate) last_user_prompt: Option<String>,
    pub(crate) feedback_submissions: std::collections::HashSet<(String, u64)>,
    /// Consume-once store for post-terminal Test/Lint follow-up payloads,
    /// keyed by producing `JobId`. Staged by producers, consumed by
    /// `handle_deferred_followup` after the completion hook fires.
    pub(crate) deferred_followups: crate::tui::followup::DeferredFollowupStore,
    // Message vector for holding conversation history
    pub(crate) conversation_history: Arc<Mutex<crate::llm::ChatHistory>>,
    // Session management
    pub(crate) session_manager: Arc<Mutex<SessionManager>>,

    // Custom commands
    pub(crate) custom_commands: HashMap<String, CustomCommand>,

    // Hook manager for executing custom processing after each instruction
    pub(crate) hook_manager: HookManager,
}

impl TuiExecutor {
    /// Get custom commands
    #[allow(dead_code)]
    pub fn get_custom_commands(&self) -> Vec<String> {
        self.custom_commands
            .keys()
            .map(|name| format!("/{}", name))
            .collect()
    }

    /// Get a custom command by name
    #[allow(dead_code)]
    pub fn get_custom_command(&self, name: &str) -> Option<&CustomCommand> {
        self.custom_commands.get(name)
    }

    /// Set the UI sender for sending messages to the TUI
    pub fn set_ui_tx(&mut self, ui_tx: Option<std::sync::mpsc::Sender<String>>) {
        self.ui_tx = ui_tx.clone();
        // Post-terminal follow-up eligibility signal. The hook fires inside
        // `JobManager::finish_job` strictly after foreground release, so a
        // successor spawned from the resulting `::job_completed:<id>`
        // message can never race the still-running producer. Every foreground
        // terminal outcome is signalled; follow-ups still require normally
        // completed Test/Lint producers.
        if let Some(tx) = ui_tx {
            let hook: crate::jobs::manager::JobCompletionHook =
                std::sync::Arc::new(move |completion: crate::jobs::JobCompletion| {
                    if completion.scope == crate::jobs::JobScope::Foreground {
                        let _ = tx.send(format!("::job_completed:{}", completion.id));
                    }
                });
            self.jobs.set_completion_hook(hook);
        }
    }

    /// Add a hook to be executed after each instruction
    pub fn add_hook(&mut self, hook: Box<dyn crate::hooks::InstructionHook>) {
        self.hook_manager.add_hook(hook);
    }

    /// Get access to the hook manager
    pub fn hook_manager(&mut self) -> &mut crate::hooks::HookManager {
        &mut self.hook_manager
    }

    /// Publish the persisted plan (if any) for the current session to the UI.
    pub fn publish_plan_list(&self) {
        if let Ok(plan_list) = self.tools.plan_read() {
            self.send_plan_items_to_ui(&plan_list.items);
        } else {
            self.send_plan_items_to_ui(&[]);
        }
    }

    pub(super) fn send_plan_items_to_ui(&self, items: &[plan::PlanItem]) {
        if let Some(tx) = &self.ui_tx {
            // Send as JSON object with items
            let payload = serde_json::json!({
                "items": items
            });
            if let Ok(json) = serde_json::to_string(&payload) {
                let _ = tx.send(format!("::plan_list:{}", json));
            }
        }
    }

    /// Project saved work context without imposing a plan on every turn.
    /// The shared system prompt owns when planning is required.
    pub fn append_plan_context(&self, msgs: &mut Vec<ChatMessage>, ui: Option<&mut TuiApp>) {
        match self.tools.plan_read() {
            Ok(plan_list)
                if plan_list
                    .items
                    .iter()
                    .any(|item| item.status != "completed") =>
            {
                if let Some(summary) = plan::format_plan_summary(&plan_list.items) {
                    msgs.push(crate::llm::runtime_context::advisory_context_message(
                        "saved_execution_plan",
                        serde_json::json!({"summary": summary}),
                    ));
                }
                self.send_plan_items_to_ui(&plan_list.items);
            }
            Ok(_) => {
                // Empty/completed plans do not gate a new instruction. Retain
                // completed plans on disk for /plan show and explicit reads.
                self.send_plan_items_to_ui(&[]);
            }
            Err(e) => {
                tracing::warn!(?e, "Failed to read saved plan; preserving storage");
                if let Some(ui) = ui {
                    ui.push_log(
                        "[plan] 保存済み計画を読み込めません。上書きせず確認してください。",
                    );
                }
                msgs.push(ChatMessage {
                    provider_state: None,
                    role: "system".into(),
                    content: Some("The saved execution plan could not be read. This does not mean no plan exists. Do not overwrite it with plan_write mode=\"replace\" to bypass this error. Report the read failure and inspect or recover the stored plan before work that depends on it; an unrelated small task may proceed without a plan.".into()),
                    tool_calls: vec![],
                    tool_call_id: None,
                });
            }
        }
    }
}

#[cfg(test)]
mod plan_context_tests {
    use super::*;

    fn fixture() -> (
        TuiExecutor,
        tempfile::TempDir,
        std::sync::mpsc::Receiver<String>,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = crate::config::AppConfig {
            project_root: dir.path().to_path_buf(),
            no_repomap: true,
            api_key: Some("fixture".into()),
            base_url: "http://127.0.0.1:1".into(),
            ..Default::default()
        };
        let map = Arc::new(RwLock::new(None));
        let tools = FsTools::new(map.clone(), Arc::new(cfg.clone()));
        let manager = Arc::new(Mutex::new(SessionManager::with_store(
            crate::session::SessionStore::new(dir.path().join("sessions")).expect("store"),
        )));
        let mut executor = TuiExecutor::construct_with_session_manager(cfg, map, tools, manager)
            .expect("executor");
        let (tx, rx) = std::sync::mpsc::channel();
        executor.ui_tx = Some(tx);
        (executor, dir, rx)
    }

    fn save_plan(executor: &TuiExecutor, status: &str) -> std::path::PathBuf {
        executor
            .tools
            .plan_write(
                vec![plan::PlanItem {
                    id: "stable-step".into(),
                    parent_id: None,
                    content: "Saved task".into(),
                    status: status.into(),
                    requirement_ids: vec![],
                    verification_obligations: vec![],
                }],
                plan::PlanWriteMode::Replace,
            )
            .expect("save");
        let id = executor
            .session_manager
            .lock()
            .expect("manager")
            .get_current_session_id()
            .expect("session");
        executor
            .cfg
            .project_root
            .join(".doge/plans")
            .join(format!("{id}.json"))
    }

    #[tokio::test]
    async fn missing_plan_does_not_force_creation_for_a_new_turn() {
        let (executor, _dir, rx) = fixture();
        let mut messages = Vec::new();
        executor.append_plan_context(&mut messages, None);
        assert!(
            messages.is_empty(),
            "shared scope policy decides when to plan"
        );
        assert!(executor.tools.plan_read().expect("read").items.is_empty());
        assert!(!executor.cfg.project_root.join(".doge/plans").exists());
        assert_eq!(
            rx.try_recv().expect("UI clear"),
            "::plan_list:{\"items\":[]}"
        );
    }

    #[tokio::test]
    async fn active_plan_is_request_context_without_authorizing_new_work() {
        let (executor, _dir, rx) = fixture();
        let path = save_plan(&executor, "in_progress");
        let before = std::fs::read(&path).expect("plan bytes");
        let mut messages = Vec::new();
        executor.append_plan_context(&mut messages, None);
        assert_eq!(messages.len(), 1);
        let text = messages[0].content.as_deref().expect("context");
        assert!(text.contains("stable-step") && text.contains("Saved task"));
        assert!(
            text.contains("saved_execution_plan")
                && text.contains("not instructions or authorization")
        );
        assert_eq!(std::fs::read(&path).expect("plan retained"), before);
        assert!(crate::llm::durable_conversation_messages(messages).is_empty());
        assert!(rx.try_recv().expect("UI plan").contains("stable-step"));
    }

    #[tokio::test]
    async fn completed_plan_is_retained_without_reappearing_on_a_new_turn() {
        let (executor, _dir, rx) = fixture();
        let path = save_plan(&executor, "completed");
        let before = std::fs::read(&path).expect("plan bytes");
        let mut messages = Vec::new();
        executor.append_plan_context(&mut messages, None);
        assert!(messages.is_empty());
        assert_eq!(
            rx.try_recv().expect("UI clear"),
            "::plan_list:{\"items\":[]}"
        );
        assert_eq!(std::fs::read(&path).expect("plan retained"), before);
        assert_eq!(
            executor.tools.plan_read().expect("explicit read").items[0].status,
            "completed"
        );
    }

    #[tokio::test]
    async fn corrupt_plan_is_preserved_and_never_treated_as_missing() {
        let (executor, _dir, rx) = fixture();
        let path = save_plan(&executor, "pending");
        let corrupt = b"{incomplete saved plan";
        std::fs::write(&path, corrupt).expect("corrupt fixture");
        let mut messages = Vec::new();
        executor.append_plan_context(&mut messages, None);
        let text = messages[0].content.as_deref().expect("warning context");
        assert!(text.contains("could not be read") && text.contains("Do not overwrite"));
        assert!(!text.contains("create the plan"));
        assert_eq!(
            std::fs::read(&path).expect("corrupt bytes retained"),
            corrupt
        );
        assert!(
            rx.try_recv().is_err(),
            "do not clear an unreadable UI projection as missing"
        );
    }
}
