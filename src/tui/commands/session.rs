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
            ui.push_log("Usage: /session <new|list|show|switch|save|export|delete|current|clear>");
            return Ok(());
        }

        if matches!(
            args[0],
            "new" | "switch" | "delete" | "clear" | "save" | "export"
        ) {
            self.ensure_session_idle()?;
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
                match self.start_new_session(ui, initial_prompt) {
                    Ok(_) => {
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
                match self.switch_to_session(id) {
                    Ok(session_after_switch) => {
                        self.reset_session_input(ui);
                        ui.clear_log();
                        ui.push_log(format!(
                            "Switched to session: {} ({})",
                            session_format::short_id(&session_after_switch.meta.id),
                            session_after_switch.meta.title
                        ));
                        self.publish_plan_list();
                    }
                    Err(e) => {
                        ui.push_log(format!("Failed to switch session: {}", e));
                    }
                }
            }
            "save" => {
                if self.current_session_id().is_none() {
                    ui.push_log("No session loaded; nothing to save.");
                } else {
                    match self.flush_session() {
                Ok(crate::session::store::SessionSaveOutcome::Durable) => {
                    ui.push_log("Session checkpoint saved.")
                }
                Ok(crate::session::store::SessionSaveOutcome::DurabilityUnconfirmed {
                    message,
                }) => ui.push_log(format!("Warning: {message}")),
                Err(error) => ui.push_log(format!(
                    "Session remains unsaved: {error}. Fix the cause, then retry /session save."
                )),
            }
                }
            }
            "delete" => {
                if args.len() != 2 {
                    ui.push_log("Usage: /session delete <id>");
                    return Ok(());
                }
                match self.delete_runtime_session(ui, args[1]) {
                    Ok(resolved) => ui.push_log(format!(
                        "Deleted session: {}",
                        session_format::short_id(&resolved)
                    )),
                    Err(e) => ui.push_log(format!("Failed to delete session: {}", e)),
                }
            }

            "export" => {
                if args.len() != 1 {
                    ui.push_log("Usage: /session export (no path or other arguments)");
                } else {
                    let (result, unsaved) = {
                        let manager = self.session_manager.lock().unwrap();
                        (
                            manager.export_current_session(),
                            manager.has_unsaved_current_session(),
                        )
                    };
                    match result {
                        Ok(export) => {
                            let status = if unsaved {
                                "Normal checkpoint remains unsaved"
                            } else {
                                "Normal checkpoint status is unchanged"
                            };
                            ui.push_log(format!("Recovery exported: {} ({} bytes). {status}; export does not clear unsaved changes and is not a resumable session.", export.path.display(), export.bytes));
                            if let Some(warning) = export.durability_warning {
                                ui.push_log(format!("Warning: {warning}"));
                            }
                        }
                        Err(error) => ui.push_log(format!("Failed to export recovery: {error:#}")),
                    }
                }
            }
            "current" => {
                if let Some(info) = self.session_manager.lock().unwrap().current_session_info() {
                    ui.push_log(info);
                } else {
                    ui.push_log("No session loaded.");
                }
            }
            "clear" => match self.clear_runtime_conversation() {
                Ok(outcome) => {
                    self.reset_session_input(ui);
                    ui.push_log("Cleared current session conversation.");
                    if let crate::session::store::SessionSaveOutcome::DurabilityUnconfirmed {
                        message,
                    } = outcome
                    {
                        ui.push_log(format!("Warning: {message}"));
                    }
                }
                Err(e) => ui.push_log(format!("Failed to clear session conversation: {}", e)),
            },
            _ => {
                ui.push_log("Unknown session command. Usage: /session <new|list|show|switch|save|export|delete|current|clear>");
            }
        }
        Ok(())
    }
}
