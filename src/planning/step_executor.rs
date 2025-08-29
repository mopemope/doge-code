use crate::llm::{ChatMessage, OpenAIClient, run_agent_loop};
use crate::planning::execution_context::ExecutionContext;
use crate::planning::prompt_builder;
use crate::planning::task_types::*;
use crate::planning::validator;
use crate::tools::FsTools;
use anyhow::{Result, anyhow};
use std::sync::mpsc;
use tracing::{debug, error, info};

/// Task execution engine
pub struct TaskExecutor {
    client: OpenAIClient,
    model: String,
    fs_tools: FsTools,
}

impl TaskExecutor {
    pub fn new(client: OpenAIClient, model: String, fs_tools: FsTools) -> Self {
        Self {
            client,
            model,
            fs_tools,
        }
    }

    /// Execute task plan
    pub async fn execute_plan(
        &self,
        plan: TaskPlan,
        ui_tx: Option<mpsc::Sender<String>>,
    ) -> Result<ExecutionResult> {
        info!("Starting execution of plan: {}", plan.id);

        if let Some(tx) = &ui_tx {
            let _ = tx.send(format!(
                "[START] Task execution started: {}",
                plan.original_request
            ));
        }

        let mut context = ExecutionContext::new();
        let mut successful_steps = 0;
        let total_steps = plan.steps.len();

        for (index, step) in plan.steps.iter().enumerate() {
            // Check dependencies
            if !self.check_dependencies(step, &context) {
                let error_msg = format!("Dependencies not satisfied for step: {}", step.id);
                error!("{}", error_msg);

                return Ok(ExecutionResult {
                    plan_id: plan.id,
                    success: false,
                    completed_steps: context.completed_steps(),
                    total_duration: ((chrono::Utc::now() - context.start_time()).num_milliseconds()
                        / 1000) as u64,
                    final_message: error_msg,
                });
            }

            // Progress notification
            if let Some(tx) = &ui_tx {
                let _ = tx.send(format!(
                    "[STEP] Step {}/{}: {}",
                    index + 1,
                    total_steps,
                    step.description
                ));
            }

            // Step execution
            let step_start = chrono::Utc::now();
            match self.execute_step(step, &mut context, &ui_tx).await {
                Ok(result) => {
                    let duration = (chrono::Utc::now() - step_start).num_seconds() as u64;
                    let step_result = StepResult {
                        step_id: step.id.clone(),
                        success: true,
                        output: result,
                        artifacts: vec![], // TODO: Collect actual artifacts
                        duration,
                        error_message: None,
                    };

                    context.mark_completed(&step.id, step_result);
                    successful_steps += 1;

                    if let Some(tx) = &ui_tx {
                        let _ = tx.send(format!("[DONE] Completed: {}", step.description));
                    }
                }
                Err(e) => {
                    let duration = (chrono::Utc::now() - step_start).num_seconds() as u64;
                    let error_msg = format!("Step '{}' failed: {}", step.id, e);
                    error!("{}", error_msg);

                    let step_result = StepResult {
                        step_id: step.id.clone(),
                        success: false,
                        output: String::new(),
                        artifacts: vec![],
                        duration,
                        error_message: Some(e.to_string()),
                    };

                    context.mark_completed(&step.id, step_result);

                    if let Some(tx) = &ui_tx {
                        let _ = tx.send(format!("[ERROR] Failed: {} - {}", step.description, e));
                    }

                    // Stop execution on error
                    break;
                }
            }
        }

        let total_duration =
            ((chrono::Utc::now() - context.start_time()).num_milliseconds() / 1000) as u64;
        let success = successful_steps == total_steps;

        let final_message = if success {
            format!(
                "[SUCCESS] Task completed! {}/{} steps succeeded",
                successful_steps, total_steps
            )
        } else {
            format!(
                "[WARNING] Task partially completed: {}/{} steps succeeded",
                successful_steps, total_steps
            )
        };

        if let Some(tx) = &ui_tx {
            let _ = tx.send(final_message.clone());
        }

        Ok(ExecutionResult {
            plan_id: plan.id,
            success,
            completed_steps: context.completed_steps(),
            total_duration,
            final_message,
        })
    }

    /// Check dependencies
    fn check_dependencies(&self, step: &TaskStep, context: &ExecutionContext) -> bool {
        for dep in &step.dependencies {
            if !context.is_completed(dep) {
                debug!("Dependency '{}' not satisfied for step '{}'", dep, step.id);
                return false;
            }

            // Check if dependent step has not failed
            if let Some(result) = context.get_result(dep)
                && !result.success
            {
                debug!("Dependency '{}' failed for step '{}'", dep, step.id);
                return false;
            }
        }
        true
    }

    /// Execute individual step
    async fn execute_step(
        &self,
        step: &TaskStep,
        context: &mut ExecutionContext,
        ui_tx: &Option<mpsc::Sender<String>>,
    ) -> Result<String> {
        debug!("Executing step: {} ({})", step.id, step.description);

        // Execute based on step type
        match step.step_type {
            StepType::Analysis => self.execute_analysis_step(step, context, ui_tx).await,
            StepType::Planning => self.execute_planning_step(step, context, ui_tx).await,
            StepType::Implementation => {
                self.execute_implementation_step(step, context, ui_tx).await
            }
            StepType::Validation => self.execute_validation_step(step, context, ui_tx).await,
            StepType::Cleanup => self.execute_cleanup_step(step, context, ui_tx).await,
        }
    }

