use anyhow::Result;
use globwalk::GlobWalkerBuilder;
use std::path::Path;

/// Lists files and directories within a specified path.
///
/// # Arguments
///
/// * `path` - The absolute path to list files and directories.
/// * `max_depth` - The maximum depth to traverse. Defaults to 1.
/// * `pattern` - An optional glob pattern to filter files.
///
/// # Returns
///
/// A vector of strings representing the file and directory paths.
pub fn fs_list(path: &str, max_depth: Option<usize>, pattern: Option<&str>) -> Result<Vec<String>> {
    let full_path = Path::new(path);

    // Ensure the path is absolute
    if !full_path.is_absolute() {
        anyhow::bail!("Path must be absolute: {}", path);
    }

    let walker = GlobWalkerBuilder::new(full_path, pattern.unwrap_or("**/*"))
        .max_depth(max_depth.unwrap_or(1))
        .build()?;

    let files = walker
        .filter_map(Result::ok)
        .map(|entry| entry.path().to_string_lossy().to_string())
        .collect();

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_fs_list_simple() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("a")).unwrap();
        fs::write(root.join("a/b.txt"), "").unwrap();
        fs::write(root.join("c.txt"), "").unwrap();

        let root_str = root.to_str().unwrap();
        // With max_depth=1, we should only see direct children of the root.
        let result = fs_list(root_str, Some(1), None).unwrap();
        let mut expected = vec![format!("{}/a", root_str), format!("{}/c.txt", root_str)];
        expected.sort();
        let mut sorted_result = result;
        sorted_result.sort();

        assert_eq!(sorted_result, expected);
    }

    #[test]
    fn test_fs_list_with_depth() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/b/c.txt"), "").unwrap();

        let root_str = root.to_str().unwrap();
        let result = fs_list(&format!("{root_str}/a"), Some(1), None).unwrap();
        assert_eq!(result, vec![format!("{root_str}/a/b")]);
    }

    #[test]
    fn test_fs_list_with_pattern() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.txt"), "").unwrap();
        fs::write(root.join("b.log"), "").unwrap();

        let root_str = root.to_str().unwrap();
        let result = fs_list(root_str, None, Some("*.txt")).unwrap();
        assert_eq!(result, vec![format!("{root_str}/a.txt")]);
    }
}
