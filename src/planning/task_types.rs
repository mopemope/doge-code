use serde::{Deserialize, Serialize};

/// タスクの種類を表す列挙型
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskType {
    /// 単純なファイル操作（読み書き、検索）
    SimpleFileOperation,
    /// 単一ファイルの小さな編集
    SimpleCodeEdit,
    /// 検索・調査タスク
    SimpleSearch,
    /// 複数ファイルの変更
    MultiFileEdit,
    /// リファクタリング
    Refactoring,
    /// 新機能実装
    FeatureImplementation,
    /// アーキテクチャ変更
    ArchitecturalChange,
    /// 大規模リファクタリング
    LargeRefactoring,
    /// プロジェクト構造変更
    ProjectRestructure,
    /// 不明・分類不可
    Unknown,
}

/// リスクレベル
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

/// ステップの種類
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepType {
    /// 分析・調査
    Analysis,
    /// 計画・設計
    Planning,
    /// 実装
    Implementation,
    /// 検証・テスト
    Validation,
    /// 後処理・クリーンアップ
    Cleanup,
}

/// タスクの分類結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskClassification {
    pub task_type: TaskType,
    pub complexity_score: f32,
    pub estimated_steps: usize,
    pub risk_level: RiskLevel,
    pub required_tools: Vec<String>,
    pub confidence: f32,
}

/// 実行ステップ
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStep {
    pub id: String,
    pub description: String,
    pub step_type: StepType,
    pub dependencies: Vec<String>,
    pub estimated_duration: u64, // seconds
    pub required_tools: Vec<String>,
    pub validation_criteria: Vec<String>,
    pub prompt_template: Option<String>,
}

/// タスク実行計画
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlan {
    pub id: String,
    pub original_request: String,
    pub classification: TaskClassification,
    pub steps: Vec<TaskStep>,
    pub total_estimated_duration: u64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// ステップ実行結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub step_id: String,
    pub success: bool,
    pub output: String,
    pub artifacts: Vec<String>, // 生成されたファイルパスなど
    pub duration: u64,
    pub error_message: Option<String>,
}

/// タスク実行結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub plan_id: String,
    pub success: bool,
    pub completed_steps: Vec<StepResult>,
    pub total_duration: u64,
    pub final_message: String,
}

impl TaskType {
    /// タスクタイプの複雑度を返す（0.0-1.0）
    pub fn base_complexity(&self) -> f32 {
        match self {
            TaskType::SimpleFileOperation => 0.1,
            TaskType::SimpleCodeEdit => 0.2,
            TaskType::SimpleSearch => 0.15,
            TaskType::MultiFileEdit => 0.4,
            TaskType::Refactoring => 0.6,
            TaskType::FeatureImplementation => 0.5,
            TaskType::ArchitecturalChange => 0.8,
            TaskType::LargeRefactoring => 0.9,
            TaskType::ProjectRestructure => 1.0,
            TaskType::Unknown => 0.5,
        }
    }

    /// 推定ステップ数を返す
    pub fn estimated_steps(&self) -> usize {
        match self {
            TaskType::SimpleFileOperation => 2,
            TaskType::SimpleCodeEdit => 3,
            TaskType::SimpleSearch => 2,
            TaskType::MultiFileEdit => 5,
            TaskType::Refactoring => 6,
            TaskType::FeatureImplementation => 7,
            TaskType::ArchitecturalChange => 10,
            TaskType::LargeRefactoring => 12,
            TaskType::ProjectRestructure => 15,
            TaskType::Unknown => 4,
        }
    }

    /// リスクレベルを返す
    pub fn risk_level(&self) -> RiskLevel {
        match self {
            TaskType::SimpleFileOperation => RiskLevel::Low,
            TaskType::SimpleCodeEdit => RiskLevel::Low,
            TaskType::SimpleSearch => RiskLevel::Low,
            TaskType::MultiFileEdit => RiskLevel::Medium,
            TaskType::Refactoring => RiskLevel::Medium,
            TaskType::FeatureImplementation => RiskLevel::Medium,
            TaskType::ArchitecturalChange => RiskLevel::High,
            TaskType::LargeRefactoring => RiskLevel::High,
            TaskType::ProjectRestructure => RiskLevel::Critical,
            TaskType::Unknown => RiskLevel::Medium,
        }
    }
}

impl TaskStep {
    /// 新しいステップを作成
    pub fn new(
        id: String,
        description: String,
        step_type: StepType,
        required_tools: Vec<String>,
    ) -> Self {
        Self {
            id,
            description,
            step_type,
            dependencies: Vec::new(),
            estimated_duration: 60, // デフォルト1分
            required_tools,
            validation_criteria: Vec::new(),
            prompt_template: None,
        }
    }

    /// 依存関係を追加
    pub fn with_dependencies(mut self, dependencies: Vec<String>) -> Self {
        self.dependencies = dependencies;
        self
    }

    /// 推定時間を設定
    pub fn with_duration(mut self, duration_secs: u64) -> Self {
        self.estimated_duration = duration_secs;
        self
    }

    /// 検証条件を追加
    pub fn with_validation(mut self, criteria: Vec<String>) -> Self {
        self.validation_criteria = criteria;
        self
    }

    /// プロンプトテンプレートを設定
    pub fn with_prompt_template(mut self, template: String) -> Self {
        self.prompt_template = Some(template);
        self
    }
}
