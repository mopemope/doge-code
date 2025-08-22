use crate::analysis::Analyzer;
use crate::tui::commands::core::{CommandHandler, TuiExecutor};
use crate::tui::theme::Theme;
use crate::tui::view::TuiApp;
use tokio::sync::watch;
use tracing::{error, info, warn};

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
                    "/help, /map, /tools, /clear, /open <path>, /quit, /theme <name>, /session <new|list|switch|save|delete|current|clear>, /rebuild-repomap, /tokens",
                );
            }
            "/tools" => ui.push_log("Available tools: fs_search, fs_read, fs_write "),
            "/clear" => {
                ui.clear_log();
            }
            "/rebuild-repomap" => {
                // Force complete rebuild (ignore cache)
                ui.push_log("[Starting forced complete repomap rebuild (ignoring cache)...]");
                let repomap_clone = self.repomap.clone();
                let project_root = self.cfg.project_root.clone();
                let ui_tx = self.ui_tx.clone();

                tokio::spawn(async move {
                    info!("Starting forced complete repomap rebuild");
                    let start_time = std::time::Instant::now();

                    let mut analyzer = match Analyzer::new(&project_root) {
                        Ok(analyzer) => analyzer,
                        Err(e) => {
                            error!("Failed to create Analyzer: {:?}", e);
                            if let Some(tx) = ui_tx {
                                let _ = tx.send(format!("[Failed to create analyzer: {}]", e));
                            }
                            return;
                        }
                    };

                    // Clear cache first to force complete rebuild
                    if let Err(e) = analyzer.clear_cache() {
                        warn!("Failed to clear cache before forced rebuild: {}", e);
                    }

                    match analyzer.build_parallel().await {
                        Ok(map) => {
                            let duration = start_time.elapsed();
                            let symbol_count = map.symbols.len();
                            *repomap_clone.write().await = Some(map);

                            info!(
                                "Forced repomap rebuild completed in {:?} with {} symbols",
                                duration, symbol_count
                            );
                            if let Some(tx) = ui_tx {
                                let _ = tx.send(format!(
                                    "[Forced rebuild completed in {:?} - {} symbols found (cache cleared)]",
                                    duration, symbol_count
                                ));
                            }
                        }
                        Err(e) => {
                            error!("Failed to force rebuild RepoMap: {:?}", e);
                            if let Some(tx) = ui_tx {
                                let _ =
                                    tx.send(format!("[Failed to force rebuild repomap: {}]", e));
                            }
                        }
                    }
                });
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

                // Handle /tokens command to show token usage
                if line == "/tokens" {
                    if let Some(client) = &self.client {
                        let tokens_used = client.get_tokens_used();
                        ui.push_log(format!("Total tokens used: {}", tokens_used));
                    } else {
                        ui.push_log("No LLM client available.");
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
                            let sys_prompt =
                                crate::tui::commands::prompt::build_system_prompt(&self.cfg);
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
                                // Get token usage after the agent loop completes
                                let tokens_used = c.get_tokens_used();
                                match res {
                                    Ok((updated_messages, final_msg)) => {
                                        if let Some(tx) = tx {
                                            let _ = tx.send(final_msg.content.clone());
                                            let _ = tx.send("::status:done".into());
                                            // Send token usage update
                                            let _ = tx.send(format!("::tokens:{}", tokens_used));
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
                                            // Send token usage update even on error
                                            let _ = tx.send(format!("::tokens:{}", tokens_used));
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

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
