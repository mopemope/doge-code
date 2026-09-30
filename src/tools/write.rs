use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use crate::tools::mutation::{
    MutationExecution, MutationReceipt, MutationSnapshot, MutationTargetReceipt, build_receipt,
    commit_text_candidate_blocking, mutation_changed, read_text_snapshot,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "fs_write".to_string(),
            description: "Writes or overwrites a file completely. Atomic operation. Use for new files or full rewrites. For partial edits, use `edit` or `apply_patch`.".to_string(),
            strict: None,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Absolute path of the file to write (must be inside the project root or allowed paths)."},
                    "content": {"type": "string", "description": "Full text content to write. Binary content is not allowed."}
                },
                "required": ["path", "content"]
            }),
        },
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsWriteResult {
    pub success: bool,
    pub changed: bool,
    pub path: String,
    #[serde(default)]
    pub bytes_written: usize,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Pure file mutation with receipt (no session/undo/provenance side effects).
///
/// Validates scope, takes an exact before snapshot, short-circuits identical
/// content as no-op, commits via the shared mutation writer, and builds a
/// `FileWrite` receipt from the observed `before -> after`.
pub fn fs_write_with_receipt(
    path: &str,
    content: &str,
    config: &AppConfig,
) -> Result<MutationExecution<FsWriteResult>> {
    if content.as_bytes().contains(&0) {
        bail!("binary content is not allowed");
    }
    let p = Path::new(path);

    // Ensure the path is absolute
    if !p.is_absolute() {
        anyhow::bail!("Path must be absolute: {}", path);
    }

    // Check if the path is within the project root, allowed paths, or the
    // temp directory. Roots and target share one canonical-path contract;
    // the temp extra scope stays explicit to this tool and is not
    // generalized to other file tools.
    let temp_dir = std::env::temp_dir();
    crate::tools::scope::ensure_in_scope(p, config, &[temp_dir]).map_err(|e| {
        anyhow::anyhow!(
            "Access to files outside the project root is not allowed: {} ({e})",
            path
        )
    })?;

    // Exact before snapshot.
    let before: MutationSnapshot =
        read_text_snapshot(p).map_err(|e| anyhow::anyhow!("failed to snapshot {path}: {e}"))?;

    // No-op: identical content commits nothing.
    if !mutation_changed(&before, content) {
        tracing::info!(file = %path, "mutation.noop");
        return Ok(MutationExecution {
            result: FsWriteResult {
                success: true,
                changed: false,
                path: path.to_string(),
                bytes_written: content.len(),
                message: "No change needed: content is identical.".to_string(),
                warnings: vec![],
            },
            receipt: None,
        });
    }

    // Ensure parent directory exists; create if missing
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent directories for {}", p.display()))?;
    }

    let after =
        commit_text_candidate_blocking(p, &before, content).map_err(|e| anyhow::anyhow!("{e}"))?;
    let receipt: MutationReceipt = build_receipt(
        crate::provenance::ChangeKind::FileWrite,
        p.to_path_buf(),
        before,
        after,
        MutationTargetReceipt::File,
    );
    Ok(MutationExecution {
        result: FsWriteResult {
            success: true,
            changed: true,
            path: path.to_string(),
            bytes_written: content.len(),
            message: "File written successfully.".to_string(),
            warnings: vec![],
        },
        receipt: Some(receipt),
    })
}

