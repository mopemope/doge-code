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

/// タスク分析エンジン
#[derive(Clone)]
pub struct TaskAnalyzer {
    /// キーワードパターンマッピング
    keyword_patterns: HashMap<String, TaskType>,
    /// ツールマッピング
    tool_mappings: HashMap<TaskType, Vec<String>>,
    /// LLM分解器（オプション）
    llm_decomposer: Option<LlmTaskDecomposer>,
}

impl TaskAnalyzer {
    /// 新しいタスク分析エンジンを作成
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

    /// LLM分解器を設定
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

    /// キーワードパターンを初期化
    fn initialize_patterns(&mut self) {
        // ファイル操作系
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

        // 検索系
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

        // 編集系
        let edit_ops = vec![
            "編集", "edit", "修正", "fix", "変更", "change", "更新", "update", "追加", "add",
            "削除", "delete", "remove",
        ];
        for keyword in edit_ops {
            self.keyword_patterns
                .insert(keyword.to_string(), TaskType::SimpleCodeEdit);
        }

        // リファクタリング系
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

        // 実装系
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

    /// ツールマッピングを初期化
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

    /// ユーザー入力からタスクを分析
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

    /// タスクを実行可能なステップに分解
    pub async fn decompose(
        &self,
        classification: &TaskClassification,
        user_input: &str,
    ) -> Result<Vec<TaskStep>> {
        debug!("Decomposing task: {:?}", classification.task_type);

        // 複雑なタスクでLLM分解器が利用可能な場合はLLMを使用
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
                    // フォールバックとして従来の方法を使用
                }
            }
        }

        // 従来のルールベース分解
        let steps = match classification.task_type {
            TaskType::SimpleFileOperation => self.decompose_file_operation(user_input),
            TaskType::SimpleSearch => self.decompose_search_task(user_input),
            TaskType::SimpleCodeEdit => self.decompose_code_edit(user_input),
            TaskType::MultiFileEdit => self.decompose_multi_file_edit(user_input),
            TaskType::Refactoring => self.decompose_refactoring(user_input),
            TaskType::FeatureImplementation => self.decompose_feature_implementation(user_input),
            _ => {
                // 複雑なタスクでLLMが利用できない場合
                self.decompose_complex_fallback(user_input, classification)
            }
        };

        Ok(steps)
    }

    /// LLM分解を使用すべきかどうかを判定
    fn should_use_llm_decomposition(&self, classification: &TaskClassification) -> bool {
        // 以下の条件でLLM分解を使用
        match classification.task_type {
            TaskType::ArchitecturalChange
            | TaskType::LargeRefactoring
            | TaskType::ProjectRestructure => true,
            TaskType::Refactoring | TaskType::FeatureImplementation => {
                // 複雑度が高い場合
                classification.complexity_score > 0.7
            }
            TaskType::MultiFileEdit => {
                // 複雑度が高いか、推定ステップ数が多い場合
                classification.complexity_score > 0.6 || classification.estimated_steps > 8
            }
            _ => false,
        }
    }

    /// 複雑なタスクのフォールバック分解
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
                // 汎用的な複雑タスク分解
                vec![
                    TaskStep::new(
                        "analyze_requirements".to_string(),
                        "要件と現状を詳細分析".to_string(),
                        StepType::Analysis,
                        vec!["search_repomap".to_string(), "fs_read".to_string()],
                    )
                    .with_duration(300),
                    TaskStep::new(
                        "create_detailed_plan".to_string(),
                        "詳細な実行計画を作成".to_string(),
                        StepType::Planning,
                        vec!["get_symbol_info".to_string()],
                    )
                    .with_dependencies(vec!["analyze_requirements".to_string()])
                    .with_duration(600),
                    TaskStep::new(
                        "implement_incrementally".to_string(),
                        "段階的に実装".to_string(),
                        StepType::Implementation,
                        vec!["edit".to_string(), "fs_write".to_string()],
                    )
                    .with_dependencies(vec!["create_detailed_plan".to_string()])
                    .with_duration(1800),
                    TaskStep::new(
                        "comprehensive_testing".to_string(),
                        "包括的なテストと検証".to_string(),
                        StepType::Validation,
                        vec!["execute_bash".to_string()],
                    )
                    .with_dependencies(vec!["implement_incrementally".to_string()])
                    .with_duration(600)
                    .with_validation(vec![
                        "全テストが通る".to_string(),
                        "コンパイル成功".to_string(),
                    ]),
                ]
            }
        }
    }

    /// アーキテクチャ変更のフォールバック分解
    fn decompose_architectural_change_fallback(&self, _user_input: &str) -> Vec<TaskStep> {
        vec![
            TaskStep::new(
                "analyze_current_architecture".to_string(),
                "現在のアーキテクチャを詳細分析".to_string(),
                StepType::Analysis,
                vec!["search_repomap".to_string(), "get_symbol_info".to_string()],
            )
            .with_duration(600),
            TaskStep::new(
                "design_new_architecture".to_string(),
                "新しいアーキテクチャを設計".to_string(),
                StepType::Planning,
                vec!["get_symbol_info".to_string()],
            )
            .with_dependencies(vec!["analyze_current_architecture".to_string()])
            .with_duration(900),
            TaskStep::new(
                "create_migration_plan".to_string(),
                "移行計画を作成".to_string(),
                StepType::Planning,
                vec!["search_text".to_string()],
            )
            .with_dependencies(vec!["design_new_architecture".to_string()])
            .with_duration(600),
            TaskStep::new(
                "implement_core_changes".to_string(),
                "コア部分の変更を実装".to_string(),
                StepType::Implementation,
                vec!["edit".to_string(), "fs_write".to_string()],
            )
            .with_dependencies(vec!["create_migration_plan".to_string()])
            .with_duration(2400),
            TaskStep::new(
                "update_dependent_modules".to_string(),
                "依存モジュールを更新".to_string(),
                StepType::Implementation,
                vec!["search_text".to_string(), "edit".to_string()],
            )
            .with_dependencies(vec!["implement_core_changes".to_string()])
            .with_duration(1800),
            TaskStep::new(
                "comprehensive_integration_test".to_string(),
                "包括的な統合テスト".to_string(),
                StepType::Validation,
                vec!["execute_bash".to_string()],
            )
            .with_dependencies(vec!["update_dependent_modules".to_string()])
            .with_duration(900)
            .with_validation(vec![
                "全テストが通る".to_string(),
                "アーキテクチャが正しく動作".to_string(),
            ]),
        ]
    }

    /// 大規模リファクタリングのフォールバック分解
    fn decompose_large_refactoring_fallback(&self, _user_input: &str) -> Vec<TaskStep> {
        vec![
            TaskStep::new(
                "comprehensive_code_analysis".to_string(),
                "包括的なコード分析".to_string(),
                StepType::Analysis,
                vec!["search_repomap".to_string(), "search_text".to_string()],
            )
            .with_duration(600),
            TaskStep::new(
                "identify_refactoring_targets".to_string(),
                "リファクタリング対象を特定".to_string(),
                StepType::Analysis,
                vec!["get_symbol_info".to_string()],
            )
            .with_dependencies(vec!["comprehensive_code_analysis".to_string()])
            .with_duration(450),
            TaskStep::new(
                "prioritize_refactoring_tasks".to_string(),
                "リファクタリングタスクの優先順位付け".to_string(),
                StepType::Planning,
                vec!["get_symbol_info".to_string()],
            )
            .with_dependencies(vec!["identify_refactoring_targets".to_string()])
            .with_duration(300),
            TaskStep::new(
                "refactor_high_priority_modules".to_string(),
                "高優先度モジュールのリファクタリング".to_string(),
                StepType::Implementation,
                vec!["edit".to_string(), "create_patch".to_string()],
            )
            .with_dependencies(vec!["prioritize_refactoring_tasks".to_string()])
            .with_duration(2100),
            TaskStep::new(
                "update_tests_and_documentation".to_string(),
                "テストとドキュメントを更新".to_string(),
                StepType::Implementation,
                vec!["edit".to_string(), "fs_write".to_string()],
            )
            .with_dependencies(vec!["refactor_high_priority_modules".to_string()])
            .with_duration(900),
            TaskStep::new(
                "validate_refactoring_results".to_string(),
                "リファクタリング結果を検証".to_string(),
                StepType::Validation,
                vec!["execute_bash".to_string()],
            )
            .with_dependencies(vec!["update_tests_and_documentation".to_string()])
            .with_duration(600)
            .with_validation(vec![
                "全テストが通る".to_string(),
                "コード品質が向上".to_string(),
            ]),
        ]
    }

    /// プロジェクト構造変更のフォールバック分解
    fn decompose_project_restructure_fallback(&self, _user_input: &str) -> Vec<TaskStep> {
        vec![
            TaskStep::new(
                "analyze_current_structure".to_string(),
                "現在のプロジェクト構造を分析".to_string(),
                StepType::Analysis,
                vec!["fs_list".to_string(), "search_repomap".to_string()],
            )
            .with_duration(450),
            TaskStep::new(
                "design_new_structure".to_string(),
                "新しいプロジェクト構造を設計".to_string(),
                StepType::Planning,
                vec!["get_symbol_info".to_string()],
            )
            .with_dependencies(vec!["analyze_current_structure".to_string()])
            .with_duration(600),
            TaskStep::new(
                "create_backup_plan".to_string(),
                "バックアップ計画を作成".to_string(),
                StepType::Planning,
                vec!["fs_list".to_string()],
            )
            .with_dependencies(vec!["design_new_structure".to_string()])
            .with_duration(300),
            TaskStep::new(
                "create_new_directories".to_string(),
                "新しいディレクトリ構造を作成".to_string(),
                StepType::Implementation,
                vec!["execute_bash".to_string()],
            )
            .with_dependencies(vec!["create_backup_plan".to_string()])
            .with_duration(300),
            TaskStep::new(
                "move_and_reorganize_files".to_string(),
                "ファイルの移動と再編成".to_string(),
                StepType::Implementation,
                vec!["execute_bash".to_string(), "fs_write".to_string()],
            )
            .with_dependencies(vec!["create_new_directories".to_string()])
            .with_duration(1800),
            TaskStep::new(
                "update_import_paths".to_string(),
                "インポートパスを更新".to_string(),
                StepType::Implementation,
                vec!["search_text".to_string(), "edit".to_string()],
            )
            .with_dependencies(vec!["move_and_reorganize_files".to_string()])
            .with_duration(1200),
            TaskStep::new(
                "update_build_configuration".to_string(),
                "ビルド設定を更新".to_string(),
                StepType::Implementation,
                vec!["edit".to_string()],
            )
            .with_dependencies(vec!["update_import_paths".to_string()])
            .with_duration(600),
            TaskStep::new(
                "comprehensive_build_test".to_string(),
                "包括的なビルドテスト".to_string(),
                StepType::Validation,
                vec!["execute_bash".to_string()],
            )
            .with_dependencies(vec!["update_build_configuration".to_string()])
            .with_duration(900)
            .with_validation(vec![
                "プロジェクトが正常にビルド".to_string(),
                "全テストが通る".to_string(),
            ]),
        ]
    }

    /// キーワードを抽出
    fn extract_keywords(&self, input: &str) -> Vec<String> {
        let input_lower = input.to_lowercase();

        let mut keywords = Vec::new();

        // 各パターンについて、入力テキスト全体で部分マッチを確認
        for pattern in self.keyword_patterns.keys() {
            if input_lower.contains(pattern) {
                keywords.push(pattern.clone());
            }
        }

        // 日本語の活用形に対応するため、語幹マッチも追加
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

    /// キーワードによる分類
    fn classify_by_keywords(&self, keywords: &[String]) -> TaskType {
        let mut type_scores: HashMap<TaskType, usize> = HashMap::new();

        for keyword in keywords {
            if let Some(task_type) = self.keyword_patterns.get(keyword) {
                *type_scores.entry(task_type.clone()).or_insert(0) += 1;
            }
        }

        // 最もスコアの高いタスクタイプを選択
        type_scores
            .into_iter()
            .max_by_key(|(_, score)| *score)
            .map(|(task_type, _)| task_type)
            .unwrap_or(TaskType::Unknown)
    }

    /// 複雑度を推定
    fn estimate_complexity(&self, input: &str, task_type: &TaskType) -> f32 {
        let base_complexity = task_type.base_complexity();

        // 入力の長さによる調整
        let length_factor = (input.len() as f32 / 100.0).min(0.3);

        // 複数ファイルを示唆するキーワード
        let multi_file_keywords = ["複数", "全て", "すべて", "multiple", "all"];
        let multi_file_factor = if multi_file_keywords.iter().any(|k| input.contains(k)) {
            0.2
        } else {
            0.0
        };

        (base_complexity + length_factor + multi_file_factor).min(1.0)
    }

    /// 必要なツールを取得
    fn get_required_tools(&self, task_type: &TaskType) -> Vec<String> {
        self.tool_mappings
            .get(task_type)
            .cloned()
            .unwrap_or_else(|| vec!["fs_read".to_string()])
    }

    /// 分類の信頼度を計算
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

    // 各タスクタイプの分解メソッド

    fn decompose_file_operation(&self, input: &str) -> Vec<TaskStep> {
        if input.contains("読") || input.contains("read") || input.contains("表示") {
            vec![
                TaskStep::new(
                    "identify_target".to_string(),
                    "対象ファイルを特定".to_string(),
                    StepType::Analysis,
                    vec!["find_file".to_string()],
                )
                .with_duration(30),
                TaskStep::new(
                    "read_file".to_string(),
                    "ファイルを読み込み".to_string(),
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
                    "書き込み内容を準備".to_string(),
                    StepType::Planning,
                    vec![],
                )
                .with_duration(60),
                TaskStep::new(
                    "write_file".to_string(),
                    "ファイルに書き込み".to_string(),
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
                "検索条件を定義".to_string(),
                StepType::Planning,
                vec![],
            )
            .with_duration(30),
            TaskStep::new(
                "execute_search".to_string(),
                "検索を実行".to_string(),
                StepType::Implementation,
                vec!["search_text".to_string(), "find_file".to_string()],
            )
            .with_dependencies(vec!["define_search_criteria".to_string()])
            .with_duration(60),
            TaskStep::new(
                "analyze_results".to_string(),
                "検索結果を分析".to_string(),
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
                "対象コードを分析".to_string(),
                StepType::Analysis,
                vec!["fs_read".to_string(), "get_symbol_info".to_string()],
            )
            .with_duration(60)
            .with_validation(vec!["ファイルが存在する".to_string()]),
            TaskStep::new(
                "plan_changes".to_string(),
                "変更計画を作成".to_string(),
                StepType::Planning,
                vec!["get_symbol_info".to_string()],
            )
            .with_dependencies(vec!["analyze_target".to_string()])
            .with_duration(90),
            TaskStep::new(
                "implement_changes".to_string(),
                "変更を実装".to_string(),
                StepType::Implementation,
                vec!["edit".to_string()],
            )
            .with_dependencies(vec!["plan_changes".to_string()])
            .with_duration(180)
            .with_validation(vec!["構文エラーなし".to_string()]),
            TaskStep::new(
                "validate_changes".to_string(),
                "変更を検証".to_string(),
                StepType::Validation,
                vec!["execute_bash".to_string()],
            )
            .with_dependencies(vec!["implement_changes".to_string()])
            .with_duration(120)
            .with_validation(vec!["コンパイル成功".to_string()]),
        ]
    }

    fn decompose_multi_file_edit(&self, _input: &str) -> Vec<TaskStep> {
        vec![
            TaskStep::new(
                "analyze_project_structure".to_string(),
                "プロジェクト構造を分析".to_string(),
                StepType::Analysis,
                vec!["search_repomap".to_string(), "get_symbol_info".to_string()],
            )
            .with_duration(120),
            TaskStep::new(
                "identify_affected_files".to_string(),
                "影響を受けるファイルを特定".to_string(),
                StepType::Analysis,
                vec!["search_text".to_string()],
            )
            .with_dependencies(vec!["analyze_project_structure".to_string()])
            .with_duration(90),
            TaskStep::new(
                "plan_coordinated_changes".to_string(),
                "協調的な変更計画を作成".to_string(),
                StepType::Planning,
                vec!["get_symbol_info".to_string()],
            )
            .with_dependencies(vec!["identify_affected_files".to_string()])
            .with_duration(180),
            TaskStep::new(
                "implement_changes_sequentially".to_string(),
                "変更を順次実装".to_string(),
                StepType::Implementation,
                vec!["edit".to_string(), "fs_write".to_string()],
            )
            .with_dependencies(vec!["plan_coordinated_changes".to_string()])
            .with_duration(300),
            TaskStep::new(
                "validate_integration".to_string(),
                "統合テストを実行".to_string(),
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
                "現在のコード構造を詳細分析".to_string(),
                StepType::Analysis,
                vec!["search_repomap".to_string(), "fs_read".to_string()],
            )
            .with_duration(180),
            TaskStep::new(
                "identify_refactoring_opportunities".to_string(),
                "リファクタリング機会を特定".to_string(),
                StepType::Analysis,
                vec!["get_symbol_info".to_string()],
            )
            .with_dependencies(vec!["analyze_current_structure".to_string()])
            .with_duration(120),
            TaskStep::new(
                "design_new_structure".to_string(),
                "新しい構造を設計".to_string(),
                StepType::Planning,
                vec!["get_symbol_info".to_string()],
            )
            .with_dependencies(vec!["identify_refactoring_opportunities".to_string()])
            .with_duration(240),
            TaskStep::new(
                "implement_refactoring".to_string(),
                "リファクタリングを実装".to_string(),
                StepType::Implementation,
                vec!["edit".to_string(), "fs_write".to_string()],
            )
            .with_dependencies(vec!["design_new_structure".to_string()])
            .with_duration(600),
            TaskStep::new(
                "update_dependencies".to_string(),
                "依存関係を更新".to_string(),
                StepType::Implementation,
                vec!["search_text".to_string(), "edit".to_string()],
            )
            .with_dependencies(vec!["implement_refactoring".to_string()])
            .with_duration(300),
            TaskStep::new(
                "run_comprehensive_tests".to_string(),
                "包括的テストを実行".to_string(),
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
                "要件を分析".to_string(),
                StepType::Analysis,
                vec!["get_symbol_info".to_string()],
            )
            .with_duration(120),
            TaskStep::new(
                "design_architecture".to_string(),
                "アーキテクチャを設計".to_string(),
                StepType::Planning,
                vec!["search_repomap".to_string()],
            )
            .with_dependencies(vec!["analyze_requirements".to_string()])
            .with_duration(180),
            TaskStep::new(
                "implement_core_logic".to_string(),
                "コアロジックを実装".to_string(),
                StepType::Implementation,
                vec!["fs_write".to_string(), "edit".to_string()],
            )
            .with_dependencies(vec!["design_architecture".to_string()])
            .with_duration(480),
            TaskStep::new(
                "implement_interfaces".to_string(),
                "インターフェースを実装".to_string(),
                StepType::Implementation,
                vec!["edit".to_string()],
            )
            .with_dependencies(vec!["implement_core_logic".to_string()])
            .with_duration(240),
            TaskStep::new(
                "add_tests".to_string(),
                "テストを追加".to_string(),
                StepType::Implementation,
                vec!["fs_write".to_string()],
            )
            .with_dependencies(vec!["implement_interfaces".to_string()])
            .with_duration(300),
            TaskStep::new(
                "validate_feature".to_string(),
                "機能を検証".to_string(),
                StepType::Validation,
                vec!["execute_bash".to_string()],
            )
            .with_dependencies(vec!["add_tests".to_string()])
            .with_duration(180),
            TaskStep::new(
                "integration_test".to_string(),
                "統合テストを実行".to_string(),
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
        assert!(steps.len() >= 3); // 分析、計画、実装、検証

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

        // 「読む」と「編集」が抽出されることを確認
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
