use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::info;

use crate::config::AppConfig;
use crate::exec::Executor;

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkflowMetadata {
    pub description: Option<String>,
}

#[derive(Debug)]
pub struct Workflow {
    pub name: String,
    pub metadata: WorkflowMetadata,
    pub steps: Vec<String>,
    pub file_path: PathBuf,
}

impl Workflow {
    pub async fn load(name: &str, project_root: &Path) -> Result<Self> {
        let workflows_dir = project_root.join(".doge").join("workflows");
        let file_path = workflows_dir.join(format!("{}.md", name));

        if !file_path.exists() {
            return Err(anyhow::anyhow!(
                "Workflow file not found: {}. Expected at: {}",
                name,
                file_path.display()
            ));
        }

        let content = fs::read_to_string(&file_path)
            .await
            .with_context(|| format!("Failed to read workflow file: {}", file_path.display()))?;

        Self::parse(name, &content, file_path)
    }

    fn parse(name: &str, content: &str, file_path: PathBuf) -> Result<Self> {
        let (metadata, body) = if let Some((yaml_str, body)) = frontmatter_sections(content) {
            let metadata: WorkflowMetadata = serde_saphyr::from_str(yaml_str)
                .map_err(|e| anyhow::anyhow!("Failed to parse workflow frontmatter: {}", e))?;
            (metadata, body)
        } else {
            // No frontmatter
            (WorkflowMetadata { description: None }, content)
        };

        // Parse steps from markdown list items or non-empty lines
        // For simplicity, we treat every non-empty line that looks like an instruction as a step.
        // We'll filter out comments and headers.
        let steps = body
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty() && !line.starts_with('#')) // Ignore headers
            .filter_map(|line: &str| {
                // Strip list markers if present
                let clean_line = if let Some(stripped) = line.strip_prefix("- ") {
                    stripped
                } else if let Some(idx) = line.find(". ") {
                    // numeric list "1. "
                    if line[..idx].chars().all(char::is_numeric) {
                        &line[idx + 2..]
                    } else {
                        line
                    }
                } else {
                    line
                };

                if clean_line.is_empty() {
                    None
                } else {
                    Some(clean_line.to_string())
                }
            })
            .collect();

        Ok(Self {
            name: name.to_string(),
            metadata,
            steps,
            file_path,
        })
    }
}

/// Frontmatter delimiters occupy a whole, unindented line. Dashes inside
/// quoted/plain YAML values or indented block scalars are metadata, not steps.
fn frontmatter_sections(content: &str) -> Option<(&str, &str)> {
    let mut offset = 0;
    let mut metadata_start = None;
    for line in content.split_inclusive('\n') {
        let next_offset = offset + line.len();
        if let Some(start) = metadata_start {
            if line.trim_end() == "---" {
                return Some((&content[start..offset], &content[next_offset..]));
            }
        } else if line.trim().is_empty() {
            // Preserve leading blank lines before frontmatter.
        } else if line.trim_end() == "---" {
            metadata_start = Some(next_offset);
        } else {
            return None;
        }
        offset = next_offset;
    }
    // Keep legacy handling of content without a complete frontmatter block.
    None
}

pub struct WorkflowExecutor {
    cfg: AppConfig,
}

impl WorkflowExecutor {
    pub fn new(cfg: AppConfig) -> Self {
        Self { cfg }
    }

    pub async fn execute(&self, workflow: &Workflow) -> Result<()> {
        info!(
            "Starting workflow: {} ({})",
            workflow.name,
            workflow
                .metadata
                .description
                .as_deref()
                .unwrap_or("No description")
        );

        let mut executor = Executor::new(self.cfg.clone()).await?;

        for (i, step) in workflow.steps.iter().enumerate() {
            let step_num = i + 1;
            info!(
                "Running step {}/{}: {}",
                step_num,
                workflow.steps.len(),
                step
            );
            println!("🚀 Step {}/{}: {}", step_num, workflow.steps.len(), step);

            // Each step is one bounded agent run. A completed response is not
            // independent evidence that its requested repair or checks succeeded.
            let instruction = format!(
                "Workflow Step {}/{}: {}\n\nExecute this step.",
                step_num,
                workflow.steps.len(),
                step
            );

            executor
                .run(&instruction, false)
                .await?
                .require_completed()
                .with_context(|| {
                    format!("Workflow '{}' stopped at step {}", workflow.name, step_num)
                })?;

            println!("Step {} agent response completed.", step_num);
        }

        println!(
            "Workflow '{}' agent responses completed. Review changes and verification; task acceptance is not confirmed.",
            workflow.name
        );
        Ok(())
    }
}

