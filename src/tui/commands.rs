use crate::analysis::{Analyzer, RepoMap};
use crate::assets::Assets;
use crate::llm::OpenAIClient;
use crate::tools::FsTools;
use crate::tui::commands_sessions::SessionManager;
use crate::tui::theme::Theme;
use crate::tui::view::TuiApp;
use anyhow::Result;
use chrono::Local;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tera::{Context, Tera};
use tokio::sync::{RwLock, watch};
use tracing::{error, info};

/// Finds the project instructions file based on a priority list.
/// Checks for AGENTS.md, QWEN.md, or GEMINI.md in that order within the project root.
pub(crate) fn find_project_instructions_file(project_root: &Path) -> Option<PathBuf> {
    let priority_files = ["AGENTS.md", "QWEN.md", "GEMINI.md"];
    for file_name in &priority_files {
        let path = project_root.join(file_name);
        if path.exists() {
            return Some(path);
        }
    }
    None
}

/// Inner function for loading project instructions, allowing for mocking the file reader.
pub(crate) fn load_project_instructions_inner<F>(
    project_root: &Path,
    read_file: F,
) -> Option<String>
where
    F: Fn(&Path) -> std::io::Result<String>,
{
    if let Some(path) = find_project_instructions_file(project_root) {
        match read_file(&path) {
            Ok(content) => Some(content),
            Err(e) => {
                error!("Failed to read {}: {}", path.display(), e);
                None
            }
        }
    } else {
        None
    }
}

/// Load project-specific instructions from a file.
/// Checks for AGENTS.md, QWEN.md, or GEMINI.md in that order.
fn load_project_instructions(cfg: &crate::config::AppConfig) -> Option<String> {
    load_project_instructions_inner(&cfg.project_root, |p: &Path| std::fs::read_to_string(p))
}

/// Combine the base system prompt with project-specific instructions.
fn build_system_prompt(cfg: &crate::config::AppConfig) -> String {
    let mut tera = Tera::default();
    let mut context = Context::new();

    let sys_prompt_template =
        String::from_utf8(Assets::get("system_prompt.md").unwrap().data.to_vec())
            .unwrap_or_default();

    context.insert("date", &Local::now().format("%Y年%m月%d日 %A").to_string());
    context.insert("os", &std::env::consts::OS);
    context.insert("project_dir", &cfg.project_root.to_string_lossy());
    context.insert(
        "shell",
        &env::var("SHELL").unwrap_or_else(|_| "unknown".to_string()),
    );

    let base_sys_prompt = tera
        .render_str(&sys_prompt_template, &context)
        .unwrap_or_else(|e| {
            error!("Failed to render system prompt: {e}");
            sys_prompt_template // fallback to the original template
        });

    let project_instructions = load_project_instructions(cfg);
    if let Some(instructions) = project_instructions {
        format!("{base_sys_prompt}\n\n# Project-Specific Instructions\n{instructions}")
    } else {
        base_sys_prompt
    }
}

pub trait CommandHandler {
    fn handle(&mut self, line: &str, ui: &mut TuiApp);
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
    // 会話履歴を保持するためのメッセージベクター
    pub(crate) conversation_history: Arc<Mutex<Vec<crate::llm::types::ChatMessage>>>,
    // Session management
    pub(crate) session_manager: Arc<Mutex<SessionManager>>,
}

