use crate::analysis::Analyzer;
use crate::llm::OpenAIClient;
use crate::tools::FsTools;
use crate::tui::view::TuiApp;
use anyhow::Result;
use tokio::sync::watch;

pub trait CommandHandler {
    fn handle(&mut self, line: &str, ui: &mut TuiApp);
}

pub struct TuiExecutor {
    pub(crate) cfg: crate::config::AppConfig,
    pub(crate) tools: FsTools,
    pub(crate) analyzer: Analyzer,
    pub(crate) client: Option<OpenAIClient>,
    #[allow(dead_code)]
    pub(crate) history: crate::llm::ChatHistory,
    pub(crate) ui_tx: Option<std::sync::mpsc::Sender<String>>,
    pub(crate) cancel_tx: Option<watch::Sender<bool>>,
    pub(crate) last_user_prompt: Option<String>,
}

impl TuiExecutor {
    pub fn new(cfg: crate::config::AppConfig) -> Result<Self> {
        let tools = FsTools::new(&cfg.project_root);
        let analyzer = Analyzer::new(&cfg.project_root)?;
        let client = match cfg.api_key.clone() {
            Some(key) => Some(OpenAIClient::new(cfg.base_url.clone(), key)?),
            None => None,
        };
        // Load system prompt
        let sys_prompt = std::fs::read_to_string("resources/system_prompt.md").ok();
        let mut history = crate::llm::ChatHistory::new(12_000, sys_prompt);
        history.append_system_once();

        Ok(Self {
            cfg,
            tools,
            analyzer,
            client,
            history,
            ui_tx: None,
            cancel_tx: None,
            last_user_prompt: None,
        })
    }
}

impl CommandHandler for TuiExecutor {
    fn handle(&mut self, line: &str, ui: &mut TuiApp) {
        if self.ui_tx.is_none() {
            self.ui_tx = ui.sender();
        }
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        match line {
            "/help" => {
                ui.push_log("/help, /map, /tools, /clear, /quit, /retry");
            }
            "/tools" => ui.push_log("Available tools: fs_search, fs_read, fs_write"),
            "/clear" => {
                ui.log.clear();
            }
            "/cancel" => {
                if let Some(tx) = &self.cancel_tx {
                    let _ = tx.send(true);
                    if let Some(tx) = &self.ui_tx {
                        let _ = tx.send("::status:cancelled".into());
                    }
                    ui.push_log("[Cancelled]");
                    self.cancel_tx = None;
                } else {
                    ui.push_log("[no running task]");
                }
            }
            "/retry" => {
                if self.cancel_tx.is_some() {
                    ui.push_log("[busy] streaming in progress; use /cancel first");
                    return;
                }
                match self.last_user_prompt.clone() {
                    Some(prompt) => {
                        // Re-dispatch as if user typed it
                        self.handle(&prompt, ui);
                    }
                    None => ui.push_log("[no previous prompt]"),
                }
            }
            "/map" => match self.analyzer.build() {
                Ok(map) => {
                    ui.push_log(format!("RepoMap: {} symbols", map.symbols.len()));
                    for s in map.symbols.iter().take(50) {
                        ui.push_log(format!(
                            "{} {}  @{}:{}",
                            s.kind.as_str(),
                            s.name,
                            s.file.display(),
                            s.start_line
                        ));
                    }
                }
                Err(e) => ui.push_log(format!("map error: {e}")),
            },
            _ => {
                if !line.starts_with('/') {
                    let rest = line;
                    self.last_user_prompt = Some(rest.to_string());
                    ui.push_log(format!("> {rest}"));
                    match self.client.as_ref() {
                        Some(c) => {
                            let rt = tokio::runtime::Handle::current();
                            let model = self.cfg.model.clone();
                            let content = rest.to_string();
                            let c = c.clone();
                            let tx = self.ui_tx.clone();
                            // Prepare a fresh line for the final output
                            ui.push_log(String::new());
                            let (cancel_tx, cancel_rx) = watch::channel(false);
                            self.cancel_tx = Some(cancel_tx);
                            // Build initial messages with optional system prompt + user
                            let mut msgs = Vec::new();
                            if let Ok(sys) = std::fs::read_to_string("resources/system_prompt.md") {
                                msgs.push(crate::llm::ChatMessage {
                                    role: "system".into(),
                                    content: sys,
                                });
                            }
                            msgs.push(crate::llm::ChatMessage {
                                role: "user".into(),
                                content: content.clone(),
                            });
                            let fs = self.tools.clone();
                            rt.spawn(async move {
                                if *cancel_rx.borrow() {
                                    if let Some(tx) = tx {
                                        let _ = tx.send("::status:cancelled".into());
                                        let _ = tx.send("[Cancelled]".into());
                                    }
                                    return;
                                }
                                let res = crate::llm::run_agent_loop(&c, &model, &fs, msgs).await;
                                match res {
                                    Ok(msg) => {
                                        if let Some(tx) = tx {
                                            let _ = tx.send(msg.content);
                                            let _ = tx.send("::status:done".into());
                                        }
                                    }
                                    Err(e) => {
                                        if let Some(tx) = tx {
                                            let _ = tx.send(format!("LLM error: {e}"));
                                            let _ = tx.send("::status:error".into());
                                        }
                                    }
                                }
                            });
                        }
                        None => ui.push_log("OPENAI_API_KEY not set; cannot call LLM."),
                    }
                } else {
                    ui.push_log(format!("> {line}"));
                }
            }
        }
    }
}
