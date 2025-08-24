use crate::analysis::Analyzer;
use crate::planning::create_task_plan;
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
                ui.push_log("Available commands:");
                ui.push_log("  /help - Show this help message");
                ui.push_log("  /map - Show repository analysis");
                ui.push_log("  /tools - List available tools");
                ui.push_log("  /clear - Clear the log area");
                ui.push_log("  /open <path> - Open a file in your editor");
                ui.push_log("  /quit - Exit the application");
                ui.push_log("  /theme <name> - Switch theme (dark/light)");
                ui.push_log("  /session <cmd> - Session management (new|list|switch|save|delete|current|clear)");
                ui.push_log("  /rebuild-repomap - Rebuild repository analysis");
                ui.push_log("  /tokens - Show token usage");
                ui.push_log("");
                ui.push_log("Scroll controls:");
                ui.push_log("  Page Up/Down - Scroll by page");
                ui.push_log("  Ctrl+Up/Down - Scroll by line");
                ui.push_log("  Ctrl+Home - Scroll to top");
                ui.push_log("  Ctrl+End - Scroll to bottom");
                ui.push_log("  Ctrl+L - Return to bottom (auto-scroll)");
                ui.push_log("");
                ui.push_log("Other controls:");
                ui.push_log("  @ - File completion");
                ui.push_log("  ! - Shell mode (at start of empty line)");
                ui.push_log("  Esc - Cancel operation or exit shell mode");
                ui.push_log("  Ctrl+C - Cancel (press twice to exit)");
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

                    let mut analyzer = match Analyzer::new(&project_root).await {
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
                    if let Err(e) = analyzer.clear_cache().await {
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
                ui.dirty = true;
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

                // Handle /plan command for task analysis and planning
                if let Some(rest) = line.strip_prefix("/plan ") {
                    let task_description = rest.trim();
                    if task_description.is_empty() {
                        ui.push_log("Usage: /plan <task description>");
                        return;
                    }

                    ui.push_log(format!("> /plan {}", task_description));

                    // 非同期処理のためにtokioランタイムを使用
                    let task_analyzer = &self.task_analyzer;
                    let task_desc = task_description.to_string();

                    // 分析は同期的に実行
                    match task_analyzer.analyze(&task_desc) {
                        Ok(classification) => {
                            ui.push_log("🔍 タスクを分析中...");
                            ui.push_log(format!("📋 タスク分類: {:?}", classification.task_type));
                            ui.push_log(format!(
                                "🎯 複雑度: {:.1}/1.0",
                                classification.complexity_score
                            ));
                            ui.push_log(format!(
                                "📊 推定ステップ数: {}",
                                classification.estimated_steps
                            ));
                            ui.push_log(format!(
                                "⚠️ リスクレベル: {:?}",
                                classification.risk_level
                            ));
                            ui.push_log(format!(
                                "🔧 必要ツール: {}",
                                classification.required_tools.join(", ")
                            ));
                            ui.push_log(format!(
                                "✅ 信頼度: {:.1}%",
                                classification.confidence * 100.0
                            ));

                            // 分解は非同期で実行するため、別スレッドで処理
                            let rt = tokio::runtime::Handle::current();
                            let task_analyzer_clone = task_analyzer.clone();
                            let ui_tx = self.ui_tx.clone();
                            let plan_manager = self.plan_manager.clone();

                            rt.spawn(async move {
                                match task_analyzer_clone.decompose(&classification, &task_desc).await {
                                    Ok(steps) => {
                                        if let Some(tx) = ui_tx {
                                            let _ = tx.send(format!("\n📝 実行計画 ({} ステップ):", steps.len()));

                                            for (i, step) in steps.iter().enumerate() {
                                                let step_icon = match step.step_type {
                                                    crate::planning::StepType::Analysis => "🔍",
                                                    crate::planning::StepType::Planning => "📋",
                                                    crate::planning::StepType::Implementation => "⚙️",
                                                    crate::planning::StepType::Validation => "✅",
                                                    crate::planning::StepType::Cleanup => "🧹",
                                                };

                                                let _ = tx.send(format!(
                                                    "  {}. {} {} ({}秒)",
                                                    i + 1,
                                                    step_icon,
                                                    step.description,
                                                    step.estimated_duration
                                                ));

                                                if !step.dependencies.is_empty() {
                                                    let _ = tx.send(format!("     依存: {}", step.dependencies.join(", ")));
                                                }

                                                if !step.required_tools.is_empty() {
                                                    let _ = tx.send(format!("     ツール: {}", step.required_tools.join(", ")));
                                                }
                                            }

                                            let plan = create_task_plan(
                                                task_desc,
                                                classification,
                                                steps,
                                            );

                                            // 計画を登録
                                            if let Ok(plan_manager) = plan_manager.lock() {
                                                match plan_manager.register_plan(plan.clone()) {
                                                    Ok(plan_id) => {
                                                        let _ = tx.send(format!("\n⏱️ 総推定時間: {}秒", plan.total_estimated_duration));
                                                        let _ = tx.send(format!("📋 計画ID: {}", plan_id));
                                                        let _ = tx.send("\n💡 実行方法:".to_string());
                                                        let _ = tx.send("   /execute        - 最新の計画を実行".to_string());
                                                        let _ = tx.send(format!("   /execute {}  - この計画を実行", plan_id));
                                                        let _ = tx.send("   または「この計画を実行して」等の指示".to_string());

                                                        info!("Generated and registered plan with ID: {}", plan_id);
                                                    }
                                                    Err(e) => {
                                                        let _ = tx.send(format!("❌ 計画の登録に失敗: {}", e));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        if let Some(tx) = ui_tx {
                                            let _ = tx.send(format!("❌ ステップ分解に失敗: {}", e));
                                        }
                                    }
                                }
                            });
                        }
                        Err(e) => {
                            ui.push_log(format!("❌ タスク分析に失敗: {}", e));
                        }
                    }
                    return;
                }

                // Handle /execute command for plan execution
                if let Some(rest) = line.strip_prefix("/execute") {
                    let plan_id = rest.trim();
                    ui.push_log(format!("> /execute {}", plan_id));
                    self.handle_execute_command(plan_id, ui);
                    return;
                }

                // Handle /plans command to list plans
                if line == "/plans" {
                    ui.push_log("> /plans");
                    self.handle_plans_command(ui);
                    return;
                }

                if !line.starts_with('/') {
                    let rest = line;
                    self.last_user_prompt = Some(rest.to_string());
                    ui.push_log(format!("> {rest}"));

                    // 計画実行の自動検出
                    let plan_to_execute = {
                        if let Ok(plan_manager) = self.plan_manager.lock() {
                            plan_manager.find_executable_plan(rest)
                        } else {
                            None
                        }
                    };

                    if let Some(plan_execution) = plan_to_execute {
                        ui.push_log(format!(
                            "🎯 実行可能な計画を検出: {}",
                            plan_execution.plan.original_request
                        ));
                        ui.push_log(format!("📋 計画ID: {}", plan_execution.plan.id));

                        // 計画実行を開始
                        let plan_id = plan_execution.plan.id.clone();
                        self.execute_plan_async(&plan_id, ui);
                        return;
                    }

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

impl TuiExecutor {
    /// Handle /execute command
    fn handle_execute_command(&mut self, plan_id: &str, ui: &mut TuiApp) {
        if plan_id.is_empty() {
            // 最新の計画を実行
            let latest_plan = {
                if let Ok(plan_manager) = self.plan_manager.lock() {
                    plan_manager.get_latest_plan()
                } else {
                    None
                }
            };

            if let Some(plan_execution) = latest_plan {
                let plan_id = plan_execution.plan.id.clone();
                ui.push_log(format!(
                    "🎯 最新の計画を実行: {}",
                    plan_execution.plan.original_request
                ));
                self.execute_plan_async(&plan_id, ui);
            } else {
                ui.push_log(
                    "❌ 実行可能な計画が見つかりません。まず /plan でタスクを分析してください。",
                );
            }
        } else {
            // 指定された計画を実行
            let plan_exists = {
                if let Ok(plan_manager) = self.plan_manager.lock() {
                    plan_manager.get_plan(plan_id).is_some()
                } else {
                    false
                }
            };

            if plan_exists {
                ui.push_log(format!("🎯 計画を実行: {}", plan_id));
                self.execute_plan_async(plan_id, ui);
            } else {
                ui.push_log(format!("❌ 計画ID '{}' が見つかりません。", plan_id));
            }
        }
    }

    /// Handle /plans command
    fn handle_plans_command(&mut self, ui: &mut TuiApp) {
        if let Ok(plan_manager) = self.plan_manager.lock() {
            let active_plans = plan_manager.list_active_plans();
            let recent_plans = plan_manager.get_recent_plans();
            let stats = plan_manager.get_statistics();

            ui.push_log("📊 計画統計:");
            ui.push_log(format!("   総計画数: {}", stats.total_plans));
            ui.push_log(format!("   アクティブ: {}", stats.active_plans));
            ui.push_log(format!("   完了: {}", stats.completed_plans));
            ui.push_log(format!("   失敗: {}", stats.failed_plans));
            ui.push_log(format!("   キャンセル: {}", stats.cancelled_plans));
            if stats.average_completion_time > 0.0 {
                ui.push_log(format!(
                    "   平均実行時間: {:.1}秒",
                    stats.average_completion_time
                ));
            }

            if !active_plans.is_empty() {
                ui.push_log("\n📋 アクティブな計画:");
                for plan_execution in &active_plans {
                    let status_icon = match plan_execution.status {
                        crate::planning::PlanStatus::Created => "⏳",
                        crate::planning::PlanStatus::Running => "🔄",
                        crate::planning::PlanStatus::Paused => "⏸️",
                        _ => "❓",
                    };
                    ui.push_log(format!(
                        "   {} {} - {} ({} ステップ)",
                        status_icon,
                        &plan_execution.plan.id[..8],
                        plan_execution.plan.original_request,
                        plan_execution.plan.steps.len()
                    ));
                }
            }

            if !recent_plans.is_empty() {
                ui.push_log("\n📚 最近の計画履歴:");
                for plan_execution in recent_plans.iter().rev().take(5) {
                    let status_icon = match plan_execution.status {
                        crate::planning::PlanStatus::Completed => "✅",
                        crate::planning::PlanStatus::Failed => "❌",
                        crate::planning::PlanStatus::Cancelled => "🚫",
                        _ => "❓",
                    };
                    ui.push_log(format!(
                        "   {} {} - {}",
                        status_icon,
                        &plan_execution.plan.id[..8],
                        plan_execution.plan.original_request
                    ));
                }
            }

            if active_plans.is_empty() && recent_plans.is_empty() {
                ui.push_log("📝 計画がありません。/plan <タスク> で新しい計画を作成してください。");
            }
        } else {
            ui.push_log("❌ 計画管理システムにアクセスできません。");
        }
    }

    /// Execute plan asynchronously
    fn execute_plan_async(&mut self, plan_id: &str, ui: &mut TuiApp) {
        if self.client.is_none() {
            ui.push_log("❌ LLMクライアントが設定されていません。");
            return;
        }

        let plan_execution = {
            if let Ok(plan_manager) = self.plan_manager.lock() {
                plan_manager.get_plan(plan_id)
            } else {
                ui.push_log("❌ 計画管理システムにアクセスできません。");
                return;
            }
        };

        let Some(plan_execution) = plan_execution else {
            ui.push_log(format!("❌ 計画ID '{}' が見つかりません。", plan_id));
            return;
        };

        // 実行開始
        if let Ok(plan_manager) = self.plan_manager.lock()
            && let Err(e) = plan_manager.start_execution(plan_id)
        {
            ui.push_log(format!("❌ 計画実行の開始に失敗: {}", e));
            return;
        }

        ui.push_log("🚀 計画実行を開始します...");

        let rt = tokio::runtime::Handle::current();
        let client = self.client.as_ref().unwrap().clone();
        let model = self.cfg.model.clone();
        let fs_tools = self.tools.clone();
        let ui_tx = self.ui_tx.clone();
        let plan_manager = self.plan_manager.clone();
        let plan_id = plan_id.to_string();
        let plan = plan_execution.plan;

        rt.spawn(async move {
            let executor = crate::planning::TaskExecutor::new(client, model, fs_tools);

            match executor.execute_plan(plan, ui_tx.clone()).await {
                Ok(result) => {
                    if let Ok(pm) = plan_manager.lock() {
                        let _ = pm.complete_execution(&plan_id, result.clone());
                    }

                    if let Some(tx) = ui_tx {
                        if result.success {
                            let _ = tx.send("🎉 計画実行が正常に完了しました！".to_string());
                        } else {
                            let _ = tx.send(format!(
                                "⚠️ 計画実行が部分的に完了: {}",
                                result.final_message
                            ));
                        }
                        let _ = tx.send(format!("📊 実行時間: {}秒", result.total_duration));
                        let _ = tx.send(format!(
                            "✅ 完了ステップ: {}/{}",
                            result.completed_steps.len(),
                            result.completed_steps.len()
                        ));
                    }
                }
                Err(e) => {
                    if let Ok(pm) = plan_manager.lock() {
                        let _ = pm.cancel_execution(&plan_id);
                    }

                    if let Some(tx) = ui_tx {
                        let _ = tx.send(format!("❌ 計画実行に失敗: {}", e));
                    }
                }
            }
        });
    }
}
