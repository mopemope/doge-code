use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ErrorType {
    PatchApplicationError {
        error_kind: PatchErrorKind,
        file_path: String,
        expected_context: Option<String>,
        actual_context: Option<String>,
        patch_content: String,
    },
    CompilationError {
        language: String,
        error_message: String,
        file_path: String,
        line_number: Option<u32>,
        column_number: Option<u32>,
    },
    TestFailure {
        test_framework: String,
        test_name: String,
        failure_message: String,
        file_path: String,
    },
    RuntimeError {
        error_message: String,
        stack_trace: Option<String>,
        file_path: String,
    },
    SyntaxError {
        file_path: String,
        error_message: String,
        line_number: u32,
        column_number: Option<u32>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PatchErrorKind {
    ContextMismatch,
    InvalidFormat,
    FileNotFound,
    PermissionDenied,
    LineNumberMismatch,
}

#[derive(Debug)]
pub enum FixResult {
    Success { message: String },
    PatchAdjustment { new_patch: String },
    RequiresHumanIntervention { message: String },
    Failed { reason: String },
}

pub struct ErrorContext {
    pub error_source: String, // "apply_patch", "execute_bash", etc.
    pub file_path: String,
    pub patch_content: String, // for patch errors
    pub command: String,       // for command execution errors
    pub output: String,        // command output
}
