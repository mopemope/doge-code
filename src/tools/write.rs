use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::{Context, Result, bail};
use diffy::create_patch;
use serde_json::json;
use std::fs;
use std::path::Path;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "fs_write".to_string(),
            description: "Writes or overwrites a file completely. Atomic operation. Use for new files or full rewrites. For partial edits, use `edit` or `apply_patch`.".to_string(),
            strict: None,
            parameters: json!({
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

pub fn fs_write(path: &str, content: &str, config: &AppConfig) -> Result<()> {
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

    // Ensure parent directory exists; create if missing
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create parent directories for {}", p.display()))?;
    }

    // Read current file content
    let old_content = if p.exists() {
        fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?
    } else {
        String::new()
    };

    // Compute and print diff
    let patch = create_patch(&old_content, content);
    if !patch.hunks().is_empty() {
        // println!("Diff for {path}:\n{patch}");
    }

    // Atomic write: write to temp file, then rename
    // usage of tempfile crate ensures the file is created in the same filesystem if possible,
    // or we can explicitly use `tempfile::Builder::new().tempfile_in(parent)`
    if let Some(parent) = p.parent() {
        let mut temp_file = tempfile::Builder::new()
            .prefix(".tmp_atomic_write_")
            .tempfile_in(parent)
            .with_context(|| format!("failed to create temp file in {}", parent.display()))?;

        use std::io::Write;
        temp_file
            .write_all(content.as_bytes())
            .with_context(|| "failed to write to temp file")?;

        // persist (rename)
        temp_file
            .persist(p)
            .with_context(|| format!("failed to persist (rename) file to {}", p.display()))?;
    } else {
        // Fallback for root path (unlikely in this context but good for safety)
        fs::write(p, content).with_context(|| format!("write {}", p.display()))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn create_temp_dir() -> PathBuf {
        // Use the system temp directory directly
        let temp_dir = std::env::temp_dir();
        let dir = tempfile::Builder::new()
            .prefix("test_")
            .tempdir_in(&temp_dir)
            .unwrap();
        #[allow(deprecated)]
        let path = dir.into_path();
        path
    }

    #[test]
    fn test_fs_write_success() -> Result<()> {
        let root = create_temp_dir();
        let file_path = root.join("test_file.txt");
        let file_path_str = file_path
            .to_str()
            .context("Failed to convert path to string")?
            .to_string();
        let content = "Hello, Rust!";

        fs::write(&file_path, "").context("Failed to create file")?;
        fs_write(&file_path_str, content, &AppConfig::default())?;

        let read_content = fs::read_to_string(&file_path).context("Failed to read file")?;
        assert_eq!(read_content, content);
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
        let root = create_temp_dir();
        // Create a subdirectory to test path escaping
        let subdir = root.join("subdir");
        fs::create_dir(&subdir).unwrap();

        // Try to write to a path that escapes the subdir
        let file_path_str = subdir.join("../escaping.txt").to_str().unwrap().to_string();

        let result = fs_write(&file_path_str, "test", &AppConfig::default());
        assert!(result.is_ok());

        // Check the file was written to the normalized location after
        // canonicalization (parent of subdir, inside the temp dir).
        let expected_path = root.join("escaping.txt");
        assert_eq!(fs::read_to_string(&expected_path).unwrap(), "test");
        Ok(())
    }

    #[test]
    fn test_fs_write_binary_content_error() -> Result<()> {
        let root = create_temp_dir();
        let file_path = root.join("binary.txt");
        let file_path_str = file_path
            .to_str()
            .context("Failed to convert path to string")?
            .to_string();
        let content_with_null = "hello\0world";

        let result = fs_write(&file_path_str, content_with_null, &AppConfig::default());
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn test_fs_write_diff_display() -> Result<()> {
        let root = create_temp_dir();
        let file_path = root.join("diff_test.txt");
        let file_path_str = file_path
            .to_str()
            .context("Failed to convert path to string")?
            .to_string();
        let old_content = "Old content\n";
        let new_content = "New content\n";

        fs::write(&file_path, old_content).unwrap();
        // Using println! in tests shows the diff in test output.
        // Here we observe that diff is printed during test execution.
        // In real tests, verifying diff content is difficult, so this test
        // primarily ensures there are no compilation errors.
        fs_write(&file_path_str, new_content, &AppConfig::default()).unwrap();
        Ok(())
    }
}
