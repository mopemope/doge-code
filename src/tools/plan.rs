use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::debug;

const DESCRIPTION: &str = r#"Manages the execution plan. Use strict ID/Status rules: max one 'in_progress'. Use `mode='replace'` to overwrite or `'merge'` to update item statuses. Returns a compact change summary rather than echoing the full plan. Use plan_read only when the complete canonical plan is needed."#;

// Verification obligation types live in the canonical provenance model
// (`crate::provenance::types`) so plan JSON and provenance wire share one
// definition. Re-export here for tool-layer convenience.
pub use crate::provenance::types::{VerificationCommandMatcher, VerificationObligation};

/// Upper bound for an obligation description (chars).
pub const MAX_OBLIGATION_DESCRIPTION_CHARS: usize = 2048;
/// Upper bound for a matcher program (chars).
pub const MAX_OBLIGATION_PROGRAM_CHARS: usize = 512;
/// Upper bound for one args_prefix token (chars).
pub const MAX_OBLIGATION_ARG_CHARS: usize = 1024;
/// Upper bound for args_prefix length.
pub const MAX_OBLIGATION_ARGS: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanItem {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub content: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requirement_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification_obligations: Vec<VerificationObligation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanList {
    pub session_id: Option<String>,
    pub items: Vec<PlanItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanWriteResult {
    pub plan: PlanList,
    pub changed: bool,
    #[serde(default)]
    pub delta: PlanWriteDelta,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Compact LLM-facing outcome for `plan_write`.
///
/// The internal [`PlanWriteResult`] keeps the full canonical [`PlanList`]
/// (needed by persistence, provenance, and validation). This type is the only
/// shape that crosses the LLM boundary: ids and counts, never item content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanWriteToolResult {
    pub ok: bool,
    pub changed: bool,
    #[serde(default)]
    pub delta: PlanWriteDelta,
    pub item_count: usize,
    #[serde(default)]
    pub status_counts: PlanStatusCounts,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warnings_truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning_count: Option<usize>,
}

/// IDs added / updated / removed by a `plan_write`, in plan order.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanWriteDelta {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added_ids: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub updated_ids: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_ids: Vec<String>,
}

/// Counts of plan items per known status.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanStatusCounts {
    pub pending: usize,
    pub in_progress: usize,
    pub completed: usize,
}

impl PlanStatusCounts {
    pub fn from_items(items: &[PlanItem]) -> Self {
        let mut counts = Self::default();
        for item in items {
            match item.status.as_str() {
                "pending" => counts.pending += 1,
                "in_progress" => counts.in_progress += 1,
                "completed" => counts.completed += 1,
                other => {
                    // Post-validation plans carry only known statuses; never
                    // break the validation contract here, just note and skip.
                    debug!(status = %other, id = %item.id, "unknown plan status in counts");
                }
            }
        }
        counts
    }
}

/// Target serialized size (chars) for the compact LLM-facing result.
/// Only `warnings` may be reduced to meet it; delta/counts are never truncated.
pub const PLAN_WRITE_RESULT_TARGET_CHARS: usize = 6_000;

/// Semantic item comparison: same id, differing in content, status,
/// parentage, requirement links, or verification obligations.
///
/// Obligation order is insignificant (canonical id-sorted comparison via
/// [`obligations_equal`]); a pure reorder is not an update.
fn plan_item_semantically_changed(before: &PlanItem, after: &PlanItem) -> bool {
    before.content != after.content
        || before.status != after.status
        || before.parent_id != after.parent_id
        || before.requirement_ids != after.requirement_ids
        || !obligations_equal(
            &before.verification_obligations,
            &after.verification_obligations,
        )
}

/// Pure, deterministic diff of two plan snapshots.
///
/// - `added_ids`: in `after`, not in `before` (ordered as in `after`)
/// - `updated_ids`: in both, semantically changed (ordered as in `after`)
/// - `removed_ids`: in `before`, not in `after` (ordered as in `before`)
///
/// Runs in O(n); result order never depends on hash-map iteration order.
pub fn summarize_plan_changes(before: &[PlanItem], after: &[PlanItem]) -> PlanWriteDelta {
    let before_index: HashMap<&str, &PlanItem> =
        before.iter().map(|item| (item.id.as_str(), item)).collect();
    let after_index: HashMap<&str, &PlanItem> =
        after.iter().map(|item| (item.id.as_str(), item)).collect();

    let mut delta = PlanWriteDelta::default();
    for item in after {
        match before_index.get(item.id.as_str()) {
            None => delta.added_ids.push(item.id.clone()),
            Some(previous) => {
                if plan_item_semantically_changed(previous, item) {
                    delta.updated_ids.push(item.id.clone());
                }
            }
        }
    }
    for item in before {
        if !after_index.contains_key(item.id.as_str()) {
            delta.removed_ids.push(item.id.clone());
        }
    }
    delta
}

fn truncate_warning_head(warning: &str, budget: usize) -> String {
    const RESERVE: usize = 60;
    let total = warning.chars().count();
    if total <= budget {
        return warning.to_string();
    }
    if budget <= RESERVE {
        return format!("[truncated warning of {total} chars]");
    }
    // Char-based take: byte slicing (`&warning[..keep]`) would panic or
    // over-keep on multi-byte UTF-8 boundaries.
    let keep = budget.saturating_sub(RESERVE);
    let kept: String = warning.chars().take(keep).collect();
    let kept_chars = kept.chars().count();
    format!(
        "{kept}\n[...truncated {} of {total} chars]",
        total - kept_chars
    )
}