impl TuiExecutor {
    pub fn new(cfg: crate::config::AppConfig) -> Result<Self> {
        info!("Initializing TuiExecutor");
        let repomap: Arc<RwLock<Option<RepoMap>>> = Arc::new(RwLock::new(None));
        let tools = FsTools::new(repomap.clone());
        let repomap_clone = repomap.clone();
        let project_root = cfg.project_root.clone();

        // Spawn an asynchronous task
        tokio::spawn(async move {
            info!(
                "Starting background repomap generation for project at {:?}",
                project_root
            );
            let start_time = std::time::Instant::now();
            let mut analyzer = match Analyzer::new(&project_root) {
                Ok(analyzer) => analyzer,
                Err(e) => {
                    error!("Failed to create Analyzer: {:?}", e);
                    return;
                }
            };

            match analyzer.build().await {
                Ok(map) => {
                    let duration = start_time.elapsed();
                    let symbol_count = map.symbols.len();
                    *repomap_clone.write().await = Some(map);
                    tracing::debug!(
                        "Background repomap generation completed in {:?} with {} symbols",
                        duration,
                        symbol_count
                    );
                }
                Err(e) => {
                    error!("Failed to build RepoMap: {:?}", e);
                }
            }
        });

        let client = match cfg.api_key.clone() {
            Some(key) => Some(OpenAIClient::new(cfg.base_url.clone(), key)?),
            None => None,
        };
        // Load system prompt
        let sys_prompt = build_system_prompt(&cfg);
        let mut history = crate::llm::ChatHistory::new(12_000, Some(sys_prompt));
        history.append_system_once();

        // Initialize session manager
        let session_manager = Arc::new(Mutex::new(SessionManager::new()?));

        Ok(Self {
            cfg,
            tools,
            repomap,
            client,
            history,
            ui_tx: None,
            cancel_tx: None,
            last_user_prompt: None,
            conversation_history: Arc::new(Mutex::new(Vec::new())), // 会話履歴を初期化
            session_manager,
        })
    }

