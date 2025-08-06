use anyhow::{Context, Result, bail};
use regex::Regex;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct FsTools {
    root: PathBuf,
}

impl FsTools {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn normalize(&self, rel: &str) -> Result<PathBuf> {
        let p = Path::new(rel);
        if p.is_absolute() {
            bail!("absolute paths are not allowed");
        }
        let candidate = self.root.join(p);
        // ensure parent exists for writes; for reads canonicalize normally
        Ok(candidate)
    }

    pub fn fs_read(
        &self,
        rel: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String> {
        let path = self.normalize(rel)?;
        let canon = path
            .canonicalize()
            .with_context(|| format!("canonicalize {rel}"))?;
        let root_canon = self.root.canonicalize().context("canonicalize root")?;
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

    pub fn fs_write(&self, rel: &str, content: &str) -> Result<()> {
        let path = self.normalize(rel)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }
        if content.as_bytes().contains(&0) {
            bail!("binary content not allowed");
        }
        // After create, verify it doesn't escape root by canonicalizing parent
        let root_canon = self.root.canonicalize().context("canonicalize root")?;
        let canon_parent = path
            .parent()
            .unwrap_or(Path::new("."))
            .canonicalize()
            .context("canonicalize parent")?;
        if !canon_parent.starts_with(&root_canon) {
            bail!("path escapes project root");
        }
        fs::write(&path, content).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    pub fn fs_search(
        &self,
        pattern: &str,
        include: Option<&str>,
    ) -> Result<Vec<(PathBuf, usize, String)>> {
        let re = Regex::new(pattern).context("invalid regex")?;
        let mut results = Vec::new();
        let walker =
            globwalk::GlobWalkerBuilder::from_patterns(&self.root, &[include.unwrap_or("**/*")])
                .follow_links(false)
                .case_insensitive(true)
                .build()
                .context("build glob walker")?;
        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let p = entry.path().to_path_buf();
            if entry.file_type().is_dir() {
                continue;
            }
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                let bin_exts = [
                    "png", "jpg", "jpeg", "gif", "webp", "bmp", "pdf", "zip", "gz", "tar", "xz",
                    "zst",
                ];
                if bin_exts.contains(&ext) {
                    continue;
                }
            }
            let content = match fs::read_to_string(&p) {
                Ok(s) => s,
                Err(_) => continue,
            };
            for (i, line) in content.lines().enumerate() {
                if re.is_match(line) {
                    results.push((p.clone(), i + 1, line.to_string()));
                }
            }
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn read_write_and_search() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("a.txt"), "hello\nworld\nhello").unwrap();

        let tools = FsTools::new(root);
        // read
        let all = tools.fs_read("a.txt", None, None).unwrap();
        assert!(all.contains("world"));
        let part = tools.fs_read("a.txt", Some(1), Some(1)).unwrap();
        assert_eq!(part, "world");
        // write
        tools.fs_write("b/c.txt", "x").unwrap();
        assert_eq!(fs::read_to_string(root.join("b/c.txt")).unwrap(), "x");
        // search
        let hits = tools.fs_search("^hello$", Some("**/*.txt")).unwrap();
        assert!(hits.iter().any(|(p, _, _)| p.ends_with("a.txt")));
    }
}