impl PlanWriteToolResult {
    /// Build the compact LLM-facing result from the internal outcome.
    /// `warnings` are budgeted to [`PLAN_WRITE_RESULT_TARGET_CHARS`];
    /// `delta`, counts, and flags are always preserved verbatim.
    pub fn from_internal(result: &PlanWriteResult) -> Self {
        Self::from_internal_with_budget(result, PLAN_WRITE_RESULT_TARGET_CHARS)
    }

    fn from_internal_with_budget(result: &PlanWriteResult, target_chars: usize) -> Self {
        let total_warnings = result.warnings.len();
        let mut warnings = result.warnings.clone();

        let base = Self {
            ok: true,
            changed: result.changed,
            delta: result.delta.clone(),
            item_count: result.plan.items.len(),
            status_counts: PlanStatusCounts::from_items(&result.plan.items),
            warnings: Vec::new(),
            warnings_truncated: None,
            warning_count: None,
        };
        let fits = |warnings: &[String]| {
            let candidate = Self {
                warnings: warnings.to_vec(),
                ..base.clone()
            };
            serde_json::to_string(&candidate)
                .map(|s| s.chars().count() <= target_chars)
                .unwrap_or(false)
        };

        if !fits(&warnings) {
            // Drop from the tail first, keeping the earliest warnings.
            while warnings.len() > 1 && !fits(&warnings) {
                warnings.pop();
            }
            // A single still-oversized warning is head-truncated in place.
            while !warnings.is_empty() && !fits(&warnings) {
                let last = warnings.len() - 1;
                let current = warnings[last].chars().count();
                if current <= 1 {
                    warnings.pop();
                    break;
                }
                let shorter = current / 2;
                warnings[last] = truncate_warning_head(&warnings[last], shorter);
                if warnings[last].chars().count() >= current {
                    warnings.pop();
                    break;
                }
            }
        }

        // Only claim truncation when warnings were actually reduced. A huge
        // delta alone can exceed the budget with zero warnings; that must not
        // report `warnings_truncated` (delta/counts are never truncated).
        let reduced = warnings != result.warnings;

        Self {
            warnings,
            warnings_truncated: if reduced { Some(true) } else { None },
            warning_count: if reduced { Some(total_warnings) } else { None },
            ..base
        }
    }
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
                                "parent_id": {"type": "string", "nullable": true},
                                "content": {"type": "string", "minLength": 1},
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"],
                                },
                                "requirement_ids": {
                                    "type": "array",
                                    "items": {"type": "string"},
                                    "description": "Requirement ids this plan item implements (must exist in requirements state)",
                                },
                                "verification_obligations": {
                                    "type": "array",
                                    "description": "Verification obligations: what should be observed for this step (e.g. cargo test, cargo clippy). Research-only steps need none.",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "id": {"type": "string"},
                                            "description": {"type": "string"},
                                            "kind": {
                                                "type": "string",
                                                "enum": ["test", "build", "lint", "type_check", "format_check", "syntax_check"],
                                            },
                                            "command": {
                                                "type": "object",
                                                "properties": {
                                                    "program": {"type": "string"},
                                                    "args_prefix": {
                                                        "type": "array",
                                                        "items": {"type": "string"},
                                                    },
                                                },
                                                "required": ["program"],
                                                "additionalProperties": false,
                                            },
                                        },
                                        "required": ["id", "description", "kind"],
                                        "additionalProperties": false,
                                    },
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
                "Reads the current execution plan. Use this to resume work or check status."
                    .to_string(),
            strict: Some(true),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
        },
    }
}

use regex::Regex;

// ... imports ...

/// Legacy plan write without requirement validation.
///
/// Prefer `plan_write_from_base_path` with explicit `valid_requirement_ids`
/// (used by `FsTools::plan_write_with_attribution`). This entry point skips
/// requirement-link validation and exists for tests and non-agent callers
/// that manage requirements separately.
pub fn plan_write(
    items: Vec<PlanItem>,
    mode: PlanWriteMode,
    session_id: &str,
    config: &AppConfig,
    valid_files: Option<&[String]>,
) -> Result<PlanWriteResult> {
    plan_write_from_base_path(
        items,
        mode,
        session_id,
        &config.project_root,
        config,
        valid_files,
        None,
    )
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
    valid_files: Option<&[String]>,
    valid_requirement_ids: Option<&[String]>,
) -> Result<PlanWriteResult> {
    let base = base_path.as_ref();
    let plan_dir = plans_dir(base);
    fs::create_dir_all(&plan_dir)
        .with_context(|| format!("Failed to create plan directory: {}", plan_dir.display()))?;

    let plan_file_path = plan_file_path(base, session_id);
    debug!(?plan_file_path, "write plans");

    let primary_read_result = plan_read_from_path(&plan_file_path);
    let existing_primary_plan = primary_read_result.as_ref().ok().cloned();
    let legacy_read_result = legacy_plan_read(base, session_id);

    let new_items = match (mode, primary_read_result, legacy_read_result) {
        (PlanWriteMode::Replace, _, _) => items,
        (PlanWriteMode::Merge, Ok(existing), _) => merge_items(existing.items, items),
        (PlanWriteMode::Merge, Err(_), Ok(legacy)) => merge_items(legacy.items, items),
        (PlanWriteMode::Merge, Err(_), Err(_)) => items,
    };

    let plan_list = PlanList {
        session_id: Some(session_id.to_string()),
        items: new_items,
    };

    validate_plan_items(&plan_list.items)?;

    if let Some(files) = valid_files {
        validate_completion_files(&plan_list.items, files)?;
    }

    if let Some(valid_reqs) = valid_requirement_ids {
        validate_requirement_links(&plan_list.items, valid_reqs)?;
    }

    let changed = existing_primary_plan
        .as_ref()
        .map(|existing| existing != &plan_list)
        .unwrap_or(true);

    if changed {
        let json_content = serde_json::to_string_pretty(&plan_list)
            .with_context(|| "Failed to serialize plan list to JSON")?;
        fs::write(&plan_file_path, &json_content)
            .with_context(|| format!("Failed to write plan file: {}", plan_file_path.display()))?;
    }

    Ok(PlanWriteResult {
        plan: plan_list,
        changed,
        delta: PlanWriteDelta::default(),
        warnings: Vec::new(),
    })
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
            "{}. [{}] {} (id: {})",
            idx + 1,
            status_symbol,
            item.content.trim(),
            item.id
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
    }

    // Build adjacency list
    let mut adjacency: std::collections::HashMap<&String, &String> =
        std::collections::HashMap::new();
    for item in items {
        if let Some(parent_id) = &item.parent_id {
            if !seen_ids.contains(parent_id) {
                anyhow::bail!(
                    "Plan item '{}' refers to non-existent parent '{}'",
                    item.id,
                    parent_id
                );
            }
            if parent_id == &item.id {
                anyhow::bail!("Plan item '{}' cannot be its own parent", item.id);
            }
            adjacency.insert(&item.id, parent_id);
        }
    }

    // Check for cycles
    for item in items {
        let mut visited = std::collections::HashSet::new();
        let mut curr = &item.id;
        while let Some(parent) = adjacency.get(curr) {
            if !visited.insert(curr) {
                // We shouldn't hit this if we only move up, unless there's a cycle
            }
            if *parent == &item.id {
                anyhow::bail!("Cycle detected involving plan item '{}'", item.id);
            }
            if visited.contains(parent) {
                anyhow::bail!("Cycle detected involving plan item '{}'", parent);
            }
            curr = parent;
        }
    }

    for item in items {
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

    validate_obligation_ids(items)?;

    Ok(())
}

