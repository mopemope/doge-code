use crate::analysis::RepoMap;
use crate::llm::OpenAIClient;
use crate::planning::llm_decomposer::LlmTaskDecomposer;
use crate::planning::task_types::*;
use crate::tools::FsTools;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

/// Task analysis engine
#[derive(Clone)]
pub struct TaskAnalyzer {
    /// Keyword pattern mapping
    keyword_patterns: HashMap<String, TaskType>,
    /// Tool mapping
    tool_mappings: HashMap<TaskType, Vec<String>>,
    /// LLM decomposer (optional)
    llm_decomposer: Option<LlmTaskDecomposer>,
}

impl TaskAnalyzer {
    /// Create a new task analysis engine
    pub fn new() -> Self {
        let mut analyzer = Self {
            keyword_patterns: HashMap::new(),
            tool_mappings: HashMap::new(),
            llm_decomposer: None,
        };
        analyzer.initialize_patterns();
        analyzer.initialize_tool_mappings();
        analyzer
    }

    /// Set LLM decomposer
    pub fn with_llm_decomposer(
        mut self,
        client: OpenAIClient,
        model: String,
        fs_tools: FsTools,
        repomap: Arc<RwLock<Option<RepoMap>>>,
    ) -> Self {
        self.llm_decomposer = Some(LlmTaskDecomposer::new(client, model, fs_tools, repomap));
        self
    }

    /// Initialize keyword patterns
    fn initialize_patterns(&mut self) {
        // File operations
        let file_ops = vec![
            "読む",
            "read",
            "読み込み",
            "表示",
            "show",
            "見る",
            "確認",
            "書く",
            "write",
            "作成",
            "create",
            "保存",
            "save",
        ];
        for keyword in file_ops {
            self.keyword_patterns
                .insert(keyword.to_string(), TaskType::SimpleFileOperation);
        }

        // Search operations
        let search_ops = vec![
            "検索",
            "search",
            "探す",
            "find",
            "調べる",
            "investigate",
            "grep",
        ];
        for keyword in search_ops {
            self.keyword_patterns
                .insert(keyword.to_string(), TaskType::SimpleSearch);
        }

        // Edit operations
        let edit_ops = vec![
            "編集", "edit", "修正", "fix", "変更", "change", "更新", "update", "追加", "add",
            "削除", "delete", "remove",
        ];
        for keyword in edit_ops {
            self.keyword_patterns
                .insert(keyword.to_string(), TaskType::SimpleCodeEdit);
        }

        // Refactoring operations
        let refactor_ops = vec![
            "リファクタ",
            "refactor",
            "整理",
            "cleanup",
            "分割",
            "split",
            "統合",
            "merge",
            "最適化",
            "optimize",
        ];
        for keyword in refactor_ops {
            self.keyword_patterns
                .insert(keyword.to_string(), TaskType::Refactoring);
        }

        // Implementation operations
        let impl_ops = vec![
            "実装",
            "implement",
            "機能",
            "feature",
            "新しい",
            "new",
            "開発",
            "develop",
            "構築",
            "build",
        ];
        for keyword in impl_ops {
            self.keyword_patterns
                .insert(keyword.to_string(), TaskType::FeatureImplementation);
        }
    }

