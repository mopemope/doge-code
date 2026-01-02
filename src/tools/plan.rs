use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::debug;

const DESCRIPTION: &str = r#"
Use this tool to create or update the execution plan for the current session.
Always draft concrete, ordered steps before modifying code, and rewrite the
plan as scope evolves.

Guidelines:
- Aim for at least three actionable steps (pending by default)
- IDs must remain stable and unique so progress can be tracked over time
- Keep only one item in_progress at a time; mark completed immediately after finishing
- Do not delete history mid-session; instead, append or update statuses via merge
- Describe concrete actions and expected outcomes (e.g., tests to run, files to touch)

Hard requirements (automatically enforced):
- Provide at least one non-empty step
- Use unique IDs per step
- Use only pending/in_progress/completed statuses, with at most one in_progress
"#;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanItem {
    pub id: String,
    pub content: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanList {
    pub session_id: Option<String>,
    pub items: Vec<PlanItem>,
    #[serde(default)]
    pub approved: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanWriteMode {
    /// Replace the entire plan with the provided items (default)
    #[default]
    Replace,
    /// Merge items by `id`, updating existing ones and appending new entries
    Merge,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanWriteArgs {
    pub items: Vec<PlanItem>,
    #[serde(default)]
    pub mode: PlanWriteMode,
}

pub fn plan_write_tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "plan_write".to_string(),
            description: DESCRIPTION.to_string(),
            strict: Some(true),
            parameters: json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string"},
                                "content": {"type": "string", "minLength": 1},
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"],
                                },
                            },
                            "required": ["id", "content", "status"],
                            "additionalProperties": false,
                        }
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["replace", "merge"],
                        "default": "replace"
                    }
                },
                "required": ["items"],
                "additionalProperties": false,
            }),
        },
    }
}

pub fn plan_read_tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "plan_read".to_string(),
            description:
                "Use this tool to fetch the current execution plan (if any) for the active session. Call it before making changes or when resuming work to stay aligned with the plan.".to_string(),
            strict: Some(true),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
        },
    }
}

pub fn plan_approve_tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "plan_approve".to_string(),
            description: "Call this tool AFTER the user has explicitly approved the current plan. This marks the plan as approved and allows implementation to proceed.".to_string(),
            strict: Some(true),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
        },
    }
}

pub fn plan_write(
    items: Vec<PlanItem>,
    mode: PlanWriteMode,
    session_id: &str,
    config: &AppConfig,
) -> Result<PlanList> {
    plan_write_from_base_path(items, mode, session_id, &config.project_root, config)
}

pub fn plan_read(session_id: &str, config: &AppConfig) -> Result<PlanList> {
    plan_read_from_base_path(session_id, &config.project_root, config)
}

pub fn plan_approve(session_id: &str, config: &AppConfig) -> Result<PlanList> {
    let mut list = plan_read(session_id, config)?;
    list.approved = true;

    // Save back
    let base = &config.project_root;
    let plan_file_path = plan_file_path(base, session_id);
    let json_content = serde_json::to_string_pretty(&list)
        .with_context(|| "Failed to serialize plan list to JSON")?;
    fs::write(&plan_file_path, &json_content)
        .with_context(|| format!("Failed to write plan file: {}", plan_file_path.display()))?;

    Ok(list)
}

pub fn ensure_plan_is_approved(session_id: &str, config: &AppConfig) -> Result<()> {
    let list = plan_read(session_id, config)?;
    if list.items.is_empty() {
        bail!(
            "現在のセッションには計画(Plan)が存在しません。`plan_write` ツールを使用して計画を作成し、`/plan approve` で承認を得てから実装に進んでください。"
        );
    }
    if !list.approved {
        bail!(
            "現在の計画(Plan)はまだ承認されていません。ユーザーに計画を提示し、`/plan approve` コマンドで承認を得てから実装に進んでください。"
        );
    }
    Ok(())
}

