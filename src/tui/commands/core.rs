use crate::analysis::RepoMap;
use crate::llm::OpenAIClient;
use crate::planning::{PlanManager, TaskAnalyzer};
use crate::session::SessionManager;
use crate::tools::FsTools;
use crate::tui::commands::handlers::custom::CustomCommand;
use crate::tui::view::TuiApp;
use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{RwLock, watch};

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

    // Task analyzer for planning
    pub(crate) task_analyzer: TaskAnalyzer,
    // Plan manager for execution
    pub(crate) plan_manager: Arc<Mutex<PlanManager>>,

    // Custom commands
    pub(crate) custom_commands: HashMap<String, CustomCommand>,
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
}
