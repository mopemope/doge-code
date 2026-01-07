use crate::config::AppConfig;
use crate::tools::plan::plan_read;
use crate::tui::view::TuiApp;
use anyhow::Result;

pub fn handle_plan(
    input: &str,
    session_id: &str,
    ui: &mut TuiApp,
    config: &AppConfig,
) -> Result<()> {
    // /plan show
    let args: Vec<&str> = input.split_whitespace().collect();
    if args.is_empty() {
        ui.push_log("Usage: /plan show");
        return Ok(());
    }

    match args[0] {
        "show" => {
            // Show current plan
            let plan = plan_read(session_id, config)?;

            if plan.items.is_empty() {
                ui.push_log("[plan] 現在の計画はありません。");
                return Ok(());
            }

            ui.push_log("--- 現在の計画 ---");
            for item in &plan.items {
                let status_symbol = match item.status.as_str() {
                    "pending" => "◌",
                    "in_progress" => "◔",
                    "completed" => "✓",
                    _ => "○",
                };
                ui.push_log(format!("{} [{}] {}", status_symbol, item.id, item.content));
            }
            ui.push_log("-------------------");
        }
        _ => {
            ui.push_log(format!("Unknown subcommand: {}", args[0]));
            ui.push_log("Usage: /plan show");
        }
    }

    Ok(())
}
