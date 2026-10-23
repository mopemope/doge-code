use super::{
    plan::Plan,
    workspace::{self, Manifest},
};
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeMap, path::PathBuf};
#[derive(Debug)]
pub struct Guard {
    pub budget: Option<std::sync::Arc<super::ledger::Store>>,
    pub root: PathBuf,
    pub baseline: Manifest,
    pub plan: Plan,
    pub suffixes: BTreeMap<String, String>,
}
impl Guard {
    pub fn permits(name: &str) -> bool {
        matches!(
            name,
            "fs_read" | "fs_list" | "search_text" | "edit" | "fs_write" | "observation_read"
        )
    }
    pub fn preflight(&self, calls: &[crate::llm::types::ToolCall]) -> Result<()> {
        if let Some(budget) = &self.budget {
            budget.require_active()?;
        }
        self.check()?;
        for call in calls {
            let name = call.function.name.as_str();
            ensure!(
                call.r#type == "function",
                "repair requires function tool calls"
            );
            ensure!(Self::permits(name), "repair scope: unsupported tool {name}");
            let args: serde_json::Value = serde_json::from_str(&call.function.arguments)?;
            crate::llm::tool_execution::validate_builtin_arguments(name, &args)?;
            if matches!(name, "fs_read" | "fs_list" | "edit" | "fs_write") {
                let key = if name == "edit" { "file_path" } else { "path" };
                let path = PathBuf::from(args[key].as_str().context("repair path required")?);
                let path = if path.is_absolute() {
                    path
                } else {
                    self.root.join(path)
                };
                let resolved = path
                    .canonicalize()
                    .context("repair permits existing paths only")?;
                let rel = resolved
                    .strip_prefix(&self.root)
                    .context("repair scope: external path")?
                    .to_str()
                    .context("repair path encoding")?;
                ensure!(
                    !workspace::excluded(rel),
                    "repair runtime/build path refused"
                );
                if matches!(name, "edit" | "fs_write") {
                    ensure!(
                        self.plan.editable_paths.contains(rel),
                        "repair scope: undeclared mutation"
                    );
                    if let Some(marker) = self.plan.protected_suffixes.get(rel) {
                        if name == "fs_write" {
                            ensure!(
                                workspace::suffix(
                                    args["content"].as_str().context("content")?,
                                    marker
                                )? == self.suffixes[rel],
                                "repair protected suffix"
                            );
                        } else {
                            let target = args["target_block"].as_str().context("target block")?;
                            let data = workspace::read_text(&resolved)?;
                            let boundary = data.find(marker).context("protected marker")?;
                            let start_line = args["start_line"].as_u64().unwrap_or(1);
                            let end_line = args["end_line"].as_u64().unwrap_or(u64::MAX);
                            ensure!(
                                !target.is_empty()
                                    && data.match_indices(target).all(|(start, _)| {
                                        // Use edit's exact match and line-range semantics.
                                        let line = crate::tools::edit::line_at(&data, start) as u64;
                                        line < start_line
                                            || line > end_line
                                            || start + target.len() <= boundary
                                    })
                                    && !args["new_block"]
                                        .as_str()
                                        .context("new block")?
                                        .contains(marker),
                                "repair protected suffix edit"
                            );
                        }
                    }
                }
            }
            if name == "search_text"
                && let Some(pattern) = args["file_glob"].as_str()
            {
                // search_text supports absolute candidate paths. Strip the
                // literal root before checking traversal and excluded files;
                // the candidate itself lives beneath an excluded .doge path.
                let relative = if std::path::Path::new(pattern).is_absolute() {
                    std::path::Path::new(pattern)
                        .strip_prefix(&self.root)
                        .context("repair search scope: external path")?
                        .to_str()
                        .context("repair search path encoding")?
                } else {
                    pattern
                };
                ensure!(
                    !relative
                        .split('/')
                        .any(|c| c == ".." || matches!(c, ".doge" | ".git" | "target")),
                    "repair search scope"
                );
            }
        }
        Ok(())
    }
    pub fn check(&self) -> Result<Manifest> {
        workspace::check_scope(&self.root, &self.baseline, &self.plan, &self.suffixes)
    }
}
