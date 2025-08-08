use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::{fs, io::Read};

pub fn fs_read(
    root: &PathBuf,
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