    /// Initialize tool mappings
    fn initialize_tool_mappings(&mut self) {
        self.tool_mappings.insert(
            TaskType::SimpleFileOperation,
            vec![
                "fs_read".to_string(),
                "fs_write".to_string(),
                "fs_list".to_string(),
            ],
        );

        self.tool_mappings.insert(
            TaskType::SimpleSearch,
            vec![
                "search_text".to_string(),
                "find_file".to_string(),
                "get_symbol_info".to_string(),
            ],
        );

        self.tool_mappings.insert(
            TaskType::SimpleCodeEdit,
            vec![
                "fs_read".to_string(),
                "edit".to_string(),
                "execute_bash".to_string(),
            ],
        );

        self.tool_mappings.insert(
            TaskType::MultiFileEdit,
            vec![
                "fs_read".to_string(),
                "edit".to_string(),
                "search_text".to_string(),
                "execute_bash".to_string(),
            ],
        );

        self.tool_mappings.insert(
            TaskType::Refactoring,
            vec![
                "fs_read".to_string(),
                "fs_write".to_string(),
                "edit".to_string(),
                "search_text".to_string(),
                "get_symbol_info".to_string(),
                "execute_bash".to_string(),
            ],
        );

        self.tool_mappings.insert(
            TaskType::FeatureImplementation,
            vec![
                "fs_read".to_string(),
                "fs_write".to_string(),
                "edit".to_string(),
                "get_symbol_info".to_string(),
                "execute_bash".to_string(),
            ],
        );
    }

    /// Analyze task
    pub fn analyze(&self, user_input: &str) -> Result<TaskClassification> {
        debug!("Analyzing task: {}", user_input);

        let keywords = self.extract_keywords(user_input);
        let task_type = self.classify_by_keywords(&keywords);
        let complexity = self.estimate_complexity(user_input, &task_type);
        let required_tools = self.get_required_tools(&task_type);

        let classification = TaskClassification {
            task_type: task_type.clone(),
            complexity_score: complexity,
            estimated_steps: task_type.estimated_steps(),
            risk_level: task_type.risk_level(),
            required_tools,
            confidence: self.calculate_confidence(&keywords, &task_type),
        };

        info!("Task classification: {:?}", classification);
        Ok(classification)
    }

