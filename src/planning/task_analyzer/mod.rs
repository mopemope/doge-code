mod confidence;
mod decomposition;
mod llm;
mod patterns;
mod tools_mapping;

use crate::analysis::RepoMap;
use crate::llm::OpenAIClient;
use crate::tools::FsTools;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

use confidence::*;
use decomposition::*;
use llm::*;
use patterns::*;
use tools_mapping::*;

use crate::planning::task_types::*;

#[derive(Clone)]
pub struct TaskAnalyzer {
    keyword_patterns: HashMap<String, TaskType>,
    tool_mappings: HashMap<TaskType, Vec<String>>,
    llm_decomposer: Option<crate::planning::llm_decomposer::LlmTaskDecomposer>,
}

impl TaskAnalyzer {
    pub fn new() -> Self {
        let mut analyzer = Self {
            keyword_patterns: HashMap::new(),
            tool_mappings: HashMap::new(),
            llm_decomposer: None,
        };

        initialize_patterns(&mut analyzer.keyword_patterns);
        initialize_tool_mappings(&mut analyzer.tool_mappings);

        analyzer
    }

    pub fn with_llm_decomposer(
        mut self,
        client: OpenAIClient,
        model: String,
        fs_tools: FsTools,
        repomap: Arc<RwLock<Option<RepoMap>>>,
    ) -> Self {
        with_llm_decomposer(&mut self.llm_decomposer, client, model, fs_tools, repomap);
        self
    }

    pub fn analyze(&self, user_input: &str) -> Result<TaskClassification> {
        debug!("Analyzing task: {}", user_input);

        let keywords = extract_keywords(&self.keyword_patterns, user_input);
        let task_type = classify_by_keywords(&self.keyword_patterns, &keywords);
        let complexity = estimate_complexity(user_input, &task_type);
        let required_tools = get_required_tools(&self.tool_mappings, &task_type);

        let classification = TaskClassification {
            task_type: task_type.clone(),
            complexity_score: complexity,
            estimated_steps: task_type.estimated_steps(),
            risk_level: task_type.risk_level(),
            required_tools,
            confidence: calculate_confidence(&self.keyword_patterns, &keywords, &task_type),
        };

        info!("Task classification: {:?}", classification);
        Ok(classification)
    }

    pub async fn decompose(
        &self,
        classification: &TaskClassification,
        user_input: &str,
    ) -> Result<Vec<TaskStep>> {
        debug!("Decomposing task: {:?}", classification.task_type);

        if should_use_llm_decomposition(classification)
            && let Some(llm_decomposer) = &self.llm_decomposer
        {
            info!("Using LLM-assisted decomposition for complex task");
            match llm_decomposer
                .decompose_complex_task(user_input, classification)
                .await
            {
                Ok(steps) => {
                    info!("LLM decomposition successful with {} steps", steps.len());
                    return Ok(steps);
                }
                Err(e) => {
                    debug!(
                        "LLM decomposition failed: {}, falling back to rule-based",
                        e
                    );
                    // fallback to rule-based
                }
            }
        }

        // Rule-based decomposition
        let steps = match classification.task_type {
            TaskType::SimpleFileOperation => decompose_file_operation(user_input),
            TaskType::SimpleSearch => decompose_search_task(user_input),
            TaskType::SimpleCodeEdit => decompose_code_edit(user_input),
            TaskType::MultiFileEdit => decompose_multi_file_edit(user_input),
            TaskType::Refactoring => decompose_refactoring(user_input),
            TaskType::FeatureImplementation => decompose_feature_implementation(user_input),
            _ => decompose_complex_fallback(user_input, classification),
        };

        Ok(steps)
    }
}

impl Default for TaskAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_task_decomposition() {
        let analyzer = TaskAnalyzer::new();

        let classification = analyzer.analyze("src/main.rsを編集してください").unwrap();
        let steps = analyzer
            .decompose(&classification, "src/main.rsを編集してください")
            .await
            .unwrap();

        assert!(!steps.is_empty());
        assert!(steps.len() >= 3); // Analysis, planning, implementation, validation

        // Check step types
        let step_types: Vec<_> = steps.iter().map(|s| &s.step_type).collect();
        assert!(step_types.contains(&&StepType::Analysis));
        assert!(step_types.contains(&&StepType::Implementation));
    }

    #[test]
    fn test_keyword_extraction() {
        let analyzer = TaskAnalyzer::new();

        let keywords = extract_keywords(
            &analyzer.keyword_patterns,
            "ファイルを読んで編集してください",
        );
        println!("Extracted keywords: {:?}", keywords);

        // Verify that "read" and "edit" are extracted
        assert!(keywords.contains(&"読む".to_string()));
        assert!(keywords.contains(&"編集".to_string()));
    }

    #[test]
    fn test_task_classification() {
        let analyzer = TaskAnalyzer::new();

        // Test simple file operation
        let result = analyzer.analyze("ファイルを読む").unwrap();
        println!("Classification for 'ファイルを読む': {:?}", result);
        assert_eq!(result.task_type, TaskType::SimpleFileOperation);

        // Test code editing
        let result = analyzer.analyze("コードを編集する").unwrap();
        println!("Classification for 'コードを編集する': {:?}", result);
        assert_eq!(result.task_type, TaskType::SimpleCodeEdit);

        // Test search
        let result = analyzer.analyze("ファイルを検索する").unwrap();
        println!("Classification for 'ファイルを検索する': {:?}", result);
        assert_eq!(result.task_type, TaskType::SimpleSearch);

        // Test refactoring
        let result = analyzer.analyze("コードをリファクタリングする").unwrap();
        println!(
            "Classification for 'コードをリファクタリングする': {:?}",
            result
        );
        assert_eq!(result.task_type, TaskType::Refactoring);
    }

    #[tokio::test]
    async fn test_complexity_estimation() {
        let analyzer = TaskAnalyzer::new();

        let simple_task = analyzer.analyze("ファイルを読む").unwrap();
        let complex_task = analyzer
            .analyze("複数のファイルを大規模にリファクタリングして新しいアーキテクチャに変更する")
            .unwrap();

        assert!(simple_task.complexity_score < complex_task.complexity_score);
    }
}
