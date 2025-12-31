use crate::config::AppConfig;
use crate::tools::plan::{plan_approve, plan_read};
use crate::tui::view::TuiApp;
use anyhow::Result;

pub fn handle_plan(
    input: &str,
    session_id: &str,
    ui: &mut TuiApp,
    config: &AppConfig,
) -> Result<()> {
    // /plan approve
    let args: Vec<&str> = input.split_whitespace().collect();
    if args.is_empty() {
        ui.push_log("Usage: /plan <show|approve>");
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

            let status_indicator = if plan.approved {
                "✓ 承認済み"
            } else {
                "⚠ 未承認"
            };

            ui.push_log(format!("--- 現在の計画 ({}) ---", status_indicator));
            for item in &plan.items {
                let status_symbol = match item.status.as_str() {
                    "pending" => "◌",
                    "in_progress" => "◔",
                    "completed" => "✓",
                    _ => "○",
                };
                ui.push_log(format!("{} [{}] {}", status_symbol, item.id, item.content));
            }
            ui.push_log("----------------------------");

            if !plan.approved {
                ui.push_log("[plan] /plan approve で計画を承認できます。");
            }
        }
        "approve" => {
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
            ui.push_log("Usage: /plan <show|approve>");
        }
    }

    Ok(())
}
