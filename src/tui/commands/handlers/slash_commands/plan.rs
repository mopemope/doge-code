use crate::config::AppConfig;
use crate::tools::plan::{plan_approve, plan_read};
use crate::tui::view::TuiApp;
use anyhow::Result;

pub fn handle_plan(
    input: &str,
    session_id: &str,
    _config: &AppConfig,
    ui: &mut TuiApp,
    config: &AppConfig,
) -> Result<()> {
    // /plan approve
    let args: Vec<&str> = input.split_whitespace().collect();
    if args.is_empty() {
        ui.push_log("Usage: /plan <approve>");
        return Ok(());
    }

    match args[0] {
        "approve" => {
            // Check if plan exists
            // Check if plan exists
            let plan = plan_read(session_id, config)?;

            if plan.items.is_empty() {
                ui.push_log("[plan] 承認する計画がありません。まずは計画を作成してください。");
                return Ok(());
            }

            if plan.approved {
                ui.push_log("[plan] この計画は既に承認されています。");
                return Ok(());
            }

            match plan_approve(session_id, config) {
                Ok(_) => {
                    ui.push_log("[plan] 計画を承認しました。実装フェーズに進めます。");
                }
                Err(e) => {
                    ui.push_log(format!("[Error] 計画の承認に失敗しました: {}", e));
                }
            }
        }
        _ => {
            ui.push_log(format!("Unknown subcommand: {}", args[0]));
            ui.push_log("Usage: /plan <approve>");
        }
    }

    Ok(())
}
