use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::{fs, path::Path};

pub fn fs_write(root: &PathBuf, rel: &str, content: &str) -> Result<()> {
    let p = std::path::Path::new(rel);
    if p.is_absolute() {
        bail!("absolute paths are not allowed");
    }
    let path = root.join(p);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    if content.as_bytes().contains(&0) {
        bail!("binary content not allowed");
    }
    let root_canon = root.canonicalize().context("canonicalize root")?;
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
