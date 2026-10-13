use super::plan::{Plan, relative};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::Path};
const MAX_FILES: usize = 10_000;
const MAX_BYTES: u64 = 64 * 1024 * 1024;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub hash: String,
    pub mode: u32,
    pub link: Option<String>,
}
pub type Manifest = BTreeMap<String, Entry>;
pub fn hash(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}
#[cfg(unix)]
fn mode(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o777
}
#[cfg(not(unix))]
fn mode(_: &fs::Metadata) -> u32 {
    0
}
pub fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    Ok(crate::utils::bounded_file::read_regular_bytes(
        path,
        MAX_BYTES as usize,
        crate::utils::bounded_file::SymlinkPolicy::Reject,
    )?)
}
pub fn read_text(path: &Path) -> Result<String> {
    Ok(String::from_utf8(read_bounded(path)?)?)
}
pub fn excluded(path: &str) -> bool {
    path.split('/')
        .any(|p| matches!(p, ".git" | ".doge" | "target" | "__pycache__"))
}
pub fn manifest(root: &Path) -> Result<Manifest> {
    fn walk(root: &Path, dir: &Path, entries: &mut Manifest, bytes: &mut u64) -> Result<()> {
        for item in fs::read_dir(dir)? {
            let path = item?.path();
            let rel = path
                .strip_prefix(root)?
                .to_str()
                .context("non-UTF8 source path")?
                .to_owned();
            if excluded(&rel) {
                continue;
            }
            ensure!(relative(&rel), "unsupported source path");
            let meta = fs::symlink_metadata(&path)?;
            if meta.is_dir() {
                walk(root, &path, entries, bytes)?;
                continue;
            }
            let (data, link) = if meta.file_type().is_symlink() {
                let target = fs::read_link(&path)?;
                ensure!(
                    !target.is_absolute() && path.canonicalize()?.starts_with(root),
                    "external or absolute symlink refused"
                );
                let text = target.to_str().context("non-UTF8 symlink")?.to_owned();
                (text.as_bytes().to_vec(), Some(text))
            } else {
                ensure!(meta.is_file(), "unsupported source object");
                ensure!(meta.len() <= MAX_BYTES, "source file limit");
                (read_bounded(&path)?, None)
            };
            *bytes = bytes
                .checked_add(data.len() as u64)
                .context("source size overflow")?;
            ensure!(
                *bytes <= MAX_BYTES && entries.len() < MAX_FILES,
                "source snapshot limit"
            );
            entries.insert(
                rel,
                Entry {
                    hash: hash(&data),
                    mode: mode(&meta),
                    link,
                },
            );
        }
        Ok(())
    }
    let mut entries = Manifest::new();
    walk(root, root, &mut entries, &mut 0)?;
    Ok(entries)
}
pub fn copy(root: &Path, dest: &Path, entries: &Manifest) -> Result<()> {
    fs::create_dir(dest)?;
    for (rel, entry) in entries {
        let from = root.join(rel);
        let to = dest.join(rel);
        fs::create_dir_all(to.parent().context("snapshot parent")?)?;
        if let Some(target) = &entry.link {
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &to)?;
            #[cfg(not(unix))]
            anyhow::bail!("repair symlinks require Unix");
        } else {
            let data = read_bounded(&from)?;
            ensure!(hash(&data) == entry.hash, "source changed during copy");
            fs::write(&to, data)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&to, fs::Permissions::from_mode(entry.mode))?;
            }
        }
    }
    ensure!(
        manifest(dest)? == *entries
            && selected(root, entries.keys().map(String::as_str))? == *entries,
        "snapshot endpoints changed"
    );
    Ok(())
}
pub fn digest(entries: &Manifest) -> Result<String> {
    Ok(hash(&serde_json::to_vec(entries)?))
}
pub fn check_scope(
    root: &Path,
    baseline: &Manifest,
    plan: &Plan,
    suffixes: &BTreeMap<String, String>,
) -> Result<Manifest> {
    let now = manifest(root)?;
    for path in now.keys().chain(baseline.keys()) {
        if now.get(path) != baseline.get(path) {
            ensure!(
                plan.editable_paths.contains(path),
                "scope violation: undeclared path {path}"
            );
            ensure!(
                now.get(path).is_some_and(|e| e.link.is_none()),
                "editable source deleted/replaced by link"
            );
        }
    }
    for (path, marker) in &plan.protected_suffixes {
        let data = read_text(&root.join(path))?;
        ensure!(
            suffix(&data, marker)? == suffixes[path],
            "protected suffix changed"
        );
    }
    Ok(now)
}
pub fn suffix<'a>(data: &'a str, marker: &str) -> Result<&'a str> {
    ensure!(
        data.matches(marker).count() == 1,
        "protected marker must occur exactly once"
    );
    Ok(&data[data.find(marker).context("missing protected marker")?..])
}

pub fn selected<'a>(root: &Path, paths: impl Iterator<Item = &'a str>) -> Result<Manifest> {
    let mut out = Manifest::new();
    let mut total = 0u64;
    for rel in paths {
        ensure!(relative(rel), "unsupported inventory path");
        let path = root.join(rel);
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        let (bytes, link) = if meta.file_type().is_symlink() {
            let target = fs::read_link(&path)?;
            ensure!(
                !target.is_absolute() && path.canonicalize()?.starts_with(root),
                "external source symlink"
            );
            let text = target.to_str().context("link encoding")?.to_owned();
            (text.as_bytes().to_vec(), Some(text))
        } else {
            ensure!(
                meta.is_file() && meta.len() <= MAX_BYTES,
                "source object/size unsupported"
            );
            (read_bounded(&path)?, None)
        };
        total = total
            .checked_add(bytes.len() as u64)
            .context("size overflow")?;
        ensure!(
            total <= MAX_BYTES && out.len() < MAX_FILES,
            "source snapshot limit"
        );
        out.insert(
            rel.into(),
            Entry {
                hash: hash(&bytes),
                mode: mode(&meta),
                link,
            },
        );
    }
    Ok(out)
}
