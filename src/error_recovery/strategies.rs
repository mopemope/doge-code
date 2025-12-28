use crate::error_recovery::types::{ErrorType, FixResult, PatchErrorKind};
use anyhow::Result;
use async_trait::async_trait;
use diffy;
use std::fs;

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
                expected_context: _expected_context,
                actual_context,
                patch_content,
            } => match error_kind {
                PatchErrorKind::ContextMismatch => {
                    let current = actual_context
                        .clone()
                        .or_else(|| fs::read_to_string(file_path).ok());

                    let Some(current_content) = current else {
                        return Ok(FixResult::RequiresHumanIntervention {
                            message: format!(
                                "Context mismatch detected in '{}', and current file content could not be read.",
                                file_path
                            ),
                        });
                    };

                    let patch = match diffy::Patch::from_str(patch_content) {
                        Ok(p) => p,
                        Err(e) => {
                            return Ok(FixResult::Failed {
                                reason: format!("Patch parse failed during recovery: {}", e),
                            });
                        }
                    };

                    match crate::tools::apply_patch::apply_patch_with_aggressive_fuzz(
                        &current_content,
                        &patch,
                    ) {
                        Ok(patched) => {
                            let adjusted =
                                diffy::create_patch(&current_content, &patched).to_string();
                            Ok(FixResult::PatchAdjustment {
                                new_patch: adjusted,
                            })
                        }
                        Err(e) => Ok(FixResult::RequiresHumanIntervention {
                            message: format!(
                                "Context mismatch recovery failed for '{}': {}",
                                file_path, e
                            ),
                        }),
                    }
                }
                PatchErrorKind::LineNumberMismatch => Ok(FixResult::RequiresHumanIntervention {
                    message: format!(
                        "Line numbers in patch do not match current file '{}'. Please regenerate the patch with current file content.",
                        file_path
                    ),
                }),
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
            },
            _ => Err(anyhow::anyhow!(
                "This strategy cannot handle this error type"
            )),
        }
    }

    fn can_handle(&self, error: &ErrorType) -> bool {
        matches!(error, ErrorType::PatchApplicationError { .. })
    }
}