    /// Execute analysis step
    async fn execute_analysis_step(
        &self,
        step: &TaskStep,
        context: &ExecutionContext,
        ui_tx: &Option<mpsc::Sender<String>>,
    ) -> Result<String> {
        let prompt = prompt_builder::build_analysis_prompt(step, context);
        self.execute_llm_step(&prompt, step, ui_tx).await
    }

    /// Execute planning step
    async fn execute_planning_step(
        &self,
        step: &TaskStep,
        context: &ExecutionContext,
        ui_tx: &Option<mpsc::Sender<String>>,
    ) -> Result<String> {
        let prompt = prompt_builder::build_planning_prompt(step, context);
        self.execute_llm_step(&prompt, step, ui_tx).await
    }

    /// Execute implementation step
    async fn execute_implementation_step(
        &self,
        step: &TaskStep,
        context: &ExecutionContext,
        ui_tx: &Option<mpsc::Sender<String>>,
    ) -> Result<String> {
        let prompt = prompt_builder::build_implementation_prompt(step, context);
        self.execute_llm_step(&prompt, step, ui_tx).await
    }

    /// Execute validation step
    async fn execute_validation_step(
        &self,
        step: &TaskStep,
        context: &ExecutionContext,
        ui_tx: &Option<mpsc::Sender<String>>,
    ) -> Result<String> {
        let prompt = prompt_builder::build_validation_prompt(step, context);
        let result = self.execute_llm_step(&prompt, step, ui_tx).await?;

        // Check validation criteria
        validator::validate_step_criteria(step, &result, &self.fs_tools).await?;

        Ok(result)
    }

    /// Execute cleanup step
    async fn execute_cleanup_step(
        &self,
        step: &TaskStep,
        context: &ExecutionContext,
        ui_tx: &Option<mpsc::Sender<String>>,
    ) -> Result<String> {
        let prompt = prompt_builder::build_cleanup_prompt(step, context);
        self.execute_llm_step(&prompt, step, ui_tx).await
    }

    /// Execute LLM step
    async fn execute_llm_step(
        &self,
        prompt: &str,
        step: &TaskStep,
        ui_tx: &Option<mpsc::Sender<String>>,
    ) -> Result<String> {
        debug!("Executing LLM step: {}", step.description);

        if let Some(tx) = ui_tx {
            let _ = tx.send(format!(
                "[LLM] Executing step with LLM: {}",
                step.description
            ));
        }

        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: Some(prompt.to_string()),
            tool_calls: vec![],
            tool_call_id: None,
        }];

        // Run LLM agent loop
        match run_agent_loop(
            &self.client,
            &self.model,
            &self.fs_tools,
            messages,
            ui_tx.clone(),
            None,
        )
        .await
        {
            Ok((_, final_message)) => {
                let result = final_message.content;
                debug!("LLM step completed successfully");

                if let Some(tx) = ui_tx {
                    let _ = tx.send(format!("[DONE] LLM step completed: {}", step.description));
                }

                Ok(result)
            }
            Err(e) => {
                error!("LLM step failed: {}", e);

                if let Some(tx) = ui_tx {
                    let _ = tx.send(format!(
                        "[ERROR] LLM step failed: {} - {}",
                        step.description, e
                    ));
                }

                Err(anyhow!("LLM step execution failed: {}", e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::OpenAIClient;
    use crate::planning::execution_context::ExecutionContext;
    use crate::planning::{
        RiskLevel, StepResult, StepType, TaskClassification, TaskPlan, TaskStep, TaskType,
    };
    use crate::tools::FsTools;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn create_test_executor() -> TaskExecutor {
        let client = OpenAIClient::new("http://test.com", "test-key").unwrap();
        let repomap = Arc::new(RwLock::new(None));
        let fs_tools = FsTools::new(repomap);

        TaskExecutor::new(client, "test-model".to_string(), fs_tools)
    }

    #[allow(dead_code)]
    fn create_test_plan() -> TaskPlan {
        let classification = TaskClassification {
            task_type: TaskType::SimpleCodeEdit,
            complexity_score: 0.5,
            estimated_steps: 2,
            risk_level: RiskLevel::Low,
            required_tools: vec!["fs_read".to_string()],
            confidence: 0.9,
        };

        let steps = vec![
            TaskStep::new(
                "analysis_1".to_string(),
                "Analyze the code".to_string(),
                StepType::Analysis,
                vec!["fs_read".to_string()],
            ),
            TaskStep::new(
                "implementation_1".to_string(),
                "Implement changes".to_string(),
                StepType::Implementation,
                vec!["edit".to_string()],
            )
            .with_dependencies(vec!["analysis_1".to_string()]),
        ];

        crate::planning::task_planner::create_task_plan(
            "Test task".to_string(),
            classification,
            steps,
        )
    }

    #[test]
    fn test_check_dependencies() {
        let executor = create_test_executor();
        let mut context = ExecutionContext::new();

        let step = TaskStep::new(
            "step2".to_string(),
            "Step 2".to_string(),
            StepType::Implementation,
            vec![],
        )
        .with_dependencies(vec!["step1".to_string()]);

        // If dependencies are not satisfied
        assert!(!executor.check_dependencies(&step, &context));

        // Satisfy dependencies
        context.mark_completed(
            "step1",
            StepResult {
                step_id: "step1".to_string(),
                success: true,
                output: "Success".to_string(),
                artifacts: vec![],
                duration: 100,
                error_message: None,
            },
        );

        // If dependencies are satisfied
        assert!(executor.check_dependencies(&step, &context));
    }
}
