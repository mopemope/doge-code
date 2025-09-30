use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;

/// Delegate /tools to the dedicated handler.
/// This separation improves modularity by isolating command logic.
pub fn handle_tools(_executor: &mut TuiExecutor, ui: &mut TuiApp) {
    ui.push_log("Available tools for the LLM:");
    ui.push_log("  🗂️ fs_list: List files and directories");
    ui.push_log("  📖 fs_read: Read a file");
    ui.push_log("  📚 fs_read_many_files: Read multiple files");
    ui.push_log("  📝 fs_write: Write to a file");
    ui.push_log("  🔍 search_text: Search for text in files");
    ui.push_log("  🔧 execute_bash: Execute a shell command in the project root directory");
    ui.push_log("  📁 find_file: Find a file by name or pattern");
    ui.push_log("  🗺️ search_repomap: Search the repomap with specific criteria");
    ui.push_log("  ✏️ edit: Edit a single unique block of text within a file");
    ui.push_log("  🧩 apply_patch: Apply a unified diff patch to a file");
    ui.push_log("  📋 todo_write: Create and manage a structured task list");
    ui.push_log("  📋 todo_read: Read the todo list for the current session");
}
