use crate::llm::{ChatMessage, OpenAIClient, run_agent_loop};
use crate::planning::task_types::*;
use crate::tools::FsTools;
use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::sync::mpsc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Execution context
#[derive(Debug)]
pub struct ExecutionContext {
    completed_steps: HashMap<String, StepResult>,
    artifacts: Vec<String>,
    start_time: chrono::DateTime<chrono::Utc>,
}

impl Default for ExecutionContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionContext {
    pub fn new() -> Self {
        Self {
            completed_steps: HashMap::new(),
            artifacts: Vec::new(),
            start_time: chrono::Utc::now(),
        }
    }

    pub fn mark_completed(&mut self, step_id: &str, result: StepResult) {
        self.completed_steps.insert(step_id.to_string(), result);
    }

    pub fn is_completed(&self, step_id: &str) -> bool {
        self.completed_steps.contains_key(step_id)
    }

    pub fn get_result(&self, step_id: &str) -> Option<&StepResult> {
        self.completed_steps.get(step_id)
    }

    pub fn completed_steps(&self) -> Vec<StepResult> {
        self.completed_steps.values().cloned().collect()
    }

    pub fn artifacts(&self) -> Vec<String> {
        self.artifacts.clone()
    }

    pub fn add_artifact(&mut self, artifact: String) {
        self.artifacts.push(artifact);
    }
}

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
                    total_duration: (chrono::Utc::now() - context.start_time).num_seconds() as u64,
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
                        artifacts: vec![], // TODO: 実際のアーティファクトを収集
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

        let total_duration = (chrono::Utc::now() - context.start_time).num_seconds() as u64;
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
        let prompt = self.build_analysis_prompt(step, context);
        self.execute_llm_step(&prompt, step, ui_tx).await
    }

    /// Execute planning step
    async fn execute_planning_step(
        &self,
        step: &TaskStep,
        context: &ExecutionContext,
        ui_tx: &Option<mpsc::Sender<String>>,
    ) -> Result<String> {
        let prompt = self.build_planning_prompt(step, context);
        self.execute_llm_step(&prompt, step, ui_tx).await
    }

    /// Execute implementation step
    async fn execute_implementation_step(
        &self,
        step: &TaskStep,
        context: &ExecutionContext,
        ui_tx: &Option<mpsc::Sender<String>>,
    ) -> Result<String> {
        let prompt = self.build_implementation_prompt(step, context);
        self.execute_llm_step(&prompt, step, ui_tx).await
    }

    /// Execute validation step
    async fn execute_validation_step(
        &self,
        step: &TaskStep,
        context: &ExecutionContext,
        ui_tx: &Option<mpsc::Sender<String>>,
    ) -> Result<String> {
        let prompt = self.build_validation_prompt(step, context);
        let result = self.execute_llm_step(&prompt, step, ui_tx).await?;

        // Check validation criteria
        self.validate_step_criteria(step, &result).await?;

        Ok(result)
    }

    /// Execute cleanup step
    async fn execute_cleanup_step(
        &self,
        step: &TaskStep,
        context: &ExecutionContext,
        ui_tx: &Option<mpsc::Sender<String>>,
    ) -> Result<String> {
        let prompt = self.build_cleanup_prompt(step, context);
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

    /// Build analysis prompt
    fn build_analysis_prompt(&self, step: &TaskStep, context: &ExecutionContext) -> String {
        let mut prompt = format!(
            r#"# 分析タスク実行

## ステップ情報
- **ID**: {}
- **説明**: {}
- **タイプ**: 分析

## 実行指示
{}

## 利用可能なツール
{}

## 前提条件
- プロジェクトのコードベースを詳細に分析してください
- 必要に応じて関連ファイルを読み込んでください
- 分析結果は具体的で実用的な情報を含めてください

## 完了条件
{}

分析を実行し、結果を詳細に報告してください。
"#,
            step.id,
            step.description,
            step.prompt_template.as_deref().unwrap_or(&step.description),
            step.required_tools.join(", "),
            step.validation_criteria.join("\n- ")
        );

        // Include results from previous steps
        if !context.completed_steps.is_empty() {
            prompt.push_str("\n## 前のステップの結果\n");
            for (i, result) in context.completed_steps().iter().enumerate() {
                if result.success {
                    prompt.push_str(&format!(
                        "{}. {}: {}\n",
                        i + 1,
                        result.step_id,
                        result.output
                    ));
                }
            }
        }

        prompt
    }

    /// Build planning prompt
    fn build_planning_prompt(&self, step: &TaskStep, context: &ExecutionContext) -> String {
        let mut prompt = format!(
            r#"# 計画タスク実行

## ステップ情報
- **ID**: {}
- **説明**: {}
- **タイプ**: 計画

## 実行指示
{}

## 利用可能なツール
{}

## 前提条件
- 分析結果に基づいて詳細な実行計画を作成してください
- 実装の順序と依存関係を明確にしてください
- リスクと対策を含めてください

## 完了条件
{}

詳細な実行計画を作成してください。
"#,
            step.id,
            step.description,
            step.prompt_template.as_deref().unwrap_or(&step.description),
            step.required_tools.join(", "),
            step.validation_criteria.join("\n- ")
        );

        // Include analysis results
        if !context.completed_steps.is_empty() {
            prompt.push_str("\n## 分析結果\n");
            for result in context.completed_steps() {
                if result.success && result.step_id.contains("analysis") {
                    prompt.push_str(&format!("- {}: {}\n", result.step_id, result.output));
                }
            }
        }

        prompt
    }

    /// Build implementation prompt
    fn build_implementation_prompt(&self, step: &TaskStep, context: &ExecutionContext) -> String {
        let mut prompt = format!(
            r#"# 実装タスク実行

## ステップ情報
- **ID**: {}
- **説明**: {}
- **タイプ**: 実装

## 実行指示
{}

## 利用可能なツール
{}

## 重要な注意事項
- ファイルを変更する前に必ず現在の内容を確認してください
- 段階的に実装し、各段階でコンパイル/テストを実行してください
- エラーが発生した場合は適切に修正してください
- 変更内容は明確にドキュメント化してください

## 完了条件
{}

実装を実行してください。必ずコンパイルが通ることを確認してください。
"#,
            step.id,
            step.description,
            step.prompt_template.as_deref().unwrap_or(&step.description),
            step.required_tools.join(", "),
            step.validation_criteria.join("\n- ")
        );

        // Include planning results
        if !context.completed_steps.is_empty() {
            prompt.push_str("\n## 実行計画\n");
            for result in context.completed_steps() {
                if result.success
                    && (result.step_id.contains("planning") || result.step_id.contains("analysis"))
                {
                    prompt.push_str(&format!("- {}: {}\n", result.step_id, result.output));
                }
            }
        }

        prompt
    }

    /// Build validation prompt
    fn build_validation_prompt(&self, step: &TaskStep, context: &ExecutionContext) -> String {
        let mut prompt = format!(
            r#"# 検証タスク実行

## ステップ情報
- **ID**: {}
- **説明**: {}
- **タイプ**: 検証

## 実行指示
{}

## 利用可能なツール
{}

## 検証項目
- 実装が正しく動作することを確認してください
- コンパイルエラーがないことを確認してください
- テストが通ることを確認してください
- 期待される動作を満たしていることを確認してください

## 完了条件
{}

包括的な検証を実行し、結果を報告してください。
"#,
            step.id,
            step.description,
            step.prompt_template.as_deref().unwrap_or(&step.description),
            step.required_tools.join(", "),
            step.validation_criteria.join("\n- ")
        );

        // Include implementation results
        if !context.completed_steps.is_empty() {
            prompt.push_str("\n## 実装結果\n");
            for result in context.completed_steps() {
                if result.success && result.step_id.contains("implementation") {
                    prompt.push_str(&format!("- {}: {}\n", result.step_id, result.output));
                }
            }
        }

        prompt
    }

    /// クリーンアップ プロンプトを構築
    /// Build cleanup prompt
    fn build_cleanup_prompt(&self, step: &TaskStep, _context: &ExecutionContext) -> String {
        format!(
            r#"# クリーンアップタスク実行

## ステップ情報
- **ID**: {}
- **説明**: {}
- **タイプ**: クリーンアップ

## 実行指示
{}

## 利用可能なツール
{}

## クリーンアップ項目
- 不要なファイルやコメントを削除してください
- コードフォーマットを整えてください
- ドキュメントを更新してください
- 最終的な動作確認を行ってください

## 完了条件
{}

クリーンアップを実行し、プロジェクトを整理してください。
"#,
            step.id,
            step.description,
            step.prompt_template.as_deref().unwrap_or(&step.description),
            step.required_tools.join(", "),
            step.validation_criteria.join("\n- ")
        )
    }

    /// Check step validation criteria
    async fn validate_step_criteria(&self, step: &TaskStep, result: &str) -> Result<()> {
        for criteria in &step.validation_criteria {
            match criteria.as_str() {
                "コンパイル成功" | "構文エラーなし" => {
                    // Run cargo check
                    if let Err(e) = self.fs_tools.execute_bash("cargo check").await {
                        return Err(anyhow!("Compilation failed: {}", e));
                    }
                }
                "テストが通る" => {
                    // Run cargo test
                    if let Err(e) = self.fs_tools.execute_bash("cargo test").await {
                        return Err(anyhow!("Tests failed: {}", e));
                    }
                }
                "ファイルが存在する" => {
                    // Extract file paths from results and check existence
                    // Simple implementation: Improve later
                    debug!("File existence check: {}", result);
                }
                _ => {
                    // Check if other criteria are contained in the result text
                    if !result.to_lowercase().contains(&criteria.to_lowercase()) {
                        warn!("Validation criteria '{}' not met in result", criteria);
                    }
                }
            }
        }
        Ok(())
    }
}

