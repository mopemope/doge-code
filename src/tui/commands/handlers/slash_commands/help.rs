use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;

/// Delegate /help to the dedicated handler.
/// This separation improves modularity by isolating command logic.
pub fn handle_help(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    ui.push_log("Available commands:");
    ui.push_log("  /help - Show this help message");
    ui.push_log("  /quit - Exit the application");
    ui.push_log("  /clear - Clear the conversation and log area");
    ui.push_log("  /open <path> - Open a file in your editor");
    ui.push_log("  /theme <name> - Switch theme (dark/light)");
    ui.push_log("  /tools - List available tools");
    ui.push_log("  /tokens - Show token usage");
    ui.push_log("  /compact - Compact conversation history to reduce token usage");
    ui.push_log("  /cancel - Cancel the current operation");
    ui.push_log("  /lint - Run linting tools for Go, Rust, and TypeScript");
    ui.push_log("");

    ui.push_log("Repository Analysis:");
    ui.push_log("  /map - Show repository analysis summary");
    ui.push_log("  /edit-symbol - Invoke symbol-scoped LLM edit and preview via the diff review");
    ui.push_log("  /rebuild-repomap - Rebuild repository analysis");
    ui.push_log("");

    ui.push_log("Session Management:");
    ui.push_log("  /session <new|list|switch|save|delete|current|clear> - Manage sessions");

    ui.push_log("");

    // Display custom commands
    ui.push_log("Custom commands:");
    executor.display_custom_commands_help(ui);

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
