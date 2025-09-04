use serde::{Deserialize, Serialize};

/// Plan execution status
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanStatus {
    /// 作成済み、実行待ち
    Created,
    /// 実行中
    Running,
    /// 一時停止中
    Paused,
    /// 正常完了
    Completed,
    /// エラーで失敗
    Failed,
    /// ユーザーによりキャンセル
    Cancelled,
}
