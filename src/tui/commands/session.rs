use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use anyhow::Result;

impl TuiExecutor {
    pub(crate) fn handle_session_command(&mut self, args: &str, ui: &mut TuiApp) -> Result<()> {
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
                        // Enter session list mode with the sessions
                        ui.enter_session_list_mode(sessions);
                    }
                }
                Err(e) => ui.push_log(format!("Failed to list sessions: {}", e)),
            },
            "new" => {
                // Allow optional title argument: /session new <title words...>
                let initial_prompt = if args.len() > 1 {
                    Some(args[1..].join(" "))
                } else {
                    None
                };
                match session_manager.create_session(initial_prompt) {
                    Ok(()) => {
                        if let Some(info) = (*session_manager).current_session_info() {
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
                        // Clear the TUI display
                        ui.clear_log();
                        ui.push_log(format!("Switched to session: {}", id));

                        // Load conversation history from session
                        if let (Some(session), Ok(mut history)) = (
                            &session_manager.current_session,
                            self.conversation_history.lock(),
                        ) {
                            history.clear();
                            // Deserialize session conversation entries into ChatMessage objects
                            for entry in &session.conversation {
                                let map: serde_json::Map<_, _> =
                                    entry.clone().into_iter().collect();
                                if let Ok(msg) =
                                    serde_json::from_value::<crate::llm::types::ChatMessage>(
                                        serde_json::Value::Object(map),
                                    )
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
                    Ok(()) => {
                        ui.push_log(format!("Deleted session: {}", id));
                        // If we're in session list mode, refresh the list
                        if ui.input_mode == crate::tui::state::InputMode::SessionList {
                            match session_manager.list_sessions() {
                                Ok(sessions) => {
                                    if sessions.is_empty() {
                                        // Exit session list mode if no sessions left
                                        ui.input_mode = crate::tui::state::InputMode::Normal;
                                        ui.session_list_state = None;
                                        ui.push_log("No more sessions available.");
                                    } else {
                                        // Update the session list
                                        if let Some(ref mut session_list_state) =
                                            ui.session_list_state
                                        {
                                            session_list_state.sessions = sessions;
                                            // Make sure selected index is within bounds
                                            if session_list_state.selected_index
                                                >= session_list_state.sessions.len()
                                            {
                                                session_list_state.selected_index =
                                                    session_list_state
                                                        .sessions
                                                        .len()
                                                        .saturating_sub(1);
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    ui.push_log(format!("Failed to refresh session list: {}", e))
                                }
                            }
                        }
                    }
                    Err(e) => ui.push_log(format!("Failed to delete session: {}", e)),
                }
            }
            "current" => {
                if let Some(info) = (*session_manager).current_session_info() {
                    ui.push_log(info);
                } else {
                    ui.push_log("No session loaded.");
                }
            }
            "clear" => match session_manager.clear_current_session_conversation() {
                Ok(()) => ui.push_log("Cleared current session conversation."),
                Err(e) => ui.push_log(format!("Failed to clear session conversation: {}", e)),
            },
            _ => {
                ui.push_log("Unknown session command. Usage: /session <new|list|switch|save|delete|current|clear>");
            }
        }
        Ok(())
    }
}
