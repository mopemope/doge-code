use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path};
use std::time::Duration;

use crate::execution::runner::{ManagedProcessSpec, ManagedRunOptions, run_managed_process};

use super::model::*;
use super::{MAX_FILE_BYTES, MAX_INPUT_BYTES, MAX_ITEMS, ReportError, Result};

#[derive(Clone)]
pub(super) struct GitReader {
    pub program: String,
    pub timeout: Duration,
}

impl Default for GitReader {
    fn default() -> Self {
        Self {
            program: "git".into(),
            timeout: Duration::from_secs(10),
        }
    }
}

impl GitReader {
    async fn command(&self, root: &Path, args: &[String]) -> Result<String> {
        let mut argv = vec![
            "--no-optional-locks".into(),
            "--literal-pathspecs".into(),
            "-c".into(),
            "core.fsmonitor=false".into(),
            "-c".into(),
            "core.untrackedCache=false".into(),
        ];
        argv.extend_from_slice(args);
        let result = run_managed_process(
            ManagedProcessSpec::new(&self.program, argv, root.to_path_buf()),
            ManagedRunOptions::new(Some(self.timeout)),
        )
        .await
        .map_err(|_| ReportError::Git("Git process could not be run".into()))?;
        if !result.success() || result.capture_truncated || !result.warnings.is_empty() {
            return Err(ReportError::Git(
                "Git failed, timed out, or returned incomplete capture".into(),
            ));
        }
        // The shared runner returns lossy UTF-8. Do not mistake a replacement
        // character for a losslessly identified path. This also conservatively
        // rejects actual U+FFFD filenames rather than aliasing them.
        if result.stdout.contains('\u{fffd}') {
            return Err(ReportError::Git(
                "Git output contains unsupported path encoding".into(),
            ));
        }
        Ok(result.stdout)
    }

    async fn strings(&self, root: &Path, args: &[&str]) -> Result<String> {
        self.command(
            root,
            &args.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
        )
        .await
    }

    pub async fn capture(&self, root: &Path, base: Option<&str>) -> Result<GitSnapshot> {
        let collected = self.capture_inner(root, base).await;
        match collected {
            Ok(value) => Ok(value),
            Err(error) if base.is_some() => Err(error),
            Err(_) => Ok(GitSnapshot::unavailable()),
        }
    }

