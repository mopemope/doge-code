use anyhow::{Context, Result, bail};
use std::{fs, path::Path};

use crate::tools::FsTools;

impl FsTools {
    pub fn fs_write(&self, rel: &str, content: &str) -> Result<()> {
        let path = self.normalize(rel)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }
        if content.as_bytes().contains(&0) {
            bail!("binary content not allowed");
        }
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
}
