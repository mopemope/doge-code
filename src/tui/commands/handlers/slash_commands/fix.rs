use std::path::PathBuf;

use crate::llm::{
    EditTarget, SymbolEditRequest, build_symbol_edit_chat_request, read_target_source,
};
use crate::tui::commands::core::TuiExecutor;
use crate::tui::commands::handlers::slash_commands::edit_symbol::enqueue_symbol_edit_request;
use crate::tui::view::TuiApp;

/// Handle /fix command
/// Usage: /fix <file>:<start>-<end> <instruction>
pub fn handle_fix(executor: &mut TuiExecutor, ui: &mut TuiApp, args: &str) {
    if args.trim().is_empty() {
        ui.push_log("Usage: /fix <file>:<start_line>-<end_line> <instruction>");
        return;
    }

    // Parse arguments: path_range and instruction
    let (path_range, instruction) = match args.split_once(' ') {
        Some((pr, inst)) => (pr.trim(), inst.trim()),
        None => {
            ui.push_log("Please provide an instruction.");
            return;
        }
    };

    if instruction.is_empty() {
        ui.push_log("Instruction cannot be empty.");
        return;
    }

    // Parse path and range
    let (path_str, range_str) = match path_range.rsplit_once(':') {
        Some((p, r)) => (p, r),
        None => {
            ui.push_log("Invalid format. Use <file>:<start>-<end>");
            return;
        }
    };

    let (start_str, end_str) = match range_str.split_once('-') {
        Some((s, e)) => (s, e),
        None => {
            ui.push_log("Invalid range format. Use <start>-<end> (e.g. 10-20)");
            return;
        }
    };

    let start_line = match start_str.parse::<u32>() {
        Ok(n) => n,
        Err(_) => {
            ui.push_log(format!("Invalid start line: {}", start_str));
            return;
        }
    };

    let end_line = match end_str.parse::<u32>() {
        Ok(n) => n,
        Err(_) => {
            ui.push_log(format!("Invalid end line: {}", end_str));
            return;
        }
    };

    if start_line > end_line {
        ui.push_log("Start line cannot be greater than end line.");
        return;
    }

    // Resolve path relative to project root (or absolute)
    let path = PathBuf::from(path_str);
    let resolved_path = if path.is_absolute() {
        path.clone()
    } else {
        executor.cfg.project_root.join(&path)
    };

    if !resolved_path.exists() {
        ui.push_log(format!("File not found: {}", resolved_path.display()));
        return;
    }

    // Read original code
    let original = match read_target_source(&resolved_path, start_line, end_line) {
        Ok(code) => code,
        Err(e) => {
            ui.push_log(format!("Failed to read target source: {e}"));
            return;
        }
    };

    // Initialize UI channel if needed
    if executor.ui_tx.is_none() {
        executor.ui_tx = ui.sender();
    }

    // Build Request
    let model = executor.cfg.model.clone();
    let req = SymbolEditRequest {
        model,
        target: EditTarget {
            file: path, // Keep relative or original path for display/logic if needed
            start_line,
            end_line,
            name: None,
            kind: "lines".to_string(),
        },
        original_code: original,
        instruction: instruction.to_string(),
    };

    ui.push_log(format!(
        "[fix] Targeting {} lines {}-{}: {}",
        req.target.file.display(),
        req.target.start_line,
        req.target.end_line,
        instruction
    ));

    let chat_req = build_symbol_edit_chat_request(&req);

    if let Err(e) = enqueue_symbol_edit_request(executor, req, chat_req) {
        ui.push_log(format!("Failed to enqueue fix request: {e}"));
    } else {
        ui.push_log("Fix request enqueued.");
    }
}
