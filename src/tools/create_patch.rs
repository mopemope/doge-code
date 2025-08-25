use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "create_patch".to_string(),
            description: "Generates a patch in the unified diff format by comparing the `original_content` of a file with its `modified_content`. This tool is crucial for preparing complex, multi-location changes that will be applied using `apply_patch`. First, use `fs_read` to get the `original_content` and its hash. Then, generate the `modified_content` (the entire desired file content after changes) in your mind or through internal reasoning. Finally, call this tool with both contents to obtain the `patch_content` string, which can then be passed to `apply_patch`.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "original_content": {"type": "string", "description": "The original content of the file."},
                    "modified_content": {"type": "string", "description": "The full desired content of the file after modification."}
                },
                "required": ["original_content", "modified_content"]
            }),
        },
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreatePatchParams {
    pub original_content: String,
    pub modified_content: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreatePatchResult {
    pub patch_content: String,
}

pub async fn create_patch(params: CreatePatchParams) -> Result<CreatePatchResult> {
    // Normalize line endings to LF for consistent diffing.
    let original_normalized = params.original_content.replace("\r\n", "\n");
    let modified_normalized = params.modified_content.replace("\r\n", "\n");

    let patch = diffy::create_patch(&original_normalized, &modified_normalized);
    Ok(CreatePatchResult {
        patch_content: patch.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_patch_simple() {
        let params = CreatePatchParams {
            original_content: "hello world\n".to_string(),
            modified_content: "hello rust\n".to_string(),
        };
        let result = create_patch(params).await.unwrap();
        assert!(result.patch_content.contains("-hello world"));
        assert!(result.patch_content.contains("+hello rust"));
    }

    #[tokio::test]
    async fn test_create_patch_multiline() {
        let params = CreatePatchParams {
            original_content: "line one\nline two\nline three\n".to_string(),
            modified_content: "line one\nline 2\nline three\n".to_string(),
        };
        let result = create_patch(params).await.unwrap();
        assert!(result.patch_content.contains("-line two"));
        assert!(result.patch_content.contains("+line 2"));
    }

    #[tokio::test]
    async fn test_create_patch_with_mixed_line_endings() {
        let params = CreatePatchParams {
            original_content: "windows\r\nline\r\n".to_string(),
            modified_content: "unix\nline\n".to_string(),
        };
        let result = create_patch(params).await.unwrap();
        assert!(result.patch_content.contains("-windows"));
        assert!(!result.patch_content.contains("-line")); // This line is unchanged, so it should not be in the patch with a '-' prefix
        assert!(result.patch_content.contains("+unix"));
        assert!(result.patch_content.contains(" line")); // This line is unchanged, so it should be in the patch with a space prefix
    }

    #[tokio::test]
    async fn test_create_patch_crlf_to_lf() {
        let params = CreatePatchParams {
            original_content: "crlf\r\nending\r\n".to_string(),
            modified_content: "crlf\nending\n".to_string(),
        };
        let result = create_patch(params).await.unwrap();
        // diffy::create_patch returns a header even if there are no changes.
        // A patch with no actual changes will have no "hunks".
        let patch = diffy::Patch::from_str(&result.patch_content).unwrap();
        assert!(
            patch.hunks().is_empty(),
            "Patch should have no hunks for content that is identical after normalization."
        );
    }

    #[tokio::test]
    async fn test_create_patch_with_mixed_endings_in_modified_content() {
        let params = CreatePatchParams {
            original_content: "line 1\nline 2\n".to_string(),
            modified_content: "line 1\r\nline two\n".to_string(),
        };
        let result = create_patch(params).await.unwrap();
        assert!(result.patch_content.contains("-line 2"));
        assert!(result.patch_content.contains("+line two"));
        assert!(!result.patch_content.contains("\r"));
    }
}