    fn handle_session_command(&mut self, args: &str, ui: &mut TuiApp) -> Result<()> {
        let args: Vec<&str> = args.split_whitespace().collect();
        if args.is_empty() {
            ui.push_log("Usage: /session <new|list|switch|save|delete|current|clear>");
            return Ok(());
        }

        let mut session_manager = self.session_manager.lock().unwrap();

        match args[0] {
            "list" => match session_manager.list_sessions() {
                Ok(sessions) => {
                    if sessions.is_empty() {
                        ui.push_log("No sessions found.");
                    } else {
                        ui.push_log("Sessions:");
                        for session in sessions {
                            ui.push_log(format!(
                                "  {} - {} (Created: {})",
                                session.id, session.title, session.created_at
                            ));
                        }
                    }
                }
                Err(e) => ui.push_log(format!("Failed to list sessions: {}", e)),
            },
            "new" => {
                let title = if args.len() > 1 {
                    args[1..].join(" ")
                } else {
                    "Untitled".to_string()
                };
                match session_manager.create_session(&title) {
                    Ok(()) => {
                        if let Some(info) = session_manager.current_session_info() {
                            ui.push_log(format!("Created new session:\n{}", info));
                        }
                    }
                    Err(e) => ui.push_log(format!("Failed to create session: {}", e)),
                }
            }
            "switch" => {
                if args.len() != 2 {
                    ui.push_log("Usage: /session switch <id>");
                    return Ok(());
                }
                let id = args[1];
                match session_manager.load_session(id) {
                    Ok(()) => {
                        if let Some(info) = session_manager.current_session_info() {
                            ui.push_log(format!("Switched to session:\n{}", info));
                        }
                        // Load conversation history from session
                        if let (Some(session), Ok(mut history)) = (
                            &session_manager.current_session,
                            self.conversation_history.lock(),
                        ) {
                            history.clear();
                            // Deserialize session history entries into ChatMessage objects
                            for entry in &session.history {
                                if let Ok(msg) =
                                    serde_json::from_str::<crate::llm::types::ChatMessage>(entry)
                                {
                                    history.push(msg);
                                }
                            }
                        }
                    }
                    Err(e) => ui.push_log(format!("Failed to switch session: {}", e)),
                }
            }
            "save" => {
                // This is implicitly handled when history is updated.
                // We can add an explicit save if needed.
                ui.push_log("Session is saved automatically.");
            }
            "delete" => {
                if args.len() != 2 {
                    ui.push_log("Usage: /session delete <id>");
                    return Ok(());
                }
                let id = args[1];
                match session_manager.delete_session(id) {
                    Ok(()) => ui.push_log(format!("Deleted session: {}", id)),
                    Err(e) => ui.push_log(format!("Failed to delete session: {}", e)),
                }
            }
            "current" => {
                if let Some(info) = session_manager.current_session_info() {
                    ui.push_log(info);
                } else {
                    ui.push_log("No session loaded.");
                }
            }
            "clear" => match session_manager.clear_current_session_history() {
                Ok(()) => ui.push_log("Cleared current session history."),
                Err(e) => ui.push_log(format!("Failed to clear session history: {}", e)),
            },
            _ => {
                ui.push_log("Unknown session command. Usage: /session <new|list|switch|save|delete|current|clear>");
            }
        }
        Ok(())
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
                ui.push_log(
                    "/help, /map, /tools, /clear, /open <path>, /quit, /retry, /theme <name>, /session <new|list|switch|save|delete|current|clear>",
                );
            }
            "/tools" => ui.push_log("Available tools: fs_search, fs_read, fs_write "),
            "/clear" => {
                ui.clear_log();
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
                    ui.push_log("[busy] streaming in progress; use /cancel first ");
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
            "/map" => {
                // Check if repomap has been generated
                let repomap = self.repomap.clone();
                let ui_tx = self.ui_tx.clone().unwrap();
                tokio::spawn(async move {
                    let repomap_guard = repomap.read().await;
                    if let Some(map) = &*repomap_guard {
                        let _ = ui_tx.send(format!("RepoMap: {} symbols ", map.symbols.len()));
                        for s in map.symbols.iter().take(50) {
                            let _ = ui_tx.send(format!(
                                "{} {}  @{}:{}",
                                s.kind.as_str(),
                                s.name,
                                s.file.display(),
                                s.start_line
                            ));
                        }
                    } else {
                        let _ = ui_tx.send("[repomap] Still generating...".to_string());
                    }
                });
            }
            // Handle /theme command
            line if line.starts_with("/theme ") => {
                let theme_name = line[7..].trim(); // get the string after "/theme "
                match theme_name.to_lowercase().as_str() {
                    "dark" => {
                        ui.theme = Theme::dark();
                        ui.push_log("[Theme switched to dark]");
                    }
                    "light" => {
                        ui.theme = Theme::light();
                        ui.push_log("[Theme switched to light]");
                    }
                    _ => {
                        ui.push_log(format!(
                            "[Unknown theme: {theme_name}. Available themes: dark, light]"
                        ));
                    }
                }
                // Redraw after theme change
                if let Err(e) = ui.draw_with_model(Some(&self.cfg.model)) {
                    ui.push_log(format!("Failed to redraw after theme change: {e}"));
                }
            }
            _ => {
                if let Some(rest) = line.strip_prefix("/open ") {
                    let path_arg = rest.trim();
                    if path_arg.is_empty() {
                        ui.push_log("usage: /open <path>");
                        return;
                    }
                    // Resolve to absolute path; allow project-internal paths and absolute paths
                    let p = std::path::Path::new(path_arg);
                    let abs = if p.is_absolute() {
                        p.to_path_buf()
                    } else {
                        self.cfg.project_root.join(p)
                    };
                    if !abs.exists() {
                        ui.push_log(format!("not found: {}", abs.display()));
                        return;
                    }
                    // Leave TUI alt screen temporarily while spawning editor in blocking mode
                    use crossterm::{cursor, execute, terminal};
                    let mut stdout = std::io::stdout();
                    let _ = execute!(stdout, terminal::LeaveAlternateScreen, cursor::Show);
                    let _ = terminal::disable_raw_mode();

                    // Choose editor from $EDITOR, then $VISUAL, else fallback list
                    let editor = std::env::var("EDITOR")
                        .ok()
                        .or_else(|| std::env::var("VISUAL").ok())
                        .unwrap_or_else(|| "vi".to_string());
                    let status = std::process::Command::new(&editor).arg(&abs).status();

                    // Re-enter TUI
                    let _ = terminal::enable_raw_mode();
                    let _ = execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide);

                    match status {
                        Ok(s) if s.success() => ui.push_log(format!("opened: {}", abs.display())),
                        Ok(s) => ui.push_log(format!("editor exited with status {s}")),
                        Err(e) => ui.push_log(format!("failed to launch editor: {e}")),
                    }
                    return;
                }

                if let Some(rest) = line.strip_prefix("/session ") {
                    match self.handle_session_command(rest.trim(), ui) {
                        Ok(_) => {} // No-op on success
                        Err(e) => ui.push_log(format!("Error handling session command: {}", e)),
                    }
                    return;
                }

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

                            // LLMリクエスト準備開始を通知
                            if let Some(tx) = &self.ui_tx {
                                let _ = tx.send("::status:preparing".into());
                            }

                            // Build initial messages with optional system prompt + user
                            let mut msgs = Vec::new();
                            // Load system prompt
                            let sys_prompt = build_system_prompt(&self.cfg);
                            msgs.push(crate::llm::ChatMessage {
                                role: "system".into(),
                                content: Some(sys_prompt),
                                tool_calls: vec![],
                                tool_call_id: None,
                            });

                            // 既存の会話履歴を追加
                            if let Ok(history) = self.conversation_history.lock() {
                                msgs.extend(history.clone());
                            }

                            msgs.push(crate::llm::ChatMessage {
                                role: "user".into(),
                                content: Some(content.clone()),
                                tool_calls: vec![],
                                tool_call_id: None,
                            });
                            let fs = self.tools.clone();
                            let conversation_history = self.conversation_history.clone();
                            let session_manager = self.session_manager.clone();
                            rt.spawn(async move {
                                if *cancel_rx.borrow() {
                                    if let Some(tx) = tx {
                                        let _ = tx.send("::status:cancelled".into());
                                        let _ = tx.send("[Cancelled]".into());
                                    }
                                    return;
                                }

                                // リクエスト送信開始を通知
                                if let Some(tx) = &tx {
                                    let _ = tx.send("::status:sending".into());
                                }

                                let res =
                                    crate::llm::run_agent_loop(&c, &model, &fs, msgs, tx.clone())
                                        .await;
                                match res {
                                    Ok((updated_messages, final_msg)) => {
                                        if let Some(tx) = tx {
                                            let _ = tx.send(final_msg.content.clone());
                                            let _ = tx.send("::status:done".into());
                                        }
                                        // 会話履歴を更新（systemメッセージを除く全てのメッセージを保存）
                                        if let Ok(mut history) = conversation_history.lock() {
                                            // systemメッセージ以外の新しいメッセージを抽出
                                            let new_messages: Vec<_> = updated_messages
                                                .into_iter()
                                                .filter(|msg| msg.role != "system")
                                                .collect();

                                            // 既存の履歴をクリアして新しいメッセージで置き換え
                                            history.clear();
                                            history.extend(new_messages);

                                            // セッションにも会話履歴を保存
                                            let mut sm = session_manager.lock().unwrap();
                                            let _ =
                                                sm.update_current_session_with_history(&history);
                                        }
                                    }
                                    Err(e) => {
                                        if let Some(tx) = tx {
                                            let _ = tx.send(format!("LLM error: {e}"));
                                            let _ = tx.send("::status:error".into());
                                        }
                                        // エラー時も会話履歴を更新（ユーザーの入力のみ）
                                        if let Ok(mut history) = conversation_history.lock() {
                                            history.push(crate::llm::ChatMessage {
                                                role: "user".into(),
                                                content: Some(content.clone()),
                                                tool_calls: vec![],
                                                tool_call_id: None,
                                            });

                                            // セッションにも会話履歴を保存
                                            let mut sm = session_manager.lock().unwrap();
                                            let _ =
                                                sm.update_current_session_with_history(&history);
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

#[cfg(test)]
mod tests {
    use super::*;
    // use std::io; // removed
    // use std::path::PathBuf; // removed

    #[test]
    fn test_find_project_instructions_file_found_agents_md() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path();
        let agents_md_path = project_root.join("AGENTS.md");
        let qwen_md_path = project_root.join("QWEN.md");
        let gemini_md_path = project_root.join("GEMINI.md");

        // Create AGENTS.md
        std::fs::write(&agents_md_path, "Agents instructions content").unwrap();
        // Also create QWEN.md and GEMINI.md to show AGENTS.md has priority
        std::fs::write(&qwen_md_path, "Qwen instructions content").unwrap();
        std::fs::write(&gemini_md_path, "Gemini instructions content").unwrap();

        let found_path = find_project_instructions_file(project_root);
        assert_eq!(found_path, Some(agents_md_path));
    }

    #[test]
    fn test_find_project_instructions_file_found_qwen_md() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path();
        let agents_md_path = project_root.join("AGENTS.md"); // explicit: AGENTS.md should not exist
        let qwen_md_path = project_root.join("QWEN.md");
        let gemini_md_path = project_root.join("GEMINI.md");

        // AGENTS.md should not exist for this test to be valid
        assert!(!agents_md_path.exists());

        // Create QWEN.md (AGENTS.md does not exist)
        std::fs::write(&qwen_md_path, "Qwen instructions content").unwrap();
        // Also create GEMINI.md to show QWEN.md has priority
        std::fs::write(&gemini_md_path, "Gemini instructions content").unwrap();

        let found_path = find_project_instructions_file(project_root);
        assert_eq!(found_path, Some(qwen_md_path));
    }

    #[test]
    fn test_find_project_instructions_file_found_gemini_md() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path();
        let agents_md_path = project_root.join("AGENTS.md"); // explicit: AGENTS.md should not exist
        let qwen_md_path = project_root.join("QWEN.md"); // explicit: QWEN.md should not exist
        let gemini_md_path = project_root.join("GEMINI.md");

        // AGENTS.md and QWEN.md should not exist for this test to be valid
        assert!(!agents_md_path.exists());
        assert!(!qwen_md_path.exists());

        // Create GEMINI.md (AGENTS.md and QWEN.md do not exist)
        std::fs::write(&gemini_md_path, "Gemini instructions content").unwrap();

        let found_path = find_project_instructions_file(project_root);
        assert_eq!(found_path, Some(gemini_md_path));
    }

    #[test]
    fn test_find_project_instructions_file_not_found() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path();

        let found_path = find_project_instructions_file(project_root);
        assert_eq!(found_path, None);
    }

    // 会話履歴の引き継ぎをテストする
    #[tokio::test]
    async fn test_conversation_history_persistence() {
        use crate::llm::types::ChatMessage;
        use std::sync::{Arc, Mutex};

        // 会話履歴を模擬する
        let conversation_history = Arc::new(Mutex::new(Vec::new()));

        // 最初のメッセージを追加
        {
            let mut history = conversation_history.lock().unwrap();
            history.push(ChatMessage {
                role: "user".into(),
                content: Some("こんにちは".into()),
                tool_calls: vec![],
                tool_call_id: None,
            });
            history.push(ChatMessage {
                role: "assistant".into(),
                content: Some("こんにちは!どのようにお手伝いできますか?".into()),
                tool_calls: vec![],
                tool_call_id: None,
            });
        }

        // 会話履歴が正しく保持されているか確認
        {
            let history = conversation_history.lock().unwrap();
            assert_eq!(history.len(), 2);
            assert_eq!(history[0].role, "user");
            assert_eq!(history[0].content, Some("こんにちは".into()));
            assert_eq!(history[1].role, "assistant");
            assert_eq!(
                history[1].content,
                Some("こんにちは!どのようにお手伝いできますか?".into())
            );
        }

        // 新しいメッセージを追加
        {
            let mut history = conversation_history.lock().unwrap();
            history.push(ChatMessage {
                role: "user".into(),
                content: Some("前回のメッセージは何でしたか?".into()),
                tool_calls: vec![],
                tool_call_id: None,
            });
        }

        // 会話履歴が3つのメッセージを含んでいるか確認
        {
            let history = conversation_history.lock().unwrap();
            assert_eq!(history.len(), 3);
            assert_eq!(history[2].role, "user");
            assert_eq!(
                history[2].content,
                Some("前回のメッセージは何でしたか?".into())
            );
        }
    }
}