pub fn fs_write(path: &str, content: &str, config: &AppConfig) -> Result<()> {
    let exec = fs_write_with_receipt(path, content, config)?;
    let _ = exec.receipt;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::fs;
    use std::path::PathBuf;

    fn test_config(project_root: &Path) -> AppConfig {
        AppConfig {
            project_root: project_root.to_path_buf(),
            ..AppConfig::default()
        }
    }

    #[test]
    fn test_fs_write_success() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test_file.txt");
        let file_path_str = file_path.to_str().unwrap().to_string();
        let content = "Hello, Rust!";

        fs::write(&file_path, "").unwrap();
        let config = test_config(dir.path());
        fs_write(&file_path_str, content, &config)?;

        let read_content = fs::read_to_string(&file_path)?;
        assert_eq!(read_content, content);
        Ok(())
    }

    #[test]
    fn test_fs_write_with_receipt_rewrite() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "old\n").unwrap();
        let config = test_config(dir.path());
        let exec = fs_write_with_receipt(file.to_str().unwrap(), "new\n", &config).unwrap();
        assert!(exec.result.changed);
        let receipt = exec.receipt.expect("receipt");
        assert_eq!(receipt.kind, crate::provenance::ChangeKind::FileWrite);
        assert!(receipt.before.exists);
        assert!(receipt.after.exists);
        assert_ne!(receipt.before.content_hash, receipt.after.content_hash);
        assert!(!receipt.diff.is_empty());
        Ok(())
    }

    #[test]
    fn test_fs_write_with_receipt_new_file() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("new.txt");
        let config = test_config(dir.path());
        let exec = fs_write_with_receipt(file.to_str().unwrap(), "hello\n", &config).unwrap();
        assert!(exec.result.changed);
        let receipt = exec.receipt.expect("receipt");
        assert!(!receipt.before.exists);
        assert!(receipt.after.exists);
        Ok(())
    }

    #[test]
    fn test_fs_write_identical_is_noop() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "same\n").unwrap();
        let config = test_config(dir.path());
        let exec = fs_write_with_receipt(file.to_str().unwrap(), "same\n", &config).unwrap();
        assert!(exec.result.success);
        assert!(!exec.result.changed);
        assert!(exec.receipt.is_none());
        Ok(())
    }

    #[test]
    fn test_fs_write_absolute_path_error() -> Result<()> {
        // Absolute paths are required, but scope still applies: a path
        // outside both the project root and the system temp dir must be
        // denied before any file is created.
        let project = tempfile::Builder::new()
            .prefix("test_write_project_")
            .tempdir()?;
        let config = AppConfig {
            project_root: project.path().to_path_buf(),
            ..Default::default()
        };
        // Sibling of the system temp dir: outside the project and outside
        // the `fs_write` temp extra root on every platform.
        let outside_base = std::env::temp_dir()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"));
        let outside = outside_base.join(format!("dgc_scope_denied_{}_outside", std::process::id()));
        let target = outside.join("new.txt");
        assert!(!target.exists());
        let result = fs_write(target.to_str().context("path to string")?, "test", &config);
        assert!(result.is_err());
        assert!(!target.exists(), "denied write must not create files");
        Ok(())
    }

    #[test]
    fn test_fs_write_path_escape_error() -> Result<()> {
        // `fs_write` keeps the system temp dir as an explicit extra root,
        // so a `..` spelling that resolves back inside the temp dir is
        // allowed and lands at the normalized location.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Create a subdirectory to test path escaping
        let subdir = root.join("subdir");
        fs::create_dir(&subdir).unwrap();

        // Try to write to a path that escapes the subdir
        let file_path_str = subdir.join("../escaping.txt").to_str().unwrap().to_string();

        let config = AppConfig {
            project_root: root.to_path_buf(),
            ..AppConfig::default()
        };
        let result = fs_write(&file_path_str, "test", &config);
        assert!(result.is_ok());

        // The normalized location is the parent of subdir, inside the temp dir.
        let expected_path = root.join("escaping.txt");
        assert_eq!(fs::read_to_string(&expected_path).unwrap(), "test");
        Ok(())
    }

    #[test]
    fn test_fs_write_binary_content_error() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("binary.txt");
        let file_path_str = file_path.to_str().unwrap().to_string();
        let content_with_null = "hello\0world";

        let config = test_config(dir.path());
        let result = fs_write(&file_path_str, content_with_null, &config);
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn test_fs_write_undo_new_file_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("created.txt");
        let config = test_config(dir.path());
        // Commit a new file, then simulate undo-restore semantics: before was
        // Missing, so undo must delete rather than empty.
        let exec = fs_write_with_receipt(file.to_str().unwrap(), "data\n", &config).unwrap();
        let receipt = exec.receipt.unwrap();
        assert!(!receipt.before.exists);
        // Undo path deletes; covered fully in undo tests.
        assert!(file.exists());
        let _ = PathBuf::from("x");
    }
}
