use super::*;
use crate::execution::{
    ManagedProcessSpec, ManagedProcessTermination, ManagedRunOptions, run_managed_process,
};
use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Inventory {
    paths: BTreeSet<String>,
    submodules: BTreeSet<String>,
    head: Option<String>,
    relative: String,
    index: String,
}

pub(super) fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\0', '\\'])
        && !Path::new(path).is_absolute()
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}
fn excluded(path: &str) -> bool {
    path.split('/').any(|p| p == ".git" || p == ".doge")
}
async fn git(
    root: &Path,
    args: &[&str],
    deadline: Instant,
    cancel: Option<CancellationToken>,
) -> Result<String, Diagnostic> {
    let left = deadline
        .checked_duration_since(Instant::now())
        .ok_or(Diagnostic::Deadline)?;
    let mut argv = vec![
        "--no-optional-locks".to_string(),
        "--literal-pathspecs".into(),
        "-c".into(),
        "core.fsmonitor=false".into(),
        "-c".into(),
        "core.untrackedCache=false".into(),
    ];
    argv.extend(args.iter().map(|s| (*s).to_string()));
    let output = run_managed_process(
        ManagedProcessSpec::new("git", argv, root.to_path_buf()),
        ManagedRunOptions {
            timeout: Some(left),
            cancellation: cancel,
        },
    )
    .await
    .map_err(|_| Diagnostic::GitUnavailable)?;
    if output.termination == ManagedProcessTermination::TimedOut {
        return Err(Diagnostic::Deadline);
    }
    if !output.success() || output.capture_truncated || !output.warnings.is_empty() {
        return Err(Diagnostic::GitUnavailable);
    }
    if output.stdout.contains('\u{fffd}') {
        return Err(Diagnostic::UnsupportedEncoding);
    }
    Ok(output.stdout)
}
async fn inventory(
    root: &Path,
    deadline: Instant,
    cancel: Option<CancellationToken>,
) -> Result<Inventory, Diagnostic> {
    let top = git(
        root,
        &["rev-parse", "--show-toplevel"],
        deadline,
        cancel.clone(),
    )
    .await?;
    let git_root = PathBuf::from(top.trim_end_matches('\n'));
    let relative = root
        .strip_prefix(&git_root)
        .map_err(|_| Diagnostic::GitUnavailable)?
        .to_str()
        .ok_or(Diagnostic::UnsupportedEncoding)?
        .replace('\\', "/");
    let raw = git(
        root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "--deduplicate",
            "-z",
            "--",
            ".",
        ],
        deadline,
        cancel.clone(),
    )
    .await?;
    let index = git(
        root,
        &["ls-files", "--stage", "-z", "--", "."],
        deadline,
        cancel.clone(),
    )
    .await?;
    let mut paths = BTreeSet::new();
    let mut submodules = BTreeSet::new();
    for path in raw.split('\0').filter(|s| !s.is_empty()) {
        if !valid_path(path) {
            return Err(Diagnostic::InvalidPath);
        }
        if !excluded(path) {
            paths.insert(path.into());
        }
        if paths.len() > MAX_PATHS {
            return Err(Diagnostic::PathLimit);
        }
    }
    for row in index.split('\0').filter(|s| !s.is_empty()) {
        let Some((info, path)) = row.split_once('\t') else {
            return Err(Diagnostic::GitUnavailable);
        };
        if info.starts_with("160000 ") && !excluded(path) {
            submodules.insert(path.into());
        }
    }
    let head = git(
        root,
        &["rev-parse", "--verify", "HEAD^{commit}"],
        deadline,
        cancel,
    )
    .await
    .ok()
    .map(|s| s.trim().into());
    Ok(Inventory {
        paths,
        submodules,
        head,
        relative,
        index,
    })
}

