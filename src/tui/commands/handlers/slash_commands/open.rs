use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;
use crossterm::{cursor, execute, terminal};
use std::env;
use std::process;

/// Delegate /open to the dedicated handler.
/// This separation improves modularity by isolating command logic.
pub fn handle_open(executor: &mut TuiExecutor, line: &str, ui: &mut TuiApp) {
    let rest = line.strip_prefix("/open ").map(|s| s.trim()).unwrap_or("");
    if rest.is_empty() {
        ui.push_log("usage: /open <path>");
        return;
    }
    // Resolve to absolute path; allow project-internal paths and absolute paths
    let p = std::path::Path::new(rest);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        executor.cfg.project_root.join(p)
    };
    if !abs.exists() {
        ui.push_log(format!("not found: {}", abs.display()));
        return;
    }
    // Leave TUI alt screen temporarily while spawning editor in blocking mode
    let mut stdout = std::io::stdout();
    let _ = execute!(stdout, terminal::LeaveAlternateScreen, cursor::Show);
    let _ = terminal::disable_raw_mode();

    // Choose editor from $EDITOR, then $VISUAL, else fallback list
    let editor = env::var("EDITOR")
        .ok()
        .or_else(|| env::var("VISUAL").ok())
        .unwrap_or_else(|| "vi".to_string());
    let status = process::Command::new(&editor).arg(&abs).status();

    // Re-enter TUI
    let _ = terminal::enable_raw_mode();
    let _ = execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide);

    match status {
        Ok(s) if s.success() => ui.push_log(format!("opened: {}", abs.display())),
        Ok(s) => ui.push_log(format!("editor exited with status {s}")),
        Err(e) => ui.push_log(format!("failed to launch editor: {e}")),
    }
}