    async fn capture_inner(&self, root: &Path, base: Option<&str>) -> Result<GitSnapshot> {
        let top = self
            .strings(root, &["rev-parse", "--show-toplevel"])
            .await?;
        let git_root = Path::new(top.trim_end_matches('\n')).canonicalize()?;
        let relative = root
            .strip_prefix(&git_root)
            .map_err(|_| ReportError::Git("Project is outside Git root".into()))?;
        let prefix = relative
            .to_str()
            .ok_or_else(|| ReportError::Git("Unsupported project path encoding".into()))?
            .replace('\\', "/");
        let head = self
            .strings(root, &["rev-parse", "--verify", "HEAD^{commit}"])
            .await
            .ok()
            .map(|s| s.trim().to_string());
        let base_oid = if let Some(reference) = base {
            let revision = format!("{reference}^{{commit}}");
            Some(
                self.strings(
                    root,
                    &["rev-parse", "--verify", "--end-of-options", &revision],
                )
                .await?
                .trim()
                .to_string(),
            )
        } else {
            head.clone()
        };
        let status = self
            .strings(
                root,
                &[
                    "status",
                    "--porcelain=v1",
                    "-z",
                    "--untracked-files=all",
                    "--ignore-submodules=all",
                    "--no-renames",
                    "--",
                    ".",
                ],
            )
            .await?;
        let mut files = BTreeMap::new();
        for record in status.split('\0').filter(|s| !s.is_empty()) {
            let bytes = record.as_bytes();
            if bytes.len() < 4 || bytes[2] != b' ' {
                return Err(ReportError::Git("Malformed porcelain status".into()));
            }
            if let Some(path) = scoped_git_path(&record[3..], &prefix)? {
                let entry = files
                    .entry(path.clone())
                    .or_insert_with(|| workspace_entry(path));
                entry.untracked = &record[..2] == "??";
                entry.staged = !entry.untracked && bytes[0] != b' ';
                entry.unstaged = !entry.untracked && bytes[1] != b' ';
                entry.unmerged = bytes[..2].contains(&b'U') || matches!(&record[..2], "AA" | "DD");
            }
        }
        if let Some(oid) = &base_oid {
            let diff = self
                .strings(
                    root,
                    &[
                        "diff",
                        "--name-only",
                        "-z",
                        "--no-renames",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--ignore-submodules=all",
                        oid,
                        "--",
                        ".",
                    ],
                )
                .await?;
            for name in diff.split('\0').filter(|s| !s.is_empty()) {
                if let Some(path) = scoped_git_path(name, &prefix)? {
                    files
                        .entry(path.clone())
                        .or_insert_with(|| workspace_entry(path))
                        .differs_from_base = true;
                }
            }
        }
        // All submodules are explicitly outside this version's comparison.
        // Keep them in the manifest as unsupported directories even when clean.
        let index = self
            .strings(
                root,
                &["ls-files", "--stage", "-z", "--full-name", "--", "."],
            )
            .await?;
        let mut unsupported = BTreeSet::new();
        for record in index.split('\0').filter(|s| !s.is_empty()) {
            if record.starts_with("160000 ") {
                let (_, name) = record
                    .split_once('\t')
                    .ok_or_else(|| ReportError::Git("Malformed index entry".into()))?;
                if let Some(path) = scoped_git_path(name, &prefix)? {
                    unsupported.insert(path);
                }
            }
        }
        if files.len() + unsupported.len() > MAX_ITEMS {
            return Err(ReportError::Limit("workspace paths"));
        }
        let repository = Repository {
            state: if head.is_some() {
                RepositoryState::Ready
            } else {
                RepositoryState::Unborn
            },
            head_oid: head,
            requested_base: base.map(str::to_string),
            base_oid: base_oid.clone(),
            comparison_available: base_oid.is_some(),
            comparison_method: "commit_to_working_tree_no_renames".into(),
            project_relative_to_git_root: Some(if prefix.is_empty() {
                ".".into()
            } else {
                prefix
            }),
        };
        Ok(GitSnapshot {
            repository,
            files: files.into_values().collect(),
            unsupported,
        })
    }
}

fn scoped_git_path(name: &str, prefix: &str) -> Result<Option<String>> {
    let scoped = if prefix.is_empty() {
        name
    } else {
        let Some(value) = name.strip_prefix(prefix).and_then(|s| s.strip_prefix('/')) else {
            return Ok(None);
        };
        value
    };
    if !valid_relative(scoped) {
        return Err(ReportError::Git("Unsafe Git path".into()));
    }
    if scoped.split('/').any(|c| c == ".git" || c == ".doge") {
        return Ok(None);
    }
    Ok(Some(scoped.into()))
}

fn workspace_entry(path: String) -> WorkspaceChange {
    WorkspaceChange {
        path,
        differs_from_base: false,
        staged: false,
        unstaged: false,
        untracked: false,
        unmerged: false,
        attribution: Attribution::Unattributed,
        session_change_ids: vec![],
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GitSnapshot {
    pub repository: Repository,
    pub files: Vec<WorkspaceChange>,
    pub unsupported: BTreeSet<String>,
}

impl GitSnapshot {
    fn unavailable() -> Self {
        Self {
            repository: Repository {
                state: RepositoryState::Unavailable,
                head_oid: None,
                requested_base: None,
                base_oid: None,
                comparison_available: false,
                comparison_method: "unavailable".into(),
                project_relative_to_git_root: None,
            },
            files: vec![],
            unsupported: BTreeSet::new(),
        }
    }
}

pub(super) fn valid_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && !path.contains('\0')
        && !path.contains(':')
        && !Path::new(path).is_absolute()
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Reject symlinks in every component, including a missing final file.
fn blocking_kind(root: &Path, path: &str) -> Option<FileKind> {
    if !valid_relative(path) {
        return Some(FileKind::Unsupported);
    }
    let mut current = root.to_path_buf();
    for component in Path::new(path).components() {
        if !matches!(component, Component::Normal(_)) {
            return Some(FileKind::Unsupported);
        }
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => return Some(FileKind::Symlink),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Some(FileKind::Unavailable),
        }
    }
    None
}

pub(super) fn safe_path(root: &Path, path: &str) -> bool {
    blocking_kind(root, path).is_none()
}

pub(super) fn bounded_read(root: &Path, path: &str, total: &mut usize) -> Result<Vec<u8>> {
    if !valid_relative(path) {
        return Err(ReportError::UnsafeInput);
    }
    let file = crate::features::verification_snapshot::open_relative(root, path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(ReportError::UnsafeInput);
    }
    if meta.len() > MAX_FILE_BYTES as u64 {
        return Err(ReportError::Limit("file bytes"));
    }
    let mut bytes = Vec::new();
    file.take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(ReportError::Limit("file bytes"));
    }
    *total = total
        .checked_add(bytes.len())
        .ok_or(ReportError::Limit("input bytes"))?;
    if *total > MAX_INPUT_BYTES {
        return Err(ReportError::Limit("input bytes"));
    }
    Ok(bytes)
}