pub(super) async fn capture(
    root: &Path,
    extra: BTreeSet<String>,
    cancel: Option<CancellationToken>,
) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(10);
    let started_at = chrono::Utc::now().to_rfc3339();
    let root = match root.canonicalize() {
        Ok(r) => r,
        Err(_) => return Snapshot::unavailable(Diagnostic::FileUnavailable),
    };
    if extra.len() > MAX_PATHS {
        return limited(Diagnostic::PathLimit);
    }
    let before = inventory(&root, deadline, cancel.clone()).await;
    let mut diagnostics = Vec::new();
    let mut paths = BTreeSet::new();
    for path in extra {
        if !valid_path(&path) {
            diagnostics.push(Diagnostic::InvalidPath);
        } else if !excluded(&path) {
            paths.insert(path);
        }
    }
    let mut submodules = BTreeSet::new();
    if let Ok(inv) = &before {
        paths.extend(inv.paths.clone());
        submodules = inv.submodules.clone();
    } else if let Err(reason) = &before {
        if matches!(reason, Diagnostic::PathLimit | Diagnostic::Deadline) {
            return limited(*reason);
        }
        diagnostics.push(*reason);
    }
    if paths.len() > MAX_PATHS {
        return limited(Diagnostic::PathLimit);
    }
    let worker_root = root.clone();
    let worker_cancel = cancel.clone();
    let read = tokio::task::spawn_blocking(move || {
        read_files(
            &worker_root,
            &paths,
            &submodules,
            deadline,
            worker_cancel.as_ref(),
        )
    })
    .await;
    let files = match read {
        Ok(Ok(files)) => files,
        Ok(Err(reason)) => return limited(reason),
        Err(_) => return Snapshot::unavailable(Diagnostic::FileUnavailable),
    };
    let after = inventory(&root, deadline, cancel).await;
    if before != after {
        diagnostics.push(Diagnostic::ChangedDuringCapture);
    }
    if files.iter().any(|f| !f.known()) {
        diagnostics.push(Diagnostic::FileUnavailable);
    }
    diagnostics.sort_by_key(|d| format!("{d:?}"));
    diagnostics.dedup();
    let status = if diagnostics.is_empty() {
        CaptureStatus::Complete
    } else {
        CaptureStatus::Partial
    };
    let digest = match serde_json::to_vec(&(1u32, &files)) {
        Ok(data) if data.len() <= MAX_JSON_BYTES - 2048 => {
            format!("blake3:{}", blake3::hash(&data).to_hex())
        }
        _ => return limited(Diagnostic::JsonLimit),
    };
    let (head_oid, relative) = before
        .as_ref()
        .ok()
        .map(|i| (i.head.clone(), Some(i.relative.clone())))
        .unwrap_or_default();
    let result = Snapshot {
        version: 1,
        started_at,
        finished_at: chrono::Utc::now().to_rfc3339(),
        scope: "project_git_files_and_provenance_references".into(),
        status,
        head_oid,
        project_relative_to_git_root: relative,
        files,
        manifest_digest: Some(digest),
        diagnostics,
    };
    if serde_json::to_vec(&result).map_or(true, |s| s.len() > MAX_JSON_BYTES) {
        return limited(Diagnostic::JsonLimit);
    }
    result
}
fn limited(reason: Diagnostic) -> Snapshot {
    let mut result = Snapshot::unavailable(reason);
    result.status = CaptureStatus::LimitExceeded;
    result
}
fn read_files(
    root: &Path,
    paths: &BTreeSet<String>,
    submodules: &BTreeSet<String>,
    deadline: Instant,
    cancel: Option<&CancellationToken>,
) -> Result<Vec<FileEntry>, Diagnostic> {
    let mut total = 0usize;
    let mut files = Vec::new();
    for path in paths {
        if Instant::now() >= deadline || cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err(Diagnostic::Deadline);
        }
        let mut entry = FileEntry {
            path: path.clone(),
            kind: FileKind::Unavailable,
            exists: None,
            byte_len: None,
            content_hash: None,
            executable_bits: None,
        };
        if submodules.contains(path) {
            entry.kind = FileKind::Submodule;
            entry.exists = Some(true);
        } else {
            read_one(root, path, &mut entry, &mut total, deadline, cancel)?;
        }
        files.push(entry);
    }
    Ok(files)
}
#[cfg(unix)]
pub(crate) fn open_relative(root: &Path, path: &str) -> std::io::Result<std::fs::File> {
    if !valid_path(path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unsafe relative path",
        ));
    }
    open_relative_with_hook(root, path, &mut |_| {})
}
#[cfg(not(unix))]
pub(crate) fn open_relative(_root: &Path, _path: &str) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "safe anchored reads require Unix",
    ))
}
#[cfg(unix)]
pub(super) fn open_relative_with_hook(
    root: &Path,
    path: &str,
    hook: &mut dyn FnMut(usize),
) -> std::io::Result<std::fs::File> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        },
    };
    let croot = CString::new(root.as_os_str().as_bytes())?;
    // SAFETY: CString lifetime covers open; each successful owned descriptor is closed by File.
    let rootfd = unsafe {
        libc::open(
            croot.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if rootfd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: rootfd was just opened and ownership has not been transferred.
    let mut parent = unsafe { std::fs::File::from_raw_fd(rootfd) };
    let parts: Vec<_> = path.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        let name = CString::new(*part)?;
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | libc::O_CLOEXEC
            | if i + 1 == parts.len() {
                0
            } else {
                libc::O_DIRECTORY
            };
        // SAFETY: valid directory fd and NUL-terminated component; no symlink traversal.
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: fd is a newly opened descriptor owned by this File.
        parent = unsafe { std::fs::File::from_raw_fd(fd) };
        hook(i);
    }
    Ok(parent)
}
#[cfg(unix)]
fn metadata_identity(meta: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64, u32) {
    use std::os::unix::fs::MetadataExt;
    (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
        meta.mode(),
    )
}
#[cfg(unix)]
fn read_one(
    root: &Path,
    path: &str,
    entry: &mut FileEntry,
    total: &mut usize,
    deadline: Instant,
    cancel: Option<&CancellationToken>,
) -> Result<(), Diagnostic> {
    use std::{io::Read, os::unix::fs::PermissionsExt};
    let mut file = match open_relative(root, path) {
        Ok(f) => f,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                entry.kind = FileKind::Missing;
                entry.exists = Some(false);
            } else if e.raw_os_error() == Some(libc::ELOOP) {
                entry.kind = FileKind::Symlink;
            }
            return Ok(());
        }
    };
    let before = match file.metadata() {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };
    entry.exists = Some(true);
    if before.is_dir() {
        entry.kind = FileKind::Directory;
        return Ok(());
    }
    if !before.is_file() {
        entry.kind = FileKind::Unsupported;
        return Ok(());
    }
    if before.len() > MAX_FILE_BYTES as u64 {
        return Err(Diagnostic::FileLimit);
    }
    let mut hasher = blake3::Hasher::new();
    let mut size = 0usize;
    let mut chunk = [0u8; 64 * 1024];
    loop {
        if Instant::now() >= deadline || cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err(Diagnostic::Deadline);
        }
        let n = match file.read(&mut chunk) {
            Ok(n) => n,
            Err(_) => return Ok(()),
        };
        if n == 0 {
            break;
        }
        size += n;
        if size > MAX_FILE_BYTES {
            return Err(Diagnostic::FileLimit);
        }
        *total = total.checked_add(n).ok_or(Diagnostic::InputLimit)?;
        if *total > MAX_INPUT_BYTES {
            return Err(Diagnostic::InputLimit);
        }
        hasher.update(&chunk[..n]);
    }
    let after = match file.metadata() {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };
    if metadata_identity(&before) != metadata_identity(&after) || size as u64 != after.len() {
        return Ok(());
    }
    entry.kind = FileKind::Regular;
    entry.byte_len = Some(size as u64);
    entry.content_hash = Some(format!("blake3:{}", hasher.finalize().to_hex()));
    entry.executable_bits = Some(after.permissions().mode() & 0o111);
    Ok(())
}
#[cfg(not(unix))]
fn read_one(
    _root: &Path,
    _path: &str,
    _entry: &mut FileEntry,
    _total: &mut usize,
    _deadline: Instant,
    _cancel: Option<&CancellationToken>,
) -> Result<(), Diagnostic> {
    Ok(())
}
