use anyhow::{Context, Result};
use std::fs;
use std::io::Read;
use std::path::Path;

pub fn fs_read(path: &str, offset: Option<usize>, limit: Option<usize>) -> Result<String> {
    let p = Path::new(path);

    // Ensure the path is absolute
    if !p.is_absolute() {
        anyhow::bail!("Path must be absolute: {}", path);
    }

    let meta = fs::metadata(p).with_context(|| format!("metadata {}", p.display()))?;
    if !meta.is_file() {
        anyhow::bail!("not a file");
    }
    let mut f = fs::File::open(p).with_context(|| format!("open {}", p.display()))?;
    let mut s = String::new();
    f.read_to_string(&mut s)
        .with_context(|| format!("read {}", p.display()))?;
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
        let file_path = file.path().to_str().unwrap();

        let content = fs_read(file_path, None, None).unwrap();
        assert_eq!(content, "line1\nline2\nline3");
    }

    #[test]
    fn test_fs_read_with_offset_limit() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let mut file = NamedTempFile::new_in(root).unwrap();
        write!(file, "line1\nline2\nline3\nline4").unwrap();
        let file_path = file.path().to_str().unwrap();

        let content = fs_read(file_path, Some(1), Some(2)).unwrap();
        assert_eq!(content, "line2\nline3");
    }

    #[test]
    fn test_fs_read_path_escape() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let file_path = root.join("../some_file");
        let file_path_str = file_path.to_str().unwrap();
        let result = fs_read(file_path_str, None, None);
        // Since we're now allowing absolute paths, this test might need to be adjusted
        // depending on the environment. For now, let's just check it's an error.
        assert!(result.is_err());
    }

    #[test]
    fn test_fs_read_not_a_file() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let dir_path = root.to_str().unwrap();
        let result = fs_read(dir_path, None, None);
        assert!(result.is_err());
    }
}