pub fn plan_write_from_base_path(
    items: Vec<PlanItem>,
    mode: PlanWriteMode,
    session_id: &str,
    base_path: impl AsRef<Path>,
    _config: &AppConfig,
) -> Result<PlanList> {
    let base = base_path.as_ref();
    let plan_dir = plans_dir(base);
    fs::create_dir_all(&plan_dir)
        .with_context(|| format!("Failed to create plan directory: {}", plan_dir.display()))?;

    let plan_file_path = plan_file_path(base, session_id);
    debug!(?plan_file_path, "write plans");

    let new_items = match (
        mode,
        plan_read_from_path(&plan_file_path),
        legacy_plan_read(base, session_id),
    ) {
        (PlanWriteMode::Replace, _, _) => items,
        (PlanWriteMode::Merge, Ok(existing), _) => merge_items(existing.items, items),
        (PlanWriteMode::Merge, Err(_), Ok(legacy)) => merge_items(legacy.items, items),
        (PlanWriteMode::Merge, Err(_), Err(_)) => items,
    };

    let plan_list = PlanList {
        session_id: Some(session_id.to_string()),
        items: new_items,
        approved: false, // Reset approval on any write
    };

    validate_plan_items(&plan_list.items)?;

    let json_content = serde_json::to_string_pretty(&plan_list)
        .with_context(|| "Failed to serialize plan list to JSON")?;
    fs::write(&plan_file_path, &json_content)
        .with_context(|| format!("Failed to write plan file: {}", plan_file_path.display()))?;

    Ok(plan_list)
}

pub fn plan_read_from_base_path(
    session_id: &str,
    base_path: impl AsRef<Path>,
    _config: &AppConfig,
) -> Result<PlanList> {
    let base = base_path.as_ref();
    let primary_path = plan_file_path(base, session_id);
    if primary_path.exists() {
        return plan_read_from_path(&primary_path);
    }

    let legacy_path = legacy_plan_file_path(base, session_id);
    if legacy_path.exists() {
        return plan_read_from_path(&legacy_path);
    }

    Ok(PlanList {
        session_id: Some(session_id.to_string()),
        items: vec![],
        approved: false,
    })
}

fn plan_read_from_path(path: &Path) -> Result<PlanList> {
    let json_content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read plan file: {}", path.display()))?;
    let list: PlanList = serde_json::from_str(&json_content)
        .with_context(|| format!("Failed to parse plan file: {}", path.display()))?;
    Ok(list)
}

fn merge_items(mut existing: Vec<PlanItem>, updates: Vec<PlanItem>) -> Vec<PlanItem> {
    for item in updates {
        if let Some(slot) = existing.iter_mut().find(|p| p.id == item.id) {
            *slot = item;
        } else {
            existing.push(item);
        }
    }
    existing
}

fn plans_dir(base_path: &Path) -> PathBuf {
    base_path.join(".doge").join("plans")
}

fn plan_file_path(base_path: &Path, session_id: &str) -> PathBuf {
    plans_dir(base_path).join(format!("{}.json", session_id))
}

fn legacy_plan_file_path(base_path: &Path, session_id: &str) -> PathBuf {
    base_path
        .join(".doge")
        .join("todos")
        .join(format!("{}.json", session_id))
}

fn legacy_plan_read(base_path: &Path, session_id: &str) -> Result<PlanList> {
    let path = legacy_plan_file_path(base_path, session_id);
    plan_read_from_path(&path)
}

pub fn format_plan_summary(items: &[PlanItem]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut lines = Vec::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        let status_symbol = match item.status.as_str() {
            "pending" => "◌",
            "in_progress" => "◔",
            "completed" => "✓",
            other => other,
        };
        lines.push(format!(
            "{}. [{}] {}",
            idx + 1,
            status_symbol,
            item.content.trim()
        ));
    }
    Some(lines.join("\n"))
}