/// Create task plan
pub fn create_task_plan(
    original_request: String,
    classification: TaskClassification,
    steps: Vec<TaskStep>,
) -> TaskPlan {
    let total_duration = steps.iter().map(|s| s.estimated_duration).sum();

    TaskPlan {
        id: Uuid::new_v4().to_string(),
        original_request,
        classification,
        steps,
        total_estimated_duration: total_duration,
        created_at: chrono::Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::OpenAIClient;
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

        create_task_plan("Test task".to_string(), classification, steps)
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

    #[test]
    fn test_build_analysis_prompt() {
        let executor = create_test_executor();
        let context = ExecutionContext::new();

        let step = TaskStep::new(
            "analysis_1".to_string(),
            "Analyze the code structure".to_string(),
            StepType::Analysis,
            vec!["fs_read".to_string(), "get_symbol_info".to_string()],
        )
        .with_validation(vec!["コードの構造を理解する".to_string()]);

        let prompt = executor.build_analysis_prompt(&step, &context);

        assert!(prompt.contains("analysis_1"));
        assert!(prompt.contains("Analyze the code structure"));
        assert!(prompt.contains("fs_read, get_symbol_info"));
        assert!(prompt.contains("コードの構造を理解する"));
    }

    #[test]
    fn test_build_implementation_prompt() {
        let executor = create_test_executor();
        let mut context = ExecutionContext::new();

        // Add analysis results
        context.mark_completed(
            "analysis_1",
            StepResult {
                step_id: "analysis_1".to_string(),
                success: true,
                output: "Code analysis completed".to_string(),
                artifacts: vec![],
                duration: 100,
                error_message: None,
            },
        );

        let step = TaskStep::new(
            "implementation_1".to_string(),
            "Implement the feature".to_string(),
            StepType::Implementation,
            vec!["edit".to_string()],
        )
        .with_dependencies(vec!["analysis_1".to_string()]);

        let prompt = executor.build_implementation_prompt(&step, &context);

        assert!(prompt.contains("implementation_1"));
        assert!(prompt.contains("Implement the feature"));
        assert!(prompt.contains("edit"));
        assert!(prompt.contains("Code analysis completed"));
    }

    #[test]
    fn test_execution_context() {
        let mut context = ExecutionContext::new();

        // Initial state
        assert!(!context.is_completed("step1"));
        assert!(context.completed_steps().is_empty());

        // Record step completion
        let result = StepResult {
            step_id: "step1".to_string(),
            success: true,
            output: "Success".to_string(),
            artifacts: vec!["file1.rs".to_string()],
            duration: 150,
            error_message: None,
        };

        context.mark_completed("step1", result.clone());

        // Verify completion state
        assert!(context.is_completed("step1"));
        assert_eq!(context.completed_steps().len(), 1);
        assert_eq!(context.get_result("step1").unwrap().step_id, "step1");

        // Add artifacts
        context.add_artifact("new_file.rs".to_string());
        assert!(context.artifacts().contains(&"new_file.rs".to_string()));
    }

    #[test]
    fn test_create_task_plan() {
        let classification = TaskClassification {
            task_type: TaskType::LargeRefactoring,
            complexity_score: 0.8,
            estimated_steps: 3,
            risk_level: RiskLevel::Medium,
            required_tools: vec!["fs_read".to_string(), "edit".to_string()],
            confidence: 0.85,
        };

        let steps = vec![
            TaskStep::new(
                "step1".to_string(),
                "Step 1".to_string(),
                StepType::Analysis,
                vec!["fs_read".to_string()],
            )
            .with_duration(300),
            TaskStep::new(
                "step2".to_string(),
                "Step 2".to_string(),
                StepType::Implementation,
                vec!["edit".to_string()],
            )
            .with_duration(600),
        ];

        let plan = create_task_plan("Refactor the codebase".to_string(), classification, steps);

        assert_eq!(plan.original_request, "Refactor the codebase");
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.total_estimated_duration, 900); // 300 + 600
        assert!(!plan.id.is_empty());
    }
}
