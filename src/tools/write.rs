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
    let canon = path
        .canonicalize()
        .with_context(|| format!("canonicalize {rel}"))?;
    let root_canon = root.canonicalize().context("canonicalize root")?;
    if !canon.starts_with(&root_canon) {
        bail!("path escapes project root");
    }
    fs::write(&canon, content).with_context(|| format!("write {}", canon.display()))
}
