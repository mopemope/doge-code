use crate::session::format as session_format;
use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use anyhow::Result;

impl TuiExecutor {
    /// Load a session by (possibly partial) ID without switching to it.
    fn load_session_by_prefix(
        &self,
        id: &str,
    ) -> Result<crate::session::SessionData, anyhow::Error> {
        let manager = self.session_manager.lock().unwrap();
        let full_id = manager.store.resolve_id_prefix(id)?;
        manager.store.load(&full_id).map_err(|e| anyhow::anyhow!(e))
    }

    /// Get the current session's full ID (for marking in listings).
    fn current_session_id(&self) -> Option<String> {
        self.session_manager.lock().unwrap().current_session_id()
    }

    pub(crate) fn handle_session_command(&mut self, args: &str, ui: &mut TuiApp) -> Result<()> {
        let args: Vec<&str> = args.split_whitespace().collect();
        if args.is_empty() {
            ui.push_log("Usage: /session <new|list|show|switch|save|delete|current|clear>");
            return Ok(());
        }

        match args[0] {
            "list" => {
                let current_id = self.current_session_id();
                match self.session_manager.lock().unwrap().store.list_with_stats() {
                    Ok(summaries) => {
                        ui.push_log(session_format::format_summary_list(
                            &summaries,
                            current_id.as_deref(),
                        ));
                    }
                    Err(e) => ui.push_log(format!("Failed to list sessions: {}", e)),
                }
            }
            "show" => {
                if args.len() != 2 {
                    ui.push_log("Usage: /session show <id>");
                    return Ok(());
                }
                let id = args[1];
                match self.load_session_by_prefix(id) {
                    Ok(data) => ui.push_log(session_format::format_detail(&data)),
                    Err(e) => ui.push_log(format!("Failed to show session: {}", e)),
                }
            }
            "new" => {
                // Allow optional title argument: /session new <title words...>
                let initial_prompt = if args.len() > 1 {
                    Some(args[1..].join(" "))
                } else {
                    None
                };
                match self
                    .session_manager
                    .lock()
                    .unwrap()
                    .create_session(initial_prompt)
                {
                    Ok(()) => {
                        if let Some(info) =
                            self.session_manager.lock().unwrap().current_session_info()
                        {
                            ui.push_log(format!("Created new session:\n{}", info));
                        }
                        self.publish_plan_list();
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
                let session_after_switch = {
                    let mut session_manager = self.session_manager.lock().unwrap();
                    match session_manager.resolve_and_load_session(id) {
                        Ok(session) => session,
                        Err(e) => {
                            ui.push_log(format!("Failed to switch session: {}", e));
                            return Ok(());
                        }
                    }
                };

                ui.clear_log();
                ui.push_log(format!(
                    "Switched to session: {} ({})",
                    session_format::short_id(&session_after_switch.meta.id),
                    session_after_switch.meta.title
                ));

                {
                    let mut history = self.conversation_history.lock().unwrap();
                    history.clear();
                    for entry in &session_after_switch.conversation {
                        let map: serde_json::Map<_, _> = entry.clone().into_iter().collect();
                        if let Ok(msg) = serde_json::from_value::<crate::llm::types::ChatMessage>(
                            serde_json::Value::Object(map),
                        ) {
                            history.append_message(msg);
                        }
                    }
                }

                self.publish_plan_list();
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
                let resolved = {
                    match self
                        .session_manager
                        .lock()
                        .unwrap()
                        .store
                        .resolve_id_prefix(id)
                    {
                        Ok(full_id) => full_id,
                        Err(e) => {
                            ui.push_log(format!("Failed to delete session: {}", e));
                            return Ok(());
                        }
                    }
                };
                match self
                    .session_manager
                    .lock()
                    .unwrap()
                    .delete_session(&resolved)
                {
                    Ok(()) => {
                        ui.push_log(format!(
                            "Deleted session: {}",
                            session_format::short_id(&resolved)
                        ));
                        self.publish_plan_list();
                    }
                    Err(e) => ui.push_log(format!("Failed to delete session: {}", e)),
                }
            }
            "current" => {
                if let Some(info) = self.session_manager.lock().unwrap().current_session_info() {
                    ui.push_log(info);
                } else {
                    ui.push_log("No session loaded.");
                }
            }
            "clear" => match self
                .session_manager
                .lock()
                .unwrap()
                .clear_current_session_conversation()
            {
                Ok(()) => ui.push_log("Cleared current session conversation."),
                Err(e) => ui.push_log(format!("Failed to clear session conversation: {}", e)),
            },
            _ => {
                ui.push_log("Unknown session command. Usage: /session <new|list|show|switch|save|delete|current|clear>");
            }
        }
        Ok(())
    }
}
