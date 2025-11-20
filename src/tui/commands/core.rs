use crate::analysis::RepoMap;
use crate::hooks::HookManager;
use crate::llm::OpenAIClient;

use crate::llm::types::ChatMessage;
use crate::session::SessionManager;
use crate::tools::{FsTools, plan};
use crate::tui::commands::handlers::custom::CustomCommand;
use crate::tui::view::TuiApp;
use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{RwLock, watch};

const PLAN_CREATION_GUIDANCE: &str = "Plan requirements:\n- Produce at least three ordered steps with stable unique ids (e.g., step-1)\n- Default each status to \"pending\" and update via plan_write mode=\"merge\"\n- Keep only one item in_progress at a time\n- Describe expected outputs (files, tests) so implementation stays concrete\n";

pub trait CommandHandler {
    fn handle(&mut self, line: &str, ui: &mut TuiApp);
    fn get_custom_commands(&self) -> Vec<String>;
    fn as_any(&self) -> &dyn Any;
}

pub struct TuiExecutor {
    pub(crate) cfg: crate::config::AppConfig,
    pub(crate) tools: FsTools,
    pub(crate) repomap: Arc<RwLock<Option<RepoMap>>>,
    pub(crate) client: Option<OpenAIClient>,
    #[allow(dead_code)]
    pub(crate) history: crate::llm::ChatHistory,
    pub(crate) ui_tx: Option<std::sync::mpsc::Sender<String>>,
    pub(crate) cancel_tx: Option<watch::Sender<bool>>,
    pub(crate) last_user_prompt: Option<String>,
    // Message vector for holding conversation history
    pub(crate) conversation_history: Arc<Mutex<Vec<crate::llm::types::ChatMessage>>>,
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
        self.ui_tx = ui_tx;
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

    fn send_plan_items_to_ui(&self, items: &[plan::PlanItem]) {
        if let Some(tx) = &self.ui_tx
            && let Ok(json) = serde_json::to_string(items)
        {
            let _ = tx.send(format!("::plan_list:{}", json));
        }
    }

    fn push_plan_creation_directive(
        &self,
        msgs: &mut Vec<ChatMessage>,
        instruction: &str,
        ui: Option<&mut TuiApp>,
        reason: &str,
    ) {
        if let Some(ui) = ui {
            ui.push_log(
                "[plan] 計画が未作成のため plan_write (mode=\"replace\") で作成して下さい。",
            );
        }
        let directive = format!(
            "{}\n{}\nBefore acting on the new instruction, call plan_write with mode=\"replace\" to create the plan. After saving it, resume work on: {}",
            reason, PLAN_CREATION_GUIDANCE, instruction
        );
        msgs.push(ChatMessage {
            role: "system".into(),
            content: Some(directive),
            tool_calls: vec![],
            tool_call_id: None,
        });
        self.send_plan_items_to_ui(&[]);
    }

    pub fn enforce_plan_context(
        &self,
        msgs: &mut Vec<ChatMessage>,
        instruction: &str,
        ui: Option<&mut TuiApp>,
    ) {
        match self.tools.plan_read() {
            Ok(plan_list) if !plan_list.items.is_empty() => {
                if let Some(summary) = plan::format_plan_summary(&plan_list.items) {
                    let plan_msg = format!(
                        "Current execution plan (keep statuses in sync via plan_write mode=\"merge\" and reference plan_read when needed):\n{}",
                        summary
                    );
                    msgs.push(ChatMessage {
                        role: "system".into(),
                        content: Some(plan_msg),
                        tool_calls: vec![],
                        tool_call_id: None,
                    });
                }
                if let Some(ui) = ui {
                    ui.push_log(
                        "[plan] 現在の計画を読み込みました。進捗は plan_write で更新して下さい。",
                    );
                }
                self.send_plan_items_to_ui(&plan_list.items);
            }
            Ok(plan_list) => {
                // Plan file exists but has no steps
                self.send_plan_items_to_ui(&plan_list.items);
                self.push_plan_creation_directive(
                    msgs,
                    instruction,
                    ui,
                    "Plan storage exists but contains no steps.",
                );
            }
            Err(e) => {
                tracing::warn!(?e, "Failed to read plan; requesting plan creation");
                self.push_plan_creation_directive(
                    msgs,
                    instruction,
                    ui,
                    "No execution plan available for the current session.",
                );
            }
        }
    }
}
