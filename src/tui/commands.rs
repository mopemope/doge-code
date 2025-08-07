use crate::analysis::Analyzer;
use crate::llm::{ChatMessage, OpenAIClient};
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
    pub(crate) ui_tx: Option<std::sync::mpsc::Sender<String>>,
    pub(crate) cancel_tx: Option<watch::Sender<bool>>,
}

impl TuiExecutor {
    pub fn new(cfg: crate::config::AppConfig) -> Result<Self> {
        let tools = FsTools::new(&cfg.project_root);
        let analyzer = Analyzer::new(&cfg.project_root)?;
        let client = match cfg.api_key.clone() {
            Some(key) => Some(OpenAIClient::new(cfg.base_url.clone(), key)?),
            None => None,
        };
        Ok(Self {
            cfg,
            tools,
            analyzer,
            client,
            ui_tx: None,
            cancel_tx: None,
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
                ui.push_log(format!("[model: {}]", self.cfg.model));
                ui.push_log("/help, /map, /tools, /clear, /quit");
                ui.push_log("/read <path> [offset limit]");
                ui.push_log("/write <path> <text>");
                ui.push_log("/search <regex> [include_glob]");
            }
            "/tools" => ui.push_log("Available tools: fs_search, fs_read, fs_write"),
            "/clear" => {
                ui.log.clear();
            }
            "/cancel" => {
                if let Some(tx) = &self.cancel_tx {
                    let _ = tx.send(true);
                    ui.push_log("[Cancelled]");
                    if let Some(tx) = &self.ui_tx {
                        let _ = tx.send("::status:cancelled".into());
                    }
                    self.cancel_tx = None;
                } else {
                    ui.push_log("[no running task]");
                }
            }
            "/map" => match self.analyzer.build() {
                Ok(map) => {
                    ui.push_log(format!(
                        "RepoMap (Rust functions): {} symbols",
                        map.symbols.len()
                    ));
                    for s in map.symbols.iter().take(50) {
                        ui.push_log(format!("fn {}  @{}:{}", s.name, s.file.display(), s.line));
                    }
                }
                Err(e) => ui.push_log(format!("map error: {e}")),
            },
            _ => {
                if !line.starts_with('/') {
                    let rest = line;
                    ui.push_log(format!("> {rest}"));
                    if let Some(tx) = &self.ui_tx {
                        let _ = tx.send("::status:streaming".into());
                    }
                    match self.client.as_ref() {
                        Some(c) => {
                            let rt = tokio::runtime::Handle::current();
                            let model = self.cfg.model.clone();
                            if let Some(tx) = &self.ui_tx {
                                let _ = tx.send(format!("::model:hint:[model: {model}]"));
                                let _ = tx.send(String::new());
                            }
                            let content = rest.to_string();
                            let c = c.clone();
                            let tx = self.ui_tx.clone();
                            // Prepare a fresh line for streaming tokens only once per request
                            ui.push_log(String::new());
                            let (cancel_tx, cancel_rx) = watch::channel(false);
                            self.cancel_tx = Some(cancel_tx);
                            rt.spawn(async move {
                                let stream_res = c
                                    .chat_stream(
                                        &model,
                                        vec![ChatMessage {
                                            role: "user".into(),
                                            content: content.clone(),
                                        }],
                                    )
                                    .await;
                                match stream_res {
                                    Ok(mut s) => {
                                        use futures::StreamExt;
                                        let mut had_error = false;
                                        while let Some(item) = s.next().await {
                                            if *cancel_rx.borrow() {
                                                if let Some(tx) = tx.as_ref() {
                                                    let _ = tx.send("[Cancelled]".into());
                                                    let _ = tx.send("::status:cancelled".into());
                                                }
                                                break;
                                            }
                                            match item {
                                                Ok(t) => {
                                                    if let Some(tx) = tx.as_ref() {
                                                        let _ = tx.send(format!("::append:{t}"));
                                                    }
                                                }
                                                Err(e) => {
                                                    had_error = true;
                                                    if let Some(tx) = tx.as_ref() {
                                                        let _ = tx.send(format!(
                                                            "[Error] stream error: {e}"
                                                        ));
                                                        let _ = tx.send("::status:error".into());
                                                    }
                                                    break;
                                                }
                                            }
                                        }
                                        if let Some(tx) = tx {
                                            if !*cancel_rx.borrow() {
                                                if !had_error {
                                                    let _ = tx.send("[Done]".into());
                                                    let _ = tx.send("::status:done".into());
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        if *cancel_rx.borrow() {
                                            if let Some(tx) = tx {
                                                let _ = tx.send("[Cancelled]".into());
                                                let _ = tx.send("::status:cancelled".into());
                                            }
                                            return;
                                        }
                                        let res = c
                                            .chat_once(
                                                &model,
                                                vec![ChatMessage {
                                                    role: "user".into(),
                                                    content,
                                                }],
                                            )
                                            .await;
                                        let out = match res {
                                            Ok(m) => m.content,
                                            Err(err) => format!("LLM error: {err}"),
                                        };
                                        if let Some(tx) = tx {
                                            let _ = tx.send(out);
                                            let _ = tx.send("[Done]".into());
                                            let _ = tx.send("::status:done".into());
                                        }
                                    }
                                }
                            });
                        }
                        None => ui.push_log("OPENAI_API_KEY not set; cannot call LLM."),
                    }
                } else if let Some(rest) = line.strip_prefix("/read ") {
                    let mut parts = rest.split_whitespace();
                    let path = match parts.next() {
                        Some(p) => p,
                        None => {
                            ui.push_log("usage: /read <path> [offset limit]");
                            return;
                        }
                    };
                    let off = parts.next().and_then(|s| s.parse::<usize>().ok());
                    let lim = parts.next().and_then(|s| s.parse::<usize>().ok());
                    match self.tools.fs_read(path, off, lim) {
                        Ok(s) => ui.push_log(s),
                        Err(e) => ui.push_log(format!("read error: {e}")),
                    }
                } else if let Some(rest) = line.strip_prefix("/write ") {
                    let mut parts = rest.splitn(2, ' ');
                    let path = match parts.next() {
                        Some(p) => p,
                        None => {
                            ui.push_log("usage: /write <path> <text>");
                            return;
                        }
                    };
                    let text = match parts.next() {
                        Some(t) => t,
                        None => {
                            ui.push_log("usage: /write <path> <text>");
                            return;
                        }
                    };
                    match self.tools.fs_write(path, text) {
                        Ok(()) => ui.push_log(format!("wrote {} bytes", text.len())),
                        Err(e) => ui.push_log(format!("write error: {e}")),
                    }
                } else if let Some(rest) = line.strip_prefix("/search ") {
                    let mut parts = rest.split_whitespace();
                    let regex = match parts.next() {
                        Some(p) => p,
                        None => {
                            ui.push_log("usage: /search <regex> [include_glob]");
                            return;
                        }
                    };
                    let include = parts.next();
                    match self.tools.fs_search(regex, include) {
                        Ok(rows) => {
                            for (p, ln, text) in rows.into_iter().take(50) {
                                ui.push_log(format!("{}:{}: {}", p.display(), ln, text));
                            }
                        }
                        Err(e) => ui.push_log(format!("search error: {e}")),
                    }
                } else {
                    ui.push_log(format!("> {line}"));
                }
            }
        }
    }
}