/// Validate a verification obligation id: 1..=64 chars, ASCII letters/digits/`. _ -`.
pub fn validate_obligation_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 64 {
        anyhow::bail!("Verification obligation id '{id}' must be 1..=64 characters");
    }
    let ok = id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
    if !ok {
        anyhow::bail!(
            "Verification obligation id '{id}' may only contain ASCII letters, digits, '.', '_' and '-'"
        );
    }
    Ok(())
}

/// Validate one obligation's shape (id, description, command).
pub fn validate_obligation(ob: &VerificationObligation) -> Result<()> {
    validate_obligation_id(&ob.id)?;
    if ob.description.trim().is_empty() {
        anyhow::bail!(
            "Verification obligation '{}' must include a non-empty description.",
            ob.id
        );
    }
    if ob.description.chars().count() > MAX_OBLIGATION_DESCRIPTION_CHARS {
        anyhow::bail!(
            "Verification obligation '{}' description exceeds {MAX_OBLIGATION_DESCRIPTION_CHARS} chars",
            ob.id
        );
    }
    if let Some(cmd) = &ob.command {
        if cmd.program.trim().is_empty() {
            anyhow::bail!(
                "Verification obligation '{}' has an empty command program.",
                ob.id
            );
        }
        if cmd.program.chars().count() > MAX_OBLIGATION_PROGRAM_CHARS {
            anyhow::bail!(
                "Verification obligation '{}' program exceeds {MAX_OBLIGATION_PROGRAM_CHARS} chars",
                ob.id
            );
        }
        if cmd.args_prefix.len() > MAX_OBLIGATION_ARGS {
            anyhow::bail!(
                "Verification obligation '{}' has too many args_prefix entries (max {MAX_OBLIGATION_ARGS}).",
                ob.id
            );
        }
        for arg in &cmd.args_prefix {
            if arg.is_empty() {
                anyhow::bail!(
                    "Verification obligation '{}' has an empty args_prefix token.",
                    ob.id
                );
            }
            if arg.chars().count() > MAX_OBLIGATION_ARG_CHARS {
                anyhow::bail!(
                    "Verification obligation '{}' args_prefix token exceeds {MAX_OBLIGATION_ARG_CHARS} chars",
                    ob.id
                );
            }
        }
    }
    Ok(())
}

/// Obligation ids must be unique across the whole plan (not just one item).
fn validate_obligation_ids(items: &[PlanItem]) -> Result<()> {
    let mut seen: HashSet<&str> = HashSet::new();
    for item in items {
        for ob in &item.verification_obligations {
            validate_obligation(ob)?;
            if !seen.insert(ob.id.as_str()) {
                anyhow::bail!("Duplicate verification obligation id detected: {}", ob.id);
            }
        }
    }
    Ok(())
}

