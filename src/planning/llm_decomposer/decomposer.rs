use crate::config::AppConfig;
use crate::llm::{ChatMessage, OpenAIClient, run_agent_loop};
use crate::planning::llm_decomposer::types::*;
use crate::planning::task_types::*;
use crate::tools::FsTools;
use anyhow::{Result, anyhow};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::planning::llm_decomposer::parser::{extract_fallback_steps, parse_llm_json};

#[derive(Clone)]
pub struct LlmTaskDecomposer {
    pub client: OpenAIClient,
    pub model: String,
    pub fs_tools: FsTools,
    pub repomap: Arc<RwLock<Option<crate::analysis::RepoMap>>>,
    pub cfg: AppConfig,
}

impl LlmTaskDecomposer {
    pub fn new(
        client: OpenAIClient,
        model: String,
        fs_tools: FsTools,
        repomap: Arc<RwLock<Option<crate::analysis::RepoMap>>>,
        cfg: AppConfig,
    ) -> Self {
        Self {
            client,
            model,
            fs_tools,
            repomap,
            cfg,
        }
    }

    pub async fn decompose_complex_task(
        &self,
        task_description: &str,
        classification: &TaskClassification,
    ) -> Result<Vec<TaskStep>> {
        info!(
            "Starting LLM-assisted decomposition for: {}",
            task_description
        );
        let project_context = self.gather_project_context().await?;

        let prompt = crate::planning::llm_decomposer::prompt::build_decomposition_prompt(
            task_description,
            classification,
            &project_context,
        );

        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: Some(prompt),
            tool_calls: vec![],
            tool_call_id: None,
        }];

        match run_agent_loop(
            &self.client,
            &self.model,
            &self.fs_tools,
            messages,
            None,
            None,
            None, // No session manager for decomposer
            &self.cfg,
            None, // No TuiExecutor for decomposer
        )
        .await
        {
            Ok((_, choice_message)) => {
                match parse_llm_json(&choice_message.content) {
                    Ok(llm_result) => {
                        let steps = self.convert_llm_steps_to_task_steps(llm_result.steps)?;
                        let validated_steps = self
                            .validate_and_adjust_steps(steps, classification)
                            .await?;
                        Ok(validated_steps)
                    }
                    Err(_) => {
                        // fallback
                        let fallback = extract_fallback_steps(&choice_message.content)?;
                        let steps = self.convert_llm_steps_to_task_steps(fallback.steps)?;
                        let validated_steps = self
                            .validate_and_adjust_steps(steps, classification)
                            .await?;
                        Ok(validated_steps)
                    }
                }
            }
            Err(e) => Err(anyhow!("LLM decomposition failed: {}", e)),
        }
    }

    async fn gather_project_context(&self) -> Result<ProjectContext> {
        debug!("Gathering project context");

        let mut context = ProjectContext {
            project_type: "Unknown".to_string(),
            main_languages: Vec::new(),
            key_files: Vec::new(),
            architecture_notes: String::new(),
            recent_changes: Vec::new(),
        };

        if self.fs_tools.fs_read("Cargo.toml", None, None).is_ok() {
            context.project_type = "Rust".to_string();
            context.main_languages.push("Rust".to_string());
            context.key_files.push("Cargo.toml".to_string());
            context.key_files.push("src/main.rs".to_string());
        }

        if self.fs_tools.fs_read("package.json", None, None).is_ok() {
            if context.project_type == "Unknown" {
                context.project_type = "Node.js".to_string();
            } else {
                context.project_type = format!("{}/Node.js", context.project_type);
            }
            context.main_languages.push("JavaScript".to_string());
            context.main_languages.push("TypeScript".to_string());
            context.key_files.push("package.json".to_string());
        }

        if let Some(repomap) = self.repomap.read().await.as_ref() {
            let mut files_with_size: Vec<_> = repomap
                .symbols
                .iter()
                .map(|symbol| symbol.file.to_string_lossy().to_string())
                .collect();
            files_with_size.sort();
            files_with_size.dedup();

            for file in files_with_size.iter().take(10) {
                if !context.key_files.contains(file) {
                    context.key_files.push(file.clone());
                }
            }

            let total_symbols = repomap.symbols.len();
            let total_files = files_with_size.len();
            context.architecture_notes = format!(
                "Project contains {} symbols across {} files. Main modules appear to be organized in a typical {} project structure.",
                total_symbols, total_files, context.project_type
            );
        }

        debug!("Project context gathered: {:?}", context);
        Ok(context)
    }

    fn convert_llm_steps_to_task_steps(
        &self,
        llm_steps: Vec<LlmTaskStep>,
    ) -> Result<Vec<TaskStep>> {
        debug!(
            "Converting {} LLM steps to internal format",
            llm_steps.len()
        );
        let mut task_steps = Vec::new();
        for llm_step in llm_steps {
            let step_type = match llm_step.step_type.as_str() {
                "analysis" => StepType::Analysis,
                "planning" => StepType::Planning,
                "implementation" => StepType::Implementation,
                "validation" => StepType::Validation,
                "cleanup" => StepType::Cleanup,
                _ => {
                    warn!(
                        "Unknown step type: {}, defaulting to Implementation",
                        llm_step.step_type
                    );
                    StepType::Implementation
                }
            };

            let task_step = TaskStep {
                id: llm_step.id,
                description: llm_step.description,
                step_type,
                dependencies: llm_step.dependencies,
                estimated_duration: (llm_step.estimated_duration_minutes * 60) as u64,
                required_tools: llm_step.required_tools,
                validation_criteria: llm_step.validation_criteria,
                prompt_template: Some(llm_step.detailed_instructions),
            };
            task_steps.push(task_step);
        }
        Ok(task_steps)
    }

    async fn validate_and_adjust_steps(
        &self,
        steps: Vec<TaskStep>,
        classification: &TaskClassification,
    ) -> Result<Vec<TaskStep>> {
        debug!("Validating and adjusting {} steps", steps.len());
        let mut validated_steps = Vec::new();
        for step in steps {
            let mut adjusted_step = step;
            adjusted_step.estimated_duration = adjusted_step.estimated_duration.clamp(30, 3600);
            adjusted_step.required_tools =
                self.validate_required_tools(adjusted_step.required_tools);
            adjusted_step.dependencies =
                self.validate_dependencies(&adjusted_step.dependencies, &validated_steps);
            if adjusted_step.validation_criteria.is_empty() {
                adjusted_step.validation_criteria =
                    self.generate_default_validation_criteria(&adjusted_step.step_type);
            }
            validated_steps.push(adjusted_step);
        }

        if classification.complexity_score > 0.8 && validated_steps.len() < 5 {
            warn!("High complexity task has few steps, adding safety validation step");
            validated_steps.push(
                TaskStep::new(
                    "final_safety_check".to_string(),
                    "Final safety check and operation verification".to_string(),
                    StepType::Validation,
                    vec!["execute_bash".to_string()],
                )
                .with_duration(300)
                .with_validation(vec!["Overall operation is normal".to_string()]),
            );
        }

        info!(
            "Validation completed: {} steps validated",
            validated_steps.len()
        );
        Ok(validated_steps)
    }

    fn validate_required_tools(&self, tools: Vec<String>) -> Vec<String> {
        let valid_tools = [
            "fs_read",
            "fs_write",
            "edit",
            "search_text",
            "find_file",
            "get_symbol_info",
            "search_repomap",
            "execute_bash",
            "create_patch",
            "apply_patch",
            "fs_list",
        ];
        tools
            .into_iter()
            .filter(|tool| valid_tools.contains(&tool.as_str()))
            .collect()
    }

    fn validate_dependencies(
        &self,
        dependencies: &[String],
        existing_steps: &[TaskStep],
    ) -> Vec<String> {
        let existing_ids: Vec<String> = existing_steps.iter().map(|s| s.id.clone()).collect();
        dependencies
            .iter()
            .filter(|dep| existing_ids.contains(dep))
            .cloned()
            .collect()
    }

    fn generate_default_validation_criteria(&self, step_type: &StepType) -> Vec<String> {
        match step_type {
            StepType::Analysis => vec!["Analysis is completed".to_string()],
            StepType::Planning => vec!["Plan is clear".to_string()],
            StepType::Implementation => vec![
                "Implementation is completed".to_string(),
                "No compilation errors".to_string(),
            ],
            StepType::Validation => vec!["Validation is successful".to_string()],
            StepType::Cleanup => vec!["Cleanup is completed".to_string()],
        }
    }
}
