use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;

pub fn fs_write(root: &Path, rel: &str, content: &str) -> Result<()> {
    if content.as_bytes().contains(&0) {
        bail!("binary content is not allowed");
    }
    let p = std::path::Path::new(rel);
    if p.is_absolute() {
        bail!("absolute paths are not allowed");
    }
    let path = root.join(p);

    // 親ディレクトリが存在することを確認し、存在しなければ作成する
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create parent directories for {}", path.display()))?;
    }

    // 修正箇所: 親ディレクトリを正規化し、それにファイル名を結合
    let parent = path.parent().unwrap_or(&path); // 親がない場合は自身を親とする（ルートファイルの場合）
    let file_name = path.file_name().context("path has no file name")?;
    let canon_parent = parent
        .canonicalize()
        .with_context(|| format!("canonicalize parent directory of {rel}"))?;
    let canon = canon_parent.join(file_name);

    let root_canon = root.canonicalize().context("canonicalize root")?;
    if !canon.starts_with(&root_canon) {
        bail!("path escapes project root");
    }
    fs::write(&canon, content).with_context(|| format!("write {}", canon.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs as std_fs;
    use tempfile::tempdir;

    #[test]
    fn test_fs_write_success() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let file_path = "test_file.txt";
        let content = "Hello, Rust!";

        std_fs::write(root.join(file_path), "").unwrap(); // Create the file first
        fs_write(root, file_path, content).unwrap();

        let read_content = std_fs::read_to_string(root.join(file_path)).unwrap();
        assert_eq!(read_content, content);
    }

    #[test]
    fn test_fs_write_absolute_path_error() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let absolute_path = "/tmp/abs_path.txt";

        let result = fs_write(root, absolute_path, "test");
        assert!(result.is_err());
    }

    #[test]
    fn test_fs_write_path_escape_error() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let result = fs_write(root, "../escaping.txt", "test");
        assert!(result.is_err());
    }

    #[test]
    fn test_fs_write_binary_content_error() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let content_with_null = "hello\0world";

        let result = fs_write(root, "binary.txt", content_with_null);
        assert!(result.is_err());
    }
}
