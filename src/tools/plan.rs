use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::debug;

const DESCRIPTION: &str = r#"
Use this tool to create or update the execution plan for the current session.
Always draft concrete, ordered steps before modifying code, and rewrite the
plan as scope evolves.

Guidelines:
- Provide at least three specific steps (pending by default)
- Track progress by updating each item's status (pending | in_progress | completed)
- Keep only one item in_progress at a time; mark completed immediately after finishing
- Do not delete history mid-session; instead, append or update statuses
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
    };

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