/// Canonical obligation ordering for semantic comparison: sort by id.
/// Pure reorder is a no-op; `args_prefix` order is significant and preserved.
pub fn sorted_obligations(obs: &[VerificationObligation]) -> Vec<&VerificationObligation> {
    let mut out: Vec<&VerificationObligation> = obs.iter().collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Order-insensitive obligation equality (id-sorted canonical comparison).
pub fn obligations_equal(a: &[VerificationObligation], b: &[VerificationObligation]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let sa = sorted_obligations(a);
    let sb = sorted_obligations(b);
    sa.into_iter().zip(sb).all(|(x, y)| x == y)
}

/// Validate that every `requirement_ids` link refers to a known requirement.
///
/// Unknown ids are rejected. Links to withdrawn requirements are allowed but
/// reported by the caller as a soft warning (existing plans must not break
/// when a requirement is later withdrawn).
pub fn validate_requirement_links(items: &[PlanItem], valid_ids: &[String]) -> Result<()> {
    let valid: HashSet<&str> = valid_ids.iter().map(String::as_str).collect();
    for item in items {
        for req in &item.requirement_ids {
            if !valid.contains(req.as_str()) {
                anyhow::bail!(
                    "Plan item '{}' references unknown requirement '{}'",
                    item.id,
                    req
                );
            }
        }
    }
    Ok(())
}

/// Warn when a plan item links a withdrawn requirement.
pub fn withdrawn_requirement_warnings(items: &[PlanItem], withdrawn_ids: &[String]) -> Vec<String> {
    let withdrawn: HashSet<&str> = withdrawn_ids.iter().map(String::as_str).collect();
    let mut out = Vec::new();
    for item in items {
        for req in &item.requirement_ids {
            if withdrawn.contains(req.as_str()) {
                out.push(format!(
                    "Plan item '{}' references withdrawn requirement '{req}'.",
                    item.id
                ));
            }
        }
    }
    out
}

fn validate_completion_files(items: &[PlanItem], valid_files: &[String]) -> Result<()> {
    // Extensions: rs, toml, js, ts, jsx, tsx, md, json, yml, yaml, html, css, py, c, cpp, h, hpp, go, java, sql, sh, bat, ps1, txt, check
    let file_pattern = Regex::new(r"(?x)
        \b
        (?P<path>
            # Match paths (optional directory prefix) ending with specific extensions
            # This avoids matching common text like 'and/or', 'n/a', 'w/o'
            ([\w.-]+/)*[\w.-]+\.(rs|toml|js|ts|jsx|tsx|md|json|yml|yaml|html|css|py|c|cpp|h|hpp|go|java|sql|sh|bat|ps1|txt|check)
        )
        \b
    ").unwrap();

    for item in items {
        if item.status == "completed" {
            for cap in file_pattern.captures_iter(&item.content) {
                let path = &cap["path"];
                // Check if this path is in valid_files (which are changed files in this session)
                // valid_files are relative or absolute. We check if the valid file *ends with* the detected path
                // to handle relative match.
                // e.g. detected "main.rs" matches valid "src/main.rs".

                let found = valid_files.iter().any(|f| f.ends_with(path));
                if !found {
                    // Try to see if it's just a casing issue or similar, but strict is better.
                    // We allow if the path is NOT in the list, then we error.
                    // BUT: What if the user mentions a file they *read* but didn't modify?
                    // The requirement is "If a plan item is marked as completed... those files must be in changed_files".
                    // This implies if you say "Read main.rs", it shouldn't trigger.
                    // But usually "completed" implies modification in this context?
                    // The user said: "plan_writeで修正内容と一致しない項目まで更新されている... 修正もしていないのにplanのチェックつけてしまう"
                    // so yes, if it's completed, it implies modification.

                    // However, we must be careful about "I read main.rs and it looks good".
                    // If the user *just* read it, they might mark it done.
                    // But the prompt says "Modify plan_write to check... if match ONLY".
                    // If the user *manually* marks it done, they might use a tool.
                    // If the *Agent* marks it done, it should have modified it.

                    anyhow::bail!(
                        "Plan item '{}' is marked as completed but refers to file '{}' which has not been modified in this session. \
                        Please only mark items as completed if you have actually modified the referenced files. \
                        If you only read the file, do not mark the item as completed yet, or verify you modified the correct file.",
                        item.id,
                        path
                    );
                }
            }
        }
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
                parent_id: None,
                content: "Review requirements and clarify scope".into(),
                status: "pending".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
            PlanItem {
                id: "step-2".into(),
                parent_id: None,
                content: "Implement feature across modules".into(),
                status: "pending".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
            PlanItem {
                id: "step-3".into(),
                parent_id: None,
                content: "Run tests and verify results".into(),
                status: "pending".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
        ];
        let result = plan_write_from_base_path(
            items,
            PlanWriteMode::Replace,
            "session",
            base,
            &AppConfig::default(),
            None,
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn plan_write_marks_noop_as_unchanged() {
        let (_dir, base) = plan_dir();
        let items = vec![
            PlanItem {
                id: "step-1".into(),
                parent_id: None,
                content: "Review requirements and clarify scope".into(),
                status: "pending".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
            PlanItem {
                id: "step-2".into(),
                parent_id: None,
                content: "Implement feature across modules".into(),
                status: "pending".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
        ];

        let first = plan_write_from_base_path(
            items.clone(),
            PlanWriteMode::Replace,
            "session",
            base.clone(),
            &AppConfig::default(),
            None,
            None,
        )
        .expect("first write should succeed");
        assert!(first.changed);
        assert_eq!(first.plan.items, items);

        let second = plan_write_from_base_path(
            items.clone(),
            PlanWriteMode::Replace,
            "session",
            base.clone(),
            &AppConfig::default(),
            None,
            None,
        )
        .expect("second write should succeed");
        assert!(!second.changed);
        assert_eq!(second.plan.items, items);

        let updated_items = vec![
            PlanItem {
                id: "step-1".into(),
                parent_id: None,
                content: "Review requirements and clarify scope".into(),
                status: "completed".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
            PlanItem {
                id: "step-2".into(),
                parent_id: None,
                content: "Implement feature across modules".into(),
                status: "pending".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
        ];
        let third = plan_write_from_base_path(
            updated_items.clone(),
            PlanWriteMode::Replace,
            "session",
            base,
            &AppConfig::default(),
            None,
            None,
        )
        .expect("third write should succeed");
        assert!(third.changed);
        assert_eq!(third.plan.items, updated_items);
    }

    #[test]
    fn plan_write_rejects_duplicate_ids() {
        let (_dir, base) = plan_dir();
        let items = vec![
            PlanItem {
                id: "step-1".into(),
                parent_id: None,
                content: "Do something".into(),
                status: "pending".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
            PlanItem {
                id: "step-1".into(),
                parent_id: None,
                content: "Do another".into(),
                status: "pending".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
        ];
        let result = plan_write_from_base_path(
            items,
            PlanWriteMode::Replace,
            "session",
            base,
            &AppConfig::default(),
            None,
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn plan_write_rejects_multiple_in_progress() {
        let (_dir, base) = plan_dir();
        let items = vec![
            PlanItem {
                id: "step-1".into(),
                parent_id: None,
                content: "Work item".into(),
                status: "in_progress".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
            PlanItem {
                id: "step-2".into(),
                parent_id: None,
                content: "Another".into(),
                status: "in_progress".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
        ];
        let result = plan_write_from_base_path(
            items,
            PlanWriteMode::Replace,
            "session",
            base,
            &AppConfig::default(),
            None,
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn plan_write_validates_hierarchy() {
        let (_dir, base) = plan_dir();

        // 1. Valid hierarchy
        let items = vec![
            PlanItem {
                id: "parent".into(),
                parent_id: None,
                content: "Parent task".into(),
                status: "pending".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
            PlanItem {
                id: "child".into(),
                parent_id: Some("parent".into()),
                content: "Child task".into(),
                status: "pending".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
        ];
        let result = plan_write_from_base_path(
            items.clone(),
            PlanWriteMode::Replace,
            "session",
            base.clone(),
            &AppConfig::default(),
            None,
            None,
        );
        assert!(result.is_ok());

        // 2. Invalid parent
        let items = vec![PlanItem {
            id: "child".into(),
            parent_id: Some("non-existent".into()),
            content: "Child task".into(),
            status: "pending".into(),
            requirement_ids: Vec::new(),
            verification_obligations: Vec::new(),
        }];
        let result = plan_write_from_base_path(
            items,
            PlanWriteMode::Replace,
            "session",
            base.clone(),
            &AppConfig::default(),
            None,
            None,
        );
        assert!(result.is_err());

        // 3. Self-referencing
        let items = vec![PlanItem {
            id: "self".into(),
            parent_id: Some("self".into()),
            content: "Infinite loop".into(),
            status: "pending".into(),
            requirement_ids: Vec::new(),
            verification_obligations: Vec::new(),
        }];
        let result = plan_write_from_base_path(
            items,
            PlanWriteMode::Replace,
            "session",
            base.clone(),
            &AppConfig::default(),
            None,
            None,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("own parent"));

        // 4. Indirect cycle (A -> B -> A)
        let items = vec![
            PlanItem {
                id: "A".into(),
                parent_id: Some("B".into()),
                content: "Task A".into(),
                status: "pending".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
            PlanItem {
                id: "B".into(),
                parent_id: Some("A".into()),
                content: "Task B".into(),
                status: "pending".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
        ];
        let result = plan_write_from_base_path(
            items,
            PlanWriteMode::Replace,
            "session",
            base,
            &AppConfig::default(),
            None,
            None,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Cycle detected"));
    }

    #[test]
    fn test_format_plan_summary_includes_ids() {
        let items = vec![
            PlanItem {
                id: "step-1".into(),
                parent_id: None,
                content: "Task 1".into(),
                status: "pending".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
            PlanItem {
                id: "step-2".into(),
                parent_id: None,
                content: "Task 2".into(),
                status: "in_progress".into(),
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            },
        ];
        let summary = format_plan_summary(&items).unwrap();

        assert!(summary.contains("step-1"));
        assert!(summary.contains("step-2"));
        assert!(summary.contains("Task 1"));
        assert!(summary.contains("Task 2"));

        // Check format "1. [◌] Task 1 (id: step-1)"
        assert!(summary.contains("1. [◌] Task 1 (id: step-1)"));
        assert!(summary.contains("2. [◔] Task 2 (id: step-2)"));
    }

    #[test]
    fn plan_write_validates_changed_files() {
        let (_dir, base) = plan_dir();

        let valid_files = vec!["src/main.rs".to_string(), "Cargo.toml".to_string()];

        // 1. Success: Referred file is in changed_files
        let items = vec![PlanItem {
            id: "step-1".into(),
            parent_id: None,
            content: "Update src/main.rs".into(),
            status: "completed".into(),
            requirement_ids: Vec::new(),
            verification_obligations: Vec::new(),
        }];
        let result = plan_write_from_base_path(
            items,
            PlanWriteMode::Replace,
            "session",
            base.clone(),
            &AppConfig::default(),
            Some(&valid_files),
            None,
        );
        assert!(result.is_ok());

        // 2. Failure: Referred file is NOT in changed_files
        let items = vec![PlanItem {
            id: "step-2".into(),
            parent_id: None,
            content: "Update utils.rs".into(),
            status: "completed".into(),
            requirement_ids: Vec::new(),
            verification_obligations: Vec::new(),
        }];
        let result = plan_write_from_base_path(
            items,
            PlanWriteMode::Replace,
            "session",
            base.clone(),
            &AppConfig::default(),
            Some(&valid_files),
            None,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("modified"));

        // 3. Success: No files mentioned
        let items = vec![PlanItem {
            id: "step-3".into(),
            parent_id: None,
            content: "Think about life".into(),
            status: "completed".into(),
            requirement_ids: Vec::new(),
            verification_obligations: Vec::new(),
        }];
        let result = plan_write_from_base_path(
            items,
            PlanWriteMode::Replace,
            "session",
            base.clone(),
            &AppConfig::default(),
            Some(&valid_files),
            None,
        );
        assert!(result.is_ok());

        // 4. Success: Pending item mentions file not in changed_files (should be ignored)
        let items = vec![PlanItem {
            id: "step-4".into(),
            parent_id: None,
            content: "Will update utils.rs".into(),
            status: "pending".into(),
            requirement_ids: Vec::new(),
            verification_obligations: Vec::new(),
        }];
        let result = plan_write_from_base_path(
            items,
            PlanWriteMode::Replace,
            "session",
            base.clone(),
            &AppConfig::default(),
            Some(&valid_files),
            None,
        );
        assert!(result.is_ok());

        // 5. Success: Content contains "and/or" which shouldn't be matched as a file
        let items = vec![PlanItem {
            id: "step-5".into(),
            parent_id: None,
            content: "Review this and/or that".into(),
            status: "completed".into(),
            requirement_ids: Vec::new(),
            verification_obligations: Vec::new(),
        }];
        let result = plan_write_from_base_path(
            items,
            PlanWriteMode::Replace,
            "session",
            base.clone(),
            &AppConfig::default(),
            Some(&valid_files),
            None,
        );
        assert!(result.is_ok());
    }

    fn obligation(id: &str) -> VerificationObligation {
        VerificationObligation {
            id: id.to_string(),
            description: "desc".to_string(),
            kind: crate::provenance::VerificationKind::Test,
            command: Some(VerificationCommandMatcher {
                program: "cargo".to_string(),
                args_prefix: vec!["test".to_string()],
            }),
        }
    }

    fn plan_item_with_obligations(id: &str, obs: Vec<VerificationObligation>) -> PlanItem {
        PlanItem {
            id: id.to_string(),
            parent_id: None,
            content: "work".to_string(),
            status: "pending".to_string(),
            requirement_ids: Vec::new(),
            verification_obligations: obs,
        }
    }

    #[test]
    fn test_old_plan_without_obligations_deserializes() {
        let json = serde_json::json!({
            "session_id": "s",
            "items": [{
                "id": "step-1",
                "content": "a",
                "status": "pending",
                "requirement_ids": []
            }]
        });
        let list: PlanList = serde_json::from_value(json).unwrap();
        assert!(list.items[0].verification_obligations.is_empty());
    }

    #[test]
    fn test_valid_obligation_roundtrip() {
        let (_dir, base) = plan_dir();
        let items = vec![plan_item_with_obligations(
            "step-1",
            vec![obligation("vo-1")],
        )];
        let res = plan_write_from_base_path(
            items.clone(),
            PlanWriteMode::Replace,
            "session",
            base.clone(),
            &AppConfig::default(),
            None,
            None,
        )
        .unwrap();
        assert!(res.changed);
        let read = plan_read_from_base_path("session", base, &AppConfig::default()).unwrap();
        assert_eq!(read.items[0].verification_obligations.len(), 1);
    }

    #[test]
    fn test_duplicate_obligation_id_same_item_rejected() {
        let (_dir, base) = plan_dir();
        let items = vec![plan_item_with_obligations(
            "step-1",
            vec![obligation("vo-1"), obligation("vo-1")],
        )];
        assert!(
            plan_write_from_base_path(
                items,
                PlanWriteMode::Replace,
                "session",
                base,
                &AppConfig::default(),
                None,
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn test_duplicate_obligation_id_across_items_rejected() {
        let (_dir, base) = plan_dir();
        let items = vec![
            plan_item_with_obligations("step-1", vec![obligation("vo-1")]),
            plan_item_with_obligations("step-2", vec![obligation("vo-1")]),
        ];
        assert!(
            plan_write_from_base_path(
                items,
                PlanWriteMode::Replace,
                "session",
                base,
                &AppConfig::default(),
                None,
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn test_empty_and_invalid_obligation_id_rejected() {
        let (_dir, base) = plan_dir();
        let mut bad = obligation("");
        bad.id = "".to_string();
        let items = vec![plan_item_with_obligations("step-1", vec![bad])];
        assert!(
            plan_write_from_base_path(
                items,
                PlanWriteMode::Replace,
                "session",
                base.clone(),
                &AppConfig::default(),
                None,
                None,
            )
            .is_err()
        );
        let mut bad2 = obligation("bad id");
        bad2.id = "bad id".to_string();
        let items2 = vec![plan_item_with_obligations("step-1", vec![bad2])];
        assert!(
            plan_write_from_base_path(
                items2,
                PlanWriteMode::Replace,
                "session",
                base,
                &AppConfig::default(),
                None,
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn test_empty_description_rejected() {
        let (_dir, base) = plan_dir();
        let mut ob = obligation("vo-1");
        ob.description = "   ".to_string();
        let items = vec![plan_item_with_obligations("step-1", vec![ob])];
        assert!(
            plan_write_from_base_path(
                items,
                PlanWriteMode::Replace,
                "session",
                base,
                &AppConfig::default(),
                None,
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn test_invalid_kind_rejected_by_schema() {
        let json = serde_json::json!({
            "id": "vo-1",
            "description": "d",
            "kind": "not_a_kind"
        });
        assert!(serde_json::from_value::<VerificationObligation>(json).is_err());
    }

    #[test]
    fn test_pure_obligation_reorder_is_noop() {
        let ob1 = obligation("vo-1");
        let mut ob2 = obligation("vo-2");
        ob2.kind = crate::provenance::VerificationKind::Lint;
        let a = vec![ob1.clone(), ob2.clone()];
        let b = vec![ob2, ob1];
        assert!(obligations_equal(&a, &b));
    }

    #[test]
    fn test_obligation_definition_change_is_not_equal() {
        let mut ob1 = obligation("vo-1");
        let ob2 = obligation("vo-1");
        ob1.description = "different".to_string();
        assert!(!obligations_equal(
            std::slice::from_ref(&ob1),
            std::slice::from_ref(&ob2)
        ));
    }

    fn delta_item(id: &str, content: &str, status: &str) -> PlanItem {
        PlanItem {
            id: id.to_string(),
            parent_id: None,
            content: content.to_string(),
            status: status.to_string(),
            requirement_ids: Vec::new(),
            verification_obligations: Vec::new(),
        }
    }

    #[test]
    fn test_summarize_no_change_yields_empty_delta() {
        let before = vec![delta_item("a", "work a", "pending")];
        let after = before.clone();
        let delta = summarize_plan_changes(&before, &after);
        assert!(delta.added_ids.is_empty());
        assert!(delta.updated_ids.is_empty());
        assert!(delta.removed_ids.is_empty());
    }

    #[test]
    fn test_summarize_added() {
        let before = vec![delta_item("a", "work a", "pending")];
        let after = vec![
            delta_item("a", "work a", "pending"),
            delta_item("b", "work b", "pending"),
        ];
        let delta = summarize_plan_changes(&before, &after);
        assert_eq!(delta.added_ids, vec!["b".to_string()]);
        assert!(delta.updated_ids.is_empty());
        assert!(delta.removed_ids.is_empty());
    }

    #[test]
    fn test_summarize_updated_status() {
        let before = vec![delta_item("a", "work a", "pending")];
        let after = vec![delta_item("a", "work a", "in_progress")];
        let delta = summarize_plan_changes(&before, &after);
        assert!(delta.added_ids.is_empty());
        assert_eq!(delta.updated_ids, vec!["a".to_string()]);
        assert!(delta.removed_ids.is_empty());
    }

    #[test]
    fn test_summarize_updated_content() {
        let before = vec![delta_item("a", "work a", "pending")];
        let after = vec![delta_item("a", "work a revised", "pending")];
        let delta = summarize_plan_changes(&before, &after);
        assert_eq!(delta.updated_ids, vec!["a".to_string()]);
    }

    #[test]
    fn test_summarize_updated_requirements() {
        let mut before_item = delta_item("a", "work a", "pending");
        let mut after_item = delta_item("a", "work a", "pending");
        before_item.requirement_ids = vec!["req-1".to_string()];
        after_item.requirement_ids = vec!["req-1".to_string(), "req-2".to_string()];
        let delta = summarize_plan_changes(
            std::slice::from_ref(&before_item),
            std::slice::from_ref(&after_item),
        );
        assert_eq!(delta.updated_ids, vec!["a".to_string()]);
    }

    #[test]
    fn test_summarize_updated_obligations() {
        let before_item = plan_item_with_obligations("a", vec![obligation("vo-1")]);
        let mut changed_ob = obligation("vo-1");
        changed_ob.description = "different".to_string();
        let after_item = plan_item_with_obligations("a", vec![changed_ob]);
        let delta = summarize_plan_changes(
            std::slice::from_ref(&before_item),
            std::slice::from_ref(&after_item),
        );
        assert_eq!(delta.updated_ids, vec!["a".to_string()]);
    }

    #[test]
    fn test_summarize_obligation_reorder_is_not_updated() {
        let ob1 = obligation("vo-1");
        let mut ob2 = obligation("vo-2");
        ob2.kind = crate::provenance::VerificationKind::Lint;
        let before_item = plan_item_with_obligations("a", vec![ob1.clone(), ob2.clone()]);
        let after_item = plan_item_with_obligations("a", vec![ob2, ob1]);
        let delta = summarize_plan_changes(
            std::slice::from_ref(&before_item),
            std::slice::from_ref(&after_item),
        );
        assert!(delta.updated_ids.is_empty());
    }

    #[test]
    fn test_summarize_removed() {
        let before = vec![
            delta_item("a", "work a", "pending"),
            delta_item("b", "work b", "pending"),
        ];
        let after = vec![delta_item("a", "work a", "pending")];
        let delta = summarize_plan_changes(&before, &after);
        assert!(delta.added_ids.is_empty());
        assert!(delta.updated_ids.is_empty());
        assert_eq!(delta.removed_ids, vec!["b".to_string()]);
    }

    #[test]
    fn test_summarize_combined() {
        let before = vec![
            delta_item("keep", "keep", "pending"),
            delta_item("change", "old", "pending"),
            delta_item("drop", "drop", "pending"),
        ];
        let after = vec![
            delta_item("change", "old", "in_progress"),
            delta_item("keep", "keep", "pending"),
            delta_item("new", "new", "pending"),
        ];
        let delta = summarize_plan_changes(&before, &after);
        assert_eq!(delta.added_ids, vec!["new".to_string()]);
        assert_eq!(delta.updated_ids, vec!["change".to_string()]);
        assert_eq!(delta.removed_ids, vec!["drop".to_string()]);
    }

    #[test]
    fn test_summarize_deterministic_order_follows_plan() {
        // HashMap iteration order must not leak into the delta: added follows
        // `after` order, removed follows `before` order.
        let before: Vec<PlanItem> = (0..20)
            .map(|i| delta_item(&format!("old-{i:02}"), "gone", "pending"))
            .collect();
        let after: Vec<PlanItem> = (0..20)
            .map(|i| delta_item(&format!("new-{i:02}"), "fresh", "pending"))
            .collect();
        for _ in 0..5 {
            let delta = summarize_plan_changes(&before, &after);
            let expected_added: Vec<String> = (0..20).map(|i| format!("new-{i:02}")).collect();
            let expected_removed: Vec<String> = (0..20).map(|i| format!("old-{i:02}")).collect();
            assert_eq!(delta.added_ids, expected_added);
            assert_eq!(delta.removed_ids, expected_removed);
        }
    }

    #[test]
    fn test_status_counts() {
        let items = vec![
            delta_item("a", "a", "pending"),
            delta_item("b", "b", "pending"),
            delta_item("c", "c", "pending"),
            delta_item("d", "d", "in_progress"),
            delta_item("e", "e", "completed"),
            delta_item("f", "f", "completed"),
            delta_item("g", "g", "completed"),
            delta_item("h", "h", "completed"),
        ];
        let counts = PlanStatusCounts::from_items(&items);
        assert_eq!(counts.pending, 3);
        assert_eq!(counts.in_progress, 1);
        assert_eq!(counts.completed, 4);
    }

    #[test]
    fn test_status_counts_empty_is_zero() {
        let counts = PlanStatusCounts::from_items(&[]);
        assert_eq!(counts, PlanStatusCounts::default());
    }

    #[test]
    fn test_status_counts_ignores_unknown_without_panic() {
        let items = vec![
            delta_item("a", "a", "pending"),
            delta_item("b", "b", "bogus"),
        ];
        let counts = PlanStatusCounts::from_items(&items);
        assert_eq!(counts.pending, 1);
        assert_eq!(counts.in_progress, 0);
        assert_eq!(counts.completed, 0);
    }

    #[test]
    fn test_tool_result_preserves_warnings() {
        let internal = PlanWriteResult {
            plan: PlanList {
                session_id: Some("s".to_string()),
                items: vec![delta_item("a", "a", "pending")],
            },
            changed: true,
            delta: PlanWriteDelta {
                added_ids: vec!["a".to_string()],
                ..PlanWriteDelta::default()
            },
            warnings: vec!["warning-a".to_string(), "warning-b".to_string()],
        };
        let tool = PlanWriteToolResult::from_internal(&internal);
        assert!(tool.ok);
        assert!(tool.changed);
        assert_eq!(
            tool.warnings,
            vec!["warning-a".to_string(), "warning-b".to_string()]
        );
        assert_eq!(tool.item_count, 1);
        assert_eq!(tool.status_counts.pending, 1);
        assert!(tool.warnings_truncated.is_none());
        assert!(tool.warning_count.is_none());
    }

    #[test]
    fn test_tool_result_noop_delta_empty() {
        let items = vec![delta_item("a", "a", "pending")];
        let internal = PlanWriteResult {
            plan: PlanList {
                session_id: Some("s".to_string()),
                items: items.clone(),
            },
            changed: false,
            delta: summarize_plan_changes(&items, &items),
            warnings: Vec::new(),
        };
        let tool = PlanWriteToolResult::from_internal(&internal);
        assert!(!tool.changed);
        assert!(tool.delta.added_ids.is_empty());
        assert!(tool.delta.updated_ids.is_empty());
        assert!(tool.delta.removed_ids.is_empty());
        let serialized = serde_json::to_string(&tool).unwrap();
        assert!(serialized.contains("\"delta\":{}"));
    }

    #[test]
    fn test_tool_result_truncates_warnings_only() {
        let items = vec![delta_item("a", "a", "pending")];
        let warnings: Vec<String> = (0..40)
            .map(|i| format!("w{i:02}-{}", "x".repeat(500)))
            .collect();
        let internal = PlanWriteResult {
            plan: PlanList {
                session_id: Some("s".to_string()),
                items,
            },
            changed: true,
            delta: PlanWriteDelta {
                updated_ids: vec!["a".to_string()],
                ..PlanWriteDelta::default()
            },
            warnings,
        };
        let tool = PlanWriteToolResult::from_internal(&internal);
        let serialized = serde_json::to_string(&tool).unwrap();
        assert!(serialized.chars().count() <= PLAN_WRITE_RESULT_TARGET_CHARS);
        // Delta and counts are never truncated.
        assert_eq!(tool.delta.updated_ids, vec!["a".to_string()]);
        assert_eq!(tool.item_count, 1);
        assert_eq!(tool.warnings_truncated, Some(true));
        assert_eq!(tool.warning_count, Some(40));
    }

    #[test]
    fn test_tool_result_huge_delta_without_warnings_claims_no_truncation() {
        // A huge delta alone can exceed the warning budget with zero warnings.
        // That must not report `warnings_truncated` (delta is never truncated).
        let items: Vec<PlanItem> = (0..500)
            .map(|i| delta_item(&format!("step-{i:04}"), "x", "pending"))
            .collect();
        let internal = PlanWriteResult {
            plan: PlanList {
                session_id: Some("s".to_string()),
                items: items.clone(),
            },
            changed: true,
            delta: summarize_plan_changes(&[], &items),
            warnings: Vec::new(),
        };
        let compact =
            serde_json::to_string(&PlanWriteToolResult::from_internal(&internal)).unwrap();
        assert!(
            compact.chars().count() > PLAN_WRITE_RESULT_TARGET_CHARS,
            "fixture must actually exceed the budget"
        );
        let tool = PlanWriteToolResult::from_internal(&internal);
        assert_eq!(tool.delta.added_ids.len(), 500);
        assert!(tool.warnings_truncated.is_none());
        assert!(tool.warning_count.is_none());
    }

    #[test]
    fn test_compact_result_much_smaller_than_full_plan() {
        let items: Vec<PlanItem> = (0..100)
            .map(|i| PlanItem {
                id: format!("step-{i:03}"),
                parent_id: None,
                content: format!("do work item {i:03} {}", "x".repeat(200)),
                status: if i == 0 {
                    "in_progress".to_string()
                } else {
                    "pending".to_string()
                },
                requirement_ids: Vec::new(),
                verification_obligations: Vec::new(),
            })
            .collect();
        let internal = PlanWriteResult {
            plan: PlanList {
                session_id: Some("s".to_string()),
                items: items.clone(),
            },
            changed: true,
            delta: summarize_plan_changes(&[], &items),
            warnings: Vec::new(),
        };
        let old_style = serde_json::to_string(&internal).unwrap();
        let compact =
            serde_json::to_string(&PlanWriteToolResult::from_internal(&internal)).unwrap();
        assert!(compact.len() < old_style.len());
        assert!(compact.chars().count() < 4_000);
        // No plan item content leaks into the compact result.
        assert!(!compact.contains("do work item"));
        assert!(!compact.contains("\"plan\""));
        assert!(!compact.contains("\"items\""));
    }
}