    /// Classify task type
    pub async fn decompose(
        &self,
        classification: &TaskClassification,
        user_input: &str,
    ) -> Result<Vec<TaskStep>> {
        debug!("Decomposing task: {:?}", classification.task_type);

        // Use LLM if available for complex tasks
        if self.should_use_llm_decomposition(classification)
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
                    // Use fallback method
                }
            }
        }

        // Rule-based decomposition
        let steps = match classification.task_type {
            TaskType::SimpleFileOperation => self.decompose_file_operation(user_input),
            TaskType::SimpleSearch => self.decompose_search_task(user_input),
            TaskType::SimpleCodeEdit => self.decompose_code_edit(user_input),
            TaskType::MultiFileEdit => self.decompose_multi_file_edit(user_input),
            TaskType::Refactoring => self.decompose_refactoring(user_input),
            TaskType::FeatureImplementation => self.decompose_feature_implementation(user_input),
            _ => {
                // For complex tasks when LLM is not available
                self.decompose_complex_fallback(user_input, classification)
            }
        };

        Ok(steps)
    }

    /// Determine whether to use LLM decomposition
    fn should_use_llm_decomposition(&self, classification: &TaskClassification) -> bool {
        // Use LLM decomposition under the following conditions
        match classification.task_type {
            TaskType::ArchitecturalChange
            | TaskType::LargeRefactoring
            | TaskType::ProjectRestructure => true,
            TaskType::Refactoring | TaskType::FeatureImplementation => {
                // When complexity is high
                classification.complexity_score > 0.7
            }
            TaskType::MultiFileEdit => {
                // When complexity is high or estimated steps are many
                classification.complexity_score > 0.6 || classification.estimated_steps > 8
            }
            _ => false,
        }
    }

    /// Fallback decomposition for complex tasks
    fn decompose_complex_fallback(
        &self,
        user_input: &str,
        classification: &TaskClassification,
    ) -> Vec<TaskStep> {
        info!("Using fallback decomposition for complex task");

        match classification.task_type {
            TaskType::ArchitecturalChange => {
                self.decompose_architectural_change_fallback(user_input)
            }
            TaskType::LargeRefactoring => self.decompose_large_refactoring_fallback(user_input),
            TaskType::ProjectRestructure => self.decompose_project_restructure_fallback(user_input),
            _ => {
                // Generic complex task decomposition
                vec![
                    TaskStep::new(
                        "analyze_requirements".to_string(),
                        "Analyze requirements and current state in detail".to_string(),
                        StepType::Analysis,
                        vec!["search_repomap".to_string(), "fs_read".to_string()],
                    )
                    .with_duration(300),
                    TaskStep::new(
                        "create_detailed_plan".to_string(),
                        "Create detailed execution plan".to_string(),
                        StepType::Planning,
                        vec!["get_symbol_info".to_string()],
                    )
                    .with_dependencies(vec!["analyze_requirements".to_string()])
                    .with_duration(600),
                    TaskStep::new(
                        "implement_incrementally".to_string(),
                        "Implement incrementally".to_string(),
                        StepType::Implementation,
                        vec!["edit".to_string(), "fs_write".to_string()],
                    )
                    .with_dependencies(vec!["create_detailed_plan".to_string()])
                    .with_duration(1800),
                    TaskStep::new(
                        "comprehensive_testing".to_string(),
                        "Comprehensive testing and validation".to_string(),
                        StepType::Validation,
                        vec!["execute_bash".to_string()],
                    )
                    .with_dependencies(vec!["implement_incrementally".to_string()])
                    .with_duration(600)
                    .with_validation(vec![
                        "All tests pass".to_string(),
                        "Compilation successful".to_string(),
                    ]),
                ]
            }
        }
    }

    /// Fallback decomposition for architectural changes
    fn decompose_architectural_change_fallback(&self, _user_input: &str) -> Vec<TaskStep> {
        vec![
            TaskStep::new(
                "analyze_current_architecture".to_string(),
                "Analyze current architecture in detail".to_string(),
                StepType::Analysis,
                vec!["search_repomap".to_string(), "get_symbol_info".to_string()],
            )
            .with_duration(600),
            TaskStep::new(
                "design_new_architecture".to_string(),
                "Design new architecture".to_string(),
                StepType::Planning,
                vec!["get_symbol_info".to_string()],
            )
            .with_dependencies(vec!["analyze_current_architecture".to_string()])
            .with_duration(900),
            TaskStep::new(
                "create_migration_plan".to_string(),
                "Create migration plan".to_string(),
                StepType::Planning,
                vec!["search_text".to_string()],
            )
            .with_dependencies(vec!["design_new_architecture".to_string()])
            .with_duration(600),
            TaskStep::new(
                "implement_core_changes".to_string(),
                "Implement core changes".to_string(),
                StepType::Implementation,
                vec!["edit".to_string(), "fs_write".to_string()],
            )
            .with_dependencies(vec!["create_migration_plan".to_string()])
            .with_duration(2400),
            TaskStep::new(
                "update_dependent_modules".to_string(),
                "Update dependent modules".to_string(),
                StepType::Implementation,
                vec!["search_text".to_string(), "edit".to_string()],
            )
            .with_dependencies(vec!["implement_core_changes".to_string()])
            .with_duration(1800),
            TaskStep::new(
                "comprehensive_integration_test".to_string(),
                "Comprehensive integration testing".to_string(),
                StepType::Validation,
                vec!["execute_bash".to_string()],
            )
            .with_dependencies(vec!["update_dependent_modules".to_string()])
            .with_duration(900)
            .with_validation(vec![
                "All tests pass".to_string(),
                "Architecture works correctly".to_string(),
            ]),
        ]
    }

    /// Fallback decomposition for large refactoring
    fn decompose_large_refactoring_fallback(&self, _user_input: &str) -> Vec<TaskStep> {
        vec![
            TaskStep::new(
                "comprehensive_code_analysis".to_string(),
                "Comprehensive code analysis".to_string(),
                StepType::Analysis,
                vec!["search_repomap".to_string(), "search_text".to_string()],
            )
            .with_duration(600),
            TaskStep::new(
                "identify_refactoring_targets".to_string(),
                "Identify refactoring targets".to_string(),
                StepType::Analysis,
                vec!["get_symbol_info".to_string()],
            )
            .with_dependencies(vec!["comprehensive_code_analysis".to_string()])
            .with_duration(450),
            TaskStep::new(
                "prioritize_refactoring_tasks".to_string(),
                "Prioritize refactoring tasks".to_string(),
                StepType::Planning,
                vec!["get_symbol_info".to_string()],
            )
            .with_dependencies(vec!["identify_refactoring_targets".to_string()])
            .with_duration(300),
            TaskStep::new(
                "refactor_high_priority_modules".to_string(),
                "Refactor high-priority modules".to_string(),
                StepType::Implementation,
                vec!["edit".to_string(), "create_patch".to_string()],
            )
            .with_dependencies(vec!["prioritize_refactoring_tasks".to_string()])
            .with_duration(2100),
            TaskStep::new(
                "update_tests_and_documentation".to_string(),
                "Update tests and documentation".to_string(),
                StepType::Implementation,
                vec!["edit".to_string(), "fs_write".to_string()],
            )
            .with_dependencies(vec!["refactor_high_priority_modules".to_string()])
            .with_duration(900),
            TaskStep::new(
                "validate_refactoring_results".to_string(),
                "Validate refactoring results".to_string(),
                StepType::Validation,
                vec!["execute_bash".to_string()],
            )
            .with_dependencies(vec!["update_tests_and_documentation".to_string()])
            .with_duration(600)
            .with_validation(vec![
                "All tests pass".to_string(),
                "Code quality improved".to_string(),
            ]),
        ]
    }

    /// Fallback decomposition for project structure changes
    fn decompose_project_restructure_fallback(&self, _user_input: &str) -> Vec<TaskStep> {
        vec![
            TaskStep::new(
                "analyze_current_structure".to_string(),
                "Analyze current project structure".to_string(),
                StepType::Analysis,
                vec!["fs_list".to_string(), "search_repomap".to_string()],
            )
            .with_duration(450),
            TaskStep::new(
                "design_new_structure".to_string(),
                "Design new project structure".to_string(),
                StepType::Planning,
                vec!["get_symbol_info".to_string()],
            )
            .with_dependencies(vec!["analyze_current_structure".to_string()])
            .with_duration(600),
            TaskStep::new(
                "create_backup_plan".to_string(),
                "Create backup plan".to_string(),
                StepType::Planning,
                vec!["fs_list".to_string()],
            )
            .with_dependencies(vec!["design_new_structure".to_string()])
            .with_duration(300),
            TaskStep::new(
                "create_new_directories".to_string(),
                "Create new directory structure".to_string(),
                StepType::Implementation,
                vec!["execute_bash".to_string()],
            )
            .with_dependencies(vec!["create_backup_plan".to_string()])
            .with_duration(300),
            TaskStep::new(
                "move_and_reorganize_files".to_string(),
                "Move and reorganize files".to_string(),
                StepType::Implementation,
                vec!["execute_bash".to_string(), "fs_write".to_string()],
            )
            .with_dependencies(vec!["create_new_directories".to_string()])
            .with_duration(1800),
            TaskStep::new(
                "update_import_paths".to_string(),
                "Update import paths".to_string(),
                StepType::Implementation,
                vec!["search_text".to_string(), "edit".to_string()],
            )
            .with_dependencies(vec!["move_and_reorganize_files".to_string()])
            .with_duration(1200),
            TaskStep::new(
                "update_build_configuration".to_string(),
                "Update build configuration".to_string(),
                StepType::Implementation,
                vec!["edit".to_string()],
            )
            .with_dependencies(vec!["update_import_paths".to_string()])
            .with_duration(600),
            TaskStep::new(
                "comprehensive_build_test".to_string(),
                "Comprehensive build testing".to_string(),
                StepType::Validation,
                vec!["execute_bash".to_string()],
            )
            .with_dependencies(vec!["update_build_configuration".to_string()])
            .with_duration(900)
            .with_validation(vec![
                "Project builds successfully".to_string(),
                "All tests pass".to_string(),
            ]),
        ]
    }

    /// Extract keywords
    fn extract_keywords(&self, input: &str) -> Vec<String> {
        let input_lower = input.to_lowercase();

        let mut keywords = Vec::new();

        // Check for partial matches across the entire input text for each pattern
        for pattern in self.keyword_patterns.keys() {
            if input_lower.contains(pattern) {
                keywords.push(pattern.clone());
            }
        }

        // Add stem matching for Japanese conjugations
        let japanese_stems = [
            ("読ん", "読む"),
            ("書い", "書く"),
            ("作っ", "作成"),
            ("見", "見る"),
            ("編集", "編集"),
            ("修正", "修正"),
            ("変更", "変更"),
            ("追加", "追加"),
            ("削除", "削除"),
            ("検索", "検索"),
            ("探", "探す"),
            ("調べ", "調べる"),
        ];

        for (stem, base) in &japanese_stems {
            if input_lower.contains(stem)
                && self.keyword_patterns.contains_key(*base)
                && !keywords.contains(&base.to_string())
            {
                keywords.push(base.to_string());
            }
        }

        keywords.sort();
        keywords.dedup();
        keywords
    }

    /// Classify by keywords
    fn classify_by_keywords(&self, keywords: &[String]) -> TaskType {
        let mut type_scores: HashMap<TaskType, usize> = HashMap::new();

        for keyword in keywords {
            if let Some(task_type) = self.keyword_patterns.get(keyword) {
                *type_scores.entry(task_type.clone()).or_insert(0) += 1;
            }
        }

        // Select the task type with the highest score
        type_scores
            .into_iter()
            .max_by_key(|(_, score)| *score)
            .map(|(task_type, _)| task_type)
            .unwrap_or(TaskType::Unknown)
    }

    /// Estimate complexity
    fn estimate_complexity(&self, input: &str, task_type: &TaskType) -> f32 {
        let base_complexity = task_type.base_complexity();

        // Adjustment based on input length
        let length_factor = (input.len() as f32 / 100.0).min(0.3);

        // Keywords suggesting multiple files
        let multi_file_keywords = ["複数", "全て", "すべて", "multiple", "all"];
        let multi_file_factor = if multi_file_keywords.iter().any(|k| input.contains(k)) {
            0.2
        } else {
            0.0
        };

        (base_complexity + length_factor + multi_file_factor).min(1.0)
    }

    /// Get required tools
    fn get_required_tools(&self, task_type: &TaskType) -> Vec<String> {
        self.tool_mappings
            .get(task_type)
            .cloned()
            .unwrap_or_else(|| vec!["fs_read".to_string()])
    }

    /// Calculate classification confidence
    fn calculate_confidence(&self, keywords: &[String], task_type: &TaskType) -> f32 {
        if keywords.is_empty() {
            return 0.1;
        }

        let matching_keywords = keywords
            .iter()
            .filter(|k| {
                self.keyword_patterns
                    .get(*k)
                    .map(|t| t == task_type)
                    .unwrap_or(false)
            })
            .count();

        (matching_keywords as f32 / keywords.len() as f32).max(0.1)
    }

    // Decomposition methods for each task type

    fn decompose_file_operation(&self, input: &str) -> Vec<TaskStep> {
        if input.contains("読") || input.contains("read") || input.contains("表示") {
            vec![
                TaskStep::new(
                    "identify_target".to_string(),
                    "Identify target file".to_string(),
                    StepType::Analysis,
                    vec!["find_file".to_string()],
                )
                .with_duration(30),
                TaskStep::new(
                    "read_file".to_string(),
                    "Read file".to_string(),
                    StepType::Implementation,
                    vec!["fs_read".to_string()],
                )
                .with_dependencies(vec!["identify_target".to_string()])
                .with_duration(30),
            ]
        } else {
            vec![
                TaskStep::new(
                    "prepare_content".to_string(),
                    "Prepare write content".to_string(),
                    StepType::Planning,
                    vec![],
                )
                .with_duration(60),
                TaskStep::new(
                    "write_file".to_string(),
                    "Write to file".to_string(),
                    StepType::Implementation,
                    vec!["fs_write".to_string()],
                )
                .with_dependencies(vec!["prepare_content".to_string()])
                .with_duration(60),
            ]
        }
    }

    fn decompose_search_task(&self, _input: &str) -> Vec<TaskStep> {
        vec![
            TaskStep::new(
                "define_search_criteria".to_string(),
                "Define search criteria".to_string(),
                StepType::Planning,
                vec![],
            )
            .with_duration(30),
            TaskStep::new(
                "execute_search".to_string(),
                "Execute search".to_string(),
                StepType::Implementation,
                vec!["search_text".to_string(), "find_file".to_string()],
            )
            .with_dependencies(vec!["define_search_criteria".to_string()])
            .with_duration(60),
            TaskStep::new(
                "analyze_results".to_string(),
                "Analyze search results".to_string(),
                StepType::Analysis,
                vec!["get_symbol_info".to_string()],
            )
            .with_dependencies(vec!["execute_search".to_string()])
            .with_duration(90),
        ]
    }

    fn decompose_code_edit(&self, _input: &str) -> Vec<TaskStep> {
        vec![
            TaskStep::new(
                "analyze_target".to_string(),
                "Analyze target code".to_string(),
                StepType::Analysis,
                vec!["fs_read".to_string(), "get_symbol_info".to_string()],
            )
            .with_duration(60)
            .with_validation(vec!["File exists".to_string()]),
            TaskStep::new(
                "plan_changes".to_string(),
                "Create change plan".to_string(),
                StepType::Planning,
                vec!["get_symbol_info".to_string()],
            )
            .with_dependencies(vec!["analyze_target".to_string()])
            .with_duration(90),
            TaskStep::new(
                "implement_changes".to_string(),
                "Implement changes".to_string(),
                StepType::Implementation,
                vec!["edit".to_string()],
            )
            .with_dependencies(vec!["plan_changes".to_string()])
            .with_duration(180)
            .with_validation(vec!["No syntax errors".to_string()]),
            TaskStep::new(
                "validate_changes".to_string(),
                "Validate changes".to_string(),
                StepType::Validation,
                vec!["execute_bash".to_string()],
            )
            .with_dependencies(vec!["implement_changes".to_string()])
            .with_duration(120)
            .with_validation(vec!["Compilation successful".to_string()]),
        ]
    }

    fn decompose_multi_file_edit(&self, _input: &str) -> Vec<TaskStep> {
        vec![
            TaskStep::new(
                "analyze_project_structure".to_string(),
                "Analyze project structure".to_string(),
                StepType::Analysis,
                vec!["search_repomap".to_string(), "get_symbol_info".to_string()],
            )
            .with_duration(120),
            TaskStep::new(
                "identify_affected_files".to_string(),
                "Identify affected files".to_string(),
                StepType::Analysis,
                vec!["search_text".to_string()],
            )
            .with_dependencies(vec!["analyze_project_structure".to_string()])
            .with_duration(90),
            TaskStep::new(
                "plan_coordinated_changes".to_string(),
                "Create coordinated change plan".to_string(),
                StepType::Planning,
                vec!["get_symbol_info".to_string()],
            )
            .with_dependencies(vec!["identify_affected_files".to_string()])
            .with_duration(180),
            TaskStep::new(
                "implement_changes_sequentially".to_string(),
                "Implement changes sequentially".to_string(),
                StepType::Implementation,
                vec!["edit".to_string(), "fs_write".to_string()],
            )
            .with_dependencies(vec!["plan_coordinated_changes".to_string()])
            .with_duration(300),
            TaskStep::new(
                "validate_integration".to_string(),
                "Execute integration tests".to_string(),
                StepType::Validation,
                vec!["execute_bash".to_string()],
            )
            .with_dependencies(vec!["implement_changes_sequentially".to_string()])
            .with_duration(180),
        ]
    }

    fn decompose_refactoring(&self, _input: &str) -> Vec<TaskStep> {
        vec![
            TaskStep::new(
                "analyze_current_structure".to_string(),
                "Analyze current code structure in detail".to_string(),
                StepType::Analysis,
                vec!["search_repomap".to_string(), "fs_read".to_string()],
            )
            .with_duration(180),
            TaskStep::new(
                "identify_refactoring_opportunities".to_string(),
                "Identify refactoring opportunities".to_string(),
                StepType::Analysis,
                vec!["get_symbol_info".to_string()],
            )
            .with_dependencies(vec!["analyze_current_structure".to_string()])
            .with_duration(120),
            TaskStep::new(
                "design_new_structure".to_string(),
                "Design new structure".to_string(),
                StepType::Planning,
                vec!["get_symbol_info".to_string()],
            )
            .with_dependencies(vec!["identify_refactoring_opportunities".to_string()])
            .with_duration(240),
            TaskStep::new(
                "implement_refactoring".to_string(),
                "Implement refactoring".to_string(),
                StepType::Implementation,
                vec!["edit".to_string(), "fs_write".to_string()],
            )
            .with_dependencies(vec!["design_new_structure".to_string()])
            .with_duration(600),
            TaskStep::new(
                "update_dependencies".to_string(),
                "Update dependencies".to_string(),
                StepType::Implementation,
                vec!["search_text".to_string(), "edit".to_string()],
            )
            .with_dependencies(vec!["implement_refactoring".to_string()])
            .with_duration(300),
            TaskStep::new(
                "run_comprehensive_tests".to_string(),
                "Execute comprehensive tests".to_string(),
                StepType::Validation,
                vec!["execute_bash".to_string()],
            )
            .with_dependencies(vec!["update_dependencies".to_string()])
            .with_duration(240),
        ]
    }

    fn decompose_feature_implementation(&self, _input: &str) -> Vec<TaskStep> {
        vec![
            TaskStep::new(
                "analyze_requirements".to_string(),
                "Analyze requirements".to_string(),
                StepType::Analysis,
                vec!["get_symbol_info".to_string()],
            )
            .with_duration(120),
            TaskStep::new(
                "design_architecture".to_string(),
                "Design architecture".to_string(),
                StepType::Planning,
                vec!["search_repomap".to_string()],
            )
            .with_dependencies(vec!["analyze_requirements".to_string()])
            .with_duration(180),
            TaskStep::new(
                "implement_core_logic".to_string(),
                "Implement core logic".to_string(),
                StepType::Implementation,
                vec!["fs_write".to_string(), "edit".to_string()],
            )
            .with_dependencies(vec!["design_architecture".to_string()])
            .with_duration(480),
            TaskStep::new(
                "implement_interfaces".to_string(),
                "Implement interfaces".to_string(),
                StepType::Implementation,
                vec!["edit".to_string()],
            )
            .with_dependencies(vec!["implement_core_logic".to_string()])
            .with_duration(240),
            TaskStep::new(
                "add_tests".to_string(),
                "Add tests".to_string(),
                StepType::Implementation,
                vec!["fs_write".to_string()],
            )
            .with_dependencies(vec!["implement_interfaces".to_string()])
            .with_duration(300),
            TaskStep::new(
                "validate_feature".to_string(),
                "Validate feature".to_string(),
                StepType::Validation,
                vec!["execute_bash".to_string()],
            )
            .with_dependencies(vec!["add_tests".to_string()])
            .with_duration(180),
            TaskStep::new(
                "integration_test".to_string(),
                "Execute integration tests".to_string(),
                StepType::Validation,
                vec!["execute_bash".to_string()],
            )
            .with_dependencies(vec!["validate_feature".to_string()])
            .with_duration(240),
        ]
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

        let keywords = analyzer.extract_keywords("ファイルを読んで編集してください");
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
