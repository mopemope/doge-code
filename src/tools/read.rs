use anyhow::{Context, Result, bail};
use std::fs;
use std::io::Read;
use std::path::Path;

pub fn fs_read(
    root: &Path,
    rel: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<String> {
    let p = std::path::Path::new(rel);
    if p.is_absolute() {
        bail!("absolute paths are not allowed");
    }
    let path = root.join(p);
    let canon = path
        .canonicalize()
        .with_context(|| format!("canonicalize {rel}"))?;
    let root_canon = root.canonicalize().context("canonicalize root")?;
    if !canon.starts_with(&root_canon) {
        bail!("path escapes project root");
    }
    let meta = fs::metadata(&canon).with_context(|| format!("metadata {}", canon.display()))?;
    if !meta.is_file() {
        bail!("not a file");
    }
    let mut f = fs::File::open(&canon).with_context(|| format!("open {}", canon.display()))?;
    let mut s = String::new();
    f.read_to_string(&mut s)
        .with_context(|| format!("read {}", canon.display()))?;
    match (offset, limit) {
        (Some(o), Some(l)) => Ok(s.lines().skip(o).take(l).collect::<Vec<_>>().join("\n")),
        _ => Ok(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::{NamedTempFile, tempdir};

    #[test]
    fn test_fs_read_full_file() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let mut file = NamedTempFile::new_in(root).unwrap();
        write!(file, "line1\nline2\nline3").unwrap();
        let file_path = file.path().strip_prefix(root).unwrap().to_str().unwrap();

        let content = fs_read(root, file_path, None, None).unwrap();
        assert_eq!(content, "line1\nline2\nline3");
    }

    #[test]
    fn test_fs_read_with_offset_limit() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let mut file = NamedTempFile::new_in(root).unwrap();
        write!(file, "line1\nline2\nline3\nline4").unwrap();
        let file_path = file.path().strip_prefix(root).unwrap().to_str().unwrap();

        let content = fs_read(root, file_path, Some(1), Some(2)).unwrap();
        assert_eq!(content, "line2\nline3");
    }

    #[test]
    fn test_fs_read_path_escape() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let result = fs_read(root, "../some_file", None, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_fs_read_not_a_file() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let result = fs_read(root, ".", None, None);
        assert!(result.is_err());
    }
}