fn validate_plan_items(items: &[PlanItem]) -> Result<()> {
    if items.is_empty() {
        anyhow::bail!(
            "Plan must contain at least one step. Provide pending steps instead of clearing the plan."
        );
    }

    let mut seen_ids = HashSet::new();
    let mut in_progress_count = 0u32;

    for item in items {
        if !seen_ids.insert(item.id.clone()) {
            anyhow::bail!("Duplicate plan item id detected: {}", item.id);
        }

        let trimmed = item.content.trim();
        if trimmed.is_empty() {
            anyhow::bail!(
                "Plan item '{}' must include a non-empty description.",
                item.id
            );
        }

        match item.status.as_str() {
            "pending" | "completed" => {}
            "in_progress" => in_progress_count += 1,
            other => anyhow::bail!("Invalid status '{}' for plan item {}", other, item.id),
        }
    }

    if in_progress_count > 1 {
        anyhow::bail!(
            "Only one plan item may be marked in_progress at a time (found {}).",
            in_progress_count
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn plan_dir() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempdir().unwrap();
        let base = dir.path().to_path_buf();
        (dir, base)
    }

    #[test]
    fn plan_write_accepts_valid_plan() {
        let (_dir, base) = plan_dir();
        let items = vec![
            PlanItem {
                id: "step-1".into(),
                content: "Review requirements and clarify scope".into(),
                status: "pending".into(),
            },
            PlanItem {
                id: "step-2".into(),
                content: "Implement feature across modules".into(),
                status: "pending".into(),
            },
            PlanItem {
                id: "step-3".into(),
                content: "Run tests and verify results".into(),
                status: "pending".into(),
            },
        ];
        let result = plan_write_from_base_path(
            items,
            PlanWriteMode::Replace,
            "session",
            base,
            &AppConfig::default(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn plan_write_rejects_duplicate_ids() {
        let (_dir, base) = plan_dir();
        let items = vec![
            PlanItem {
                id: "step-1".into(),
                content: "Do something".into(),
                status: "pending".into(),
            },
            PlanItem {
                id: "step-1".into(),
                content: "Do another".into(),
                status: "pending".into(),
            },
        ];
        let result = plan_write_from_base_path(
            items,
            PlanWriteMode::Replace,
            "session",
            base,
            &AppConfig::default(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn plan_write_rejects_multiple_in_progress() {
        let (_dir, base) = plan_dir();
        let items = vec![
            PlanItem {
                id: "step-1".into(),
                content: "Work item".into(),
                status: "in_progress".into(),
            },
            PlanItem {
                id: "step-2".into(),
                content: "Another".into(),
                status: "in_progress".into(),
            },
        ];
        let result = plan_write_from_base_path(
            items,
            PlanWriteMode::Replace,
            "session",
            base,
            &AppConfig::default(),
        );
        assert!(result.is_err());
    }
    #[test]
    fn plan_write_resets_approval() {
        let (_dir, base) = plan_dir();
        let config = AppConfig::default();
        let session_id = "approval_test";

        // 1. Write initial plan
        let items = vec![PlanItem {
            id: "step-1".into(),
            content: "Task".into(),
            status: "pending".into(),
        }];
        plan_write_from_base_path(
            items.clone(),
            PlanWriteMode::Replace,
            session_id,
            &base,
            &config,
        )
        .unwrap();

        // 2. Approve it - verify manual approval works (simulating plan_approve)
        let mut list = plan_read_from_base_path(session_id, &base, &config).unwrap();
        list.approved = true;
        let path = plan_file_path(&base, session_id);
        fs::write(&path, serde_json::to_string(&list).unwrap()).unwrap();

        // Verify it is approved
        let loaded = plan_read_from_base_path(session_id, &base, &config).unwrap();
        assert!(loaded.approved);

        // 3. Update plan via plan_write
        let new_items = vec![PlanItem {
            id: "step-1".into(),
            content: "Task Updated".into(),
            status: "pending".into(),
        }];
        let updated = plan_write_from_base_path(
            new_items,
            PlanWriteMode::Replace,
            session_id,
            &base,
            &config,
        )
        .unwrap();

        // 4. Verify approval is reset
        assert!(!updated.approved);
        let reloaded = plan_read_from_base_path(session_id, &base, &config).unwrap();
        assert!(!reloaded.approved);
    }
}
