use crate::tui::view::TuiApp;

use crate::tui::commands::core::TuiExecutor;

impl TuiExecutor {
    pub fn handle_dispatch_rest(&mut self, line: &str, ui: &mut TuiApp, skip_plan: bool) {
        if let Some(rest) = line.strip_prefix("/session ") {
            match self.handle_session_command(rest.trim(), ui) {
                Ok(_) => {} // No-op on success
                Err(e) => ui.push_log(format!("Error handling session command: {}", e)),
            }
            return;
        }

        if !line.starts_with('/') {
            let content = line.to_string();
            // `spawn_agent_turn` owns session creation, history updates, and
            // the foreground reservation so busy rejections stay clean.
            let _ = crate::tui::commands::agent_job::spawn_agent_turn(
                self, ui, line, content, skip_plan,
            );
        } else {
            self.handle_custom_command(line, ui);
        }
    }
}
