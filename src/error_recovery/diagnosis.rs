use crate::error_recovery::types::{ErrorType, PatchErrorKind};

pub struct DiagnosisEngine;

impl DiagnosisEngine {
    pub fn diagnose_apply_patch_error(
        &self,
        error_message: &str,
        file_path: &str,
        patch_content: &str,
    ) -> ErrorType {
        if error_message.contains("context lines do not match") {
            // Extract context information if possible
            ErrorType::PatchApplicationError {
                error_kind: PatchErrorKind::ContextMismatch,
                file_path: file_path.to_string(),
                expected_context: Self::extract_expected_context(patch_content),
                actual_context: Self::read_current_file_context(file_path),
            }
        } else if error_message.contains("line numbers do not match") {
            ErrorType::PatchApplicationError {
                error_kind: PatchErrorKind::LineNumberMismatch,
                file_path: file_path.to_string(),
                expected_context: None,
                actual_context: None,
            }
        } else if error_message.contains("File does not exist") {
            ErrorType::PatchApplicationError {
                error_kind: PatchErrorKind::FileNotFound,
                file_path: file_path.to_string(),
                expected_context: None,
                actual_context: None,
            }
        } else if error_message.contains("read-only")
            || error_message.contains("no write permissions")
        {
            ErrorType::PatchApplicationError {
                error_kind: PatchErrorKind::PermissionDenied,
                file_path: file_path.to_string(),
                expected_context: None,
                actual_context: None,
            }
        } else {
            ErrorType::PatchApplicationError {
                error_kind: PatchErrorKind::InvalidFormat,
                file_path: file_path.to_string(),
                expected_context: None,
                actual_context: None,
            }
        }
    }

    fn extract_expected_context(patch_content: &str) -> Option<String> {
        // Extract context lines from the patch (lines starting with ' ')
        let mut context_lines = Vec::new();
        for line in patch_content.lines() {
            if line.starts_with(' ') && !line.starts_with(" @@ ") {
                context_lines.push(line[1..].to_string()); // Remove the space prefix
            }
        }

        if context_lines.is_empty() {
            None
        } else {
            Some(context_lines.join("\n"))
        }
    }

    fn read_current_file_context(_file_path: &str) -> Option<String> {
        // This would normally read the actual file, but for now return None
        // In a real implementation, this would read the current file content
        None
    }
}