pub(super) fn snapshot(
    root: &Path,
    paths: &BTreeSet<String>,
    unsupported: &BTreeSet<String>,
) -> Result<Snapshot> {
    if paths.len() > MAX_ITEMS {
        return Err(ReportError::Limit("manifest paths"));
    }
    let mut total = 0;
    let mut files = Vec::new();
    for path in paths {
        let absolute = root.join(path);
        let mut entry = FileEntry {
            path: path.clone(),
            kind: FileKind::Unavailable,
            exists: None,
            byte_len: None,
            content_hash: None,
        };
        if let Some(kind) = blocking_kind(root, path) {
            entry.kind = kind;
        } else if unsupported.contains(path) {
            entry.kind = FileKind::Unsupported;
        } else {
            match fs::symlink_metadata(&absolute) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    entry.kind = FileKind::Missing;
                    entry.exists = Some(false);
                }
                Err(_) => {}
                Ok(meta) => {
                    entry.exists = Some(true);
                    entry.byte_len = Some(meta.len());
                    if meta.is_dir() {
                        entry.kind = FileKind::Directory;
                    } else if !meta.is_file() {
                        entry.kind = FileKind::Unsupported;
                    } else {
                        match bounded_read(root, path, &mut total) {
                            Ok(bytes) => {
                                entry.kind =
                                    if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
                                        FileKind::Binary
                                    } else {
                                        FileKind::Text
                                    };
                                entry.byte_len = Some(bytes.len() as u64);
                                entry.content_hash =
                                    Some(format!("blake3:{}", blake3::hash(&bytes).to_hex()));
                            }
                            Err(ReportError::Io(_)) => {}
                            Err(error) => return Err(error),
                        }
                    }
                }
            }
        }
        files.push(entry);
    }
    let complete = files
        .iter()
        .all(|f| matches!(f.kind, FileKind::Text | FileKind::Missing));
    let digest = serde_json::to_vec(&(1u32, &files))?;
    Ok(Snapshot {
        manifest_version: 1,
        scope: "session_files_and_project_git_comparison".into(),
        consistency: "optimistic_before_after_match".into(),
        complete,
        manifest_digest: format!("blake3:{}", blake3::hash(&digest).to_hex()),
        files,
    })
}

/// Materialize only the exact text bytes represented by the manifest. Existing
/// provenance queries then read this private copy without path races or growth.
pub(super) fn freeze_query_files(
    root: &Path,
    destination: &Path,
    snapshot: &Snapshot,
) -> Result<bool> {
    let mut total = 0;
    for entry in &snapshot.files {
        if entry.kind != FileKind::Text {
            continue;
        }
        let bytes = match bounded_read(root, &entry.path, &mut total) {
            Ok(bytes) => bytes,
            Err(ReportError::Io(_)) => return Ok(false),
            Err(error) => return Err(error),
        };
        let hash = format!("blake3:{}", blake3::hash(&bytes).to_hex());
        if entry.content_hash.as_deref() != Some(hash.as_str()) {
            return Ok(false);
        }
        let target = destination.join(&entry.path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(target, bytes)?;
    }
    Ok(true)
}