pub async fn run_workflow(cfg: AppConfig, workflow_name: &str) -> Result<()> {
    let workflow = Workflow::load(workflow_name, &cfg.project_root).await?;
    let executor = WorkflowExecutor::new(cfg);
    executor.execute(&workflow).await
}

#[cfg(test)]
mod yaml_compatibility_tests {
    use super::*;
    #[test]
    fn legacy_markdown_frontmatter_preserves_multiline_description() {
        let workflow = Workflow::parse(
            "legacy",
            "---\ndescription: |\n  on 001 null\n  日本語\n---\n- first step\n- second step\n",
            PathBuf::from("legacy.md"),
        )
        .expect("legacy workflow");
        assert_eq!(
            workflow.metadata.description.as_deref(),
            Some("on 001 null\n日本語\n")
        );
        assert_eq!(workflow.steps, ["first step", "second step"]);
    }
}

#[cfg(test)]
mod frontmatter_delimiter_tests {
    use super::*;
    fn parse(content: &str) -> Workflow {
        Workflow::parse("local", content, PathBuf::from("local.md")).expect("valid workflow")
    }
    #[test]
    fn quoted_description_dashes_are_literal_metadata() {
        let workflow = parse("---\ndescription: \"Fix foo --- keep\"\n---\n- actual step\n");
        assert_eq!(
            workflow.metadata.description.as_deref(),
            Some("Fix foo --- keep")
        );
        assert_eq!(workflow.steps, ["actual step"]);
    }
    #[test]
    fn plain_description_dashes_do_not_become_extra_instructions() {
        let workflow = parse("---\ndescription: Fix foo --- keep\n---\n- actual step\n");
        assert_eq!(
            workflow.metadata.description.as_deref(),
            Some("Fix foo --- keep")
        );
        assert_eq!(workflow.steps, ["actual step"]);
    }
    #[test]
    fn indented_block_scalar_delimiter_is_metadata_not_a_boundary() {
        let workflow =
            parse("---\ndescription: |\n  first --- detail\n  ---\n  日本語\n---\n- actual step\n");
        assert_eq!(
            workflow.metadata.description.as_deref(),
            Some("first --- detail\n---\n日本語\n")
        );
        assert_eq!(workflow.steps, ["actual step"]);
    }
    #[test]
    fn crlf_and_leading_blank_lines_keep_description_and_step_order() {
        let workflow = parse(
            "\r\n \r\n--- \r\ndescription: \"literal --- detail\"\r\n---\r\n1. first\r\n2. second\r\n",
        );
        assert_eq!(
            workflow.metadata.description.as_deref(),
            Some("literal --- detail")
        );
        assert_eq!(workflow.steps, ["first", "second"]);
    }
    #[test]
    fn no_frontmatter_preserves_literal_body_and_missing_close_behavior() {
        for content in [
            "- first --- detail\n---\n- second\n",
            "---\ndescription: incomplete\n- first\n",
        ] {
            let workflow = parse(content);
            assert!(workflow.metadata.description.is_none());
            assert_eq!(
                workflow.steps,
                content
                    .lines()
                    .map(|line| line.strip_prefix("- ").unwrap_or(line))
                    .collect::<Vec<_>>()
            );
        }
    }
    #[test]
    fn malformed_yaml_still_errors_and_inline_opening_is_not_frontmatter() {
        assert!(
            Workflow::parse(
                "local",
                "---\ndescription: [broken\n---\n- step\n",
                PathBuf::from("local.md")
            )
            .is_err()
        );
        let workflow = parse("--- inline --- marker\n- actual step\n");
        assert!(workflow.metadata.description.is_none());
        assert_eq!(workflow.steps, ["--- inline --- marker", "actual step"]);
    }
}
