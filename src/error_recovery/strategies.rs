use crate::error_recovery::types::{ErrorType, FixResult, PatchErrorKind};
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait FixStrategy: Send + Sync {
    async fn attempt_fix(&self, error: &ErrorType) -> Result<FixResult>;
    fn can_handle(&self, error: &ErrorType) -> bool;
}

pub struct PatchFixStrategy;

#[async_trait]
impl FixStrategy for PatchFixStrategy {
    async fn attempt_fix(&self, error: &ErrorType) -> Result<FixResult> {
        match error {
            ErrorType::PatchApplicationError {
                error_kind,
                file_path,
                expected_context,
                actual_context,
            } => {
                match error_kind {
                    PatchErrorKind::ContextMismatch => {
                        if let (Some(_expected), Some(_actual)) = (expected_context, actual_context)
                        {
                            // For now, return a message indicating context mismatch
                            // In a real implementation, we would try to find the best match
                            Ok(FixResult::RequiresHumanIntervention {
                                message: format!(
                                    "Context mismatch detected in file '{}'. Expected context differs from actual file content. Please review and adjust the patch.",
                                    file_path
                                ),
                            })
                        } else {
                            Ok(FixResult::RequiresHumanIntervention {
                                message: format!(
                                    "Context mismatch detected in file '{}', but context information is incomplete. Please review and adjust the patch.",
                                    file_path
                                ),
                            })
                        }
                    }
                    PatchErrorKind::LineNumberMismatch => {
                        Ok(FixResult::RequiresHumanIntervention {
                            message: format!(
                                "Line numbers in patch do not match current file '{}'. Please regenerate the patch with current file content.",
                                file_path
                            ),
                        })
                    }
                    PatchErrorKind::FileNotFound => Ok(FixResult::Failed {
                        reason: format!("File '{}' does not exist. Cannot apply patch.", file_path),
                    }),
                    PatchErrorKind::PermissionDenied => Ok(FixResult::Failed {
                        reason: format!(
                            "No permission to write to file '{}'. Check file permissions.",
                            file_path
                        ),
                    }),
                    PatchErrorKind::InvalidFormat => Ok(FixResult::Failed {
                        reason: format!(
                            "Patch format is invalid for file '{}'. Please check the patch format.",
                            file_path
                        ),
                    }),
                }
            }
            _ => Err(anyhow::anyhow!(
                "This strategy cannot handle this error type"
            )),
        }
    }

    fn can_handle(&self, error: &ErrorType) -> bool {
        matches!(error, ErrorType::PatchApplicationError { .. })
    }
}
