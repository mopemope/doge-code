use anyhow::{Context, Result};
use std::fs::{File, TryLockError};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

mod files;

const RUN_BYTES: u64 = 2 * 1024 * 1024;
const TOTAL_BYTES: u64 = 8 * 1024 * 1024;
const RUN_COUNT: usize = 8;
const LIMIT_NOTICE: &[u8] = b"\n[diagnostic log byte limit reached]\n";

/// Keep only identifier-safe characters and bound the length.
/// Shared by telemetry and provider diagnostics; never use for free text.
pub fn safe_identifier(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
        .take(128)
        .collect()
}

#[derive(Clone)]
pub struct RunLog(Arc<Mutex<CappedFile>>);

struct CappedFile {
    // The exclusive lease stays held until the final writer is dropped.
    file: File,
    bytes: u64,
    limit: u64,
    exhausted: bool,
}

impl Write for RunLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| std::io::Error::other("log writer poisoned"))?;
        if !state.exhausted {
            let remaining = state.limit.saturating_sub(state.bytes);
            if bytes.len() as u64 + LIMIT_NOTICE.len() as u64 <= remaining {
                // A failed write_all may already have persisted a prefix.
                // Stop permanently rather than undercounting and retrying.
                state.exhausted = true;
                state.file.write_all(bytes)?;
                state.bytes += bytes.len() as u64;
                state.exhausted = false;
            } else {
                state.exhausted = true;
                if LIMIT_NOTICE.len() as u64 <= remaining {
                    state.file.write_all(LIMIT_NOTICE)?;
                    state.bytes += LIMIT_NOTICE.len() as u64;
                }
            }
        }
        // Suppression is intentional after the bounded notice. Do not split
        // UTF-8 or buffer an unbounded tracing event to complete a line.
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0
            .lock()
            .map_err(|_| std::io::Error::other("log writer poisoned"))?
            .file
            .flush()
    }
}

pub fn open_project_log(root: &Path) -> Result<RunLog> {
    open_with_limits(root, RUN_COUNT, TOTAL_BYTES, RUN_BYTES)
}

fn run_header(name: &str) -> Option<String> {
    let id = name.strip_prefix("run-")?.strip_suffix(".log")?;
    let uuid = uuid::Uuid::parse_str(id).ok()?;
    (uuid.to_string() == id).then(|| format!("DGC_LOG_V1 {id}\n"))
}

fn open_with_limits(root: &Path, count: usize, total: u64, per_run: u64) -> Result<RunLog> {
    let directory = files::LogDirectory::open(root)?;
    let retention = directory.open_file(".retention.lock", false)?;
    files::validate(&retention).context("unsafe retention lock")?;
    retention
        .try_lock()
        .context("log retention is busy; retry startup")?;
    let mut candidates = Vec::new();
    let mut bytes = 0u64;
    let mut runs = 0usize;
    let mut active_runs = 0usize;
    let mut active_bytes = 0u64;
    for name in directory.names()? {
        let Some(header) = run_header(&name) else {
            continue;
        };
        let mut file = match directory.open_existing(&name) {
            Ok(file) => file,
            Err(error) => {
                let io = error.downcast_ref::<std::io::Error>();
                // Symlinks are unmanaged, and vanished entries consume no
                // capacity. Other errors cannot establish a safe size bound.
                let vanished = io.is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound);
                #[cfg(unix)]
                let symlink = io.is_some_and(|e| e.raw_os_error() == Some(libc::ELOOP));
                #[cfg(not(unix))]
                let symlink = false;
                if vanished || symlink {
                    continue;
                }
                return Err(error).context("inspect potential managed run log");
            }
        };
        if files::validate(&file).is_err() {
            continue;
        }
        let mut actual = vec![0; header.len()];
        match file.read_exact(&mut actual) {
            Ok(()) if actual == header.as_bytes() => {}
            Ok(()) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => continue,
            Err(error) => return Err(error).context("read managed log header"),
        }
        let inactive = match file.try_lock() {
            Ok(()) => true,
            Err(TryLockError::WouldBlock) => false,
            Err(TryLockError::Error(error)) => return Err(error).context("inspect run log lease"),
        };
        let metadata = file.metadata()?;
        let size = if inactive {
            metadata.len()
        } else {
            metadata.len().max(per_run)
        };
        bytes = bytes.checked_add(size).context("run log size overflow")?;
        runs += 1;
        if !inactive {
            active_runs += 1;
            active_bytes = active_bytes
                .checked_add(size)
                .context("active log size overflow")?;
        }
        if inactive {
            candidates.push((metadata.modified()?, name, file, size));
        }
    }
    // Do not discard useful inactive diagnostics when active reservations
    // alone already make this startup impossible.
    anyhow::ensure!(
        active_runs < count
            && active_bytes
                .checked_add(per_run)
                .is_some_and(|n| n <= total),
        "active diagnostic logs exhaust retention capacity; close another run and retry"
    );
    candidates.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    for (_, name, file, size) in candidates {
        if runs < count && bytes.checked_add(per_run).is_some_and(|n| n <= total) {
            break;
        }
        directory.remove_if_same(&name, &file)?;
        runs -= 1;
        bytes -= size;
    }
    anyhow::ensure!(
        runs < count && bytes.checked_add(per_run).is_some_and(|n| n <= total),
        "active diagnostic logs exhaust retention capacity; close another run and retry"
    );
    let name = format!("run-{}.log", uuid::Uuid::new_v4());
    let header = run_header(&name).context("invalid run log name")?;
    anyhow::ensure!(
        per_run >= (header.len() + LIMIT_NOTICE.len()) as u64,
        "log byte budget too small"
    );
    let mut file = directory.open_file(&name, true)?;
    files::validate(&file)?;
    file.try_lock().context("acquire new run log lease")?;
    file.write_all(header.as_bytes())?;
    Ok(RunLog(Arc::new(Mutex::new(CappedFile {
        file,
        bytes: header.len() as u64,
        limit: per_run,
        exhausted: false,
    }))))
}

pub fn init_logging() -> Result<()> {
    let root = std::env::current_dir().context("resolve project root for logging")?;
    let log = open_project_log(&root)?;
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .with_ansi(false)
        .with_file(true)
        .with_line_number(true)
        .with_writer(move || log.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;
    std::panic::set_hook(Box::new(|panic_info| {
        // Panic payloads can contain source text or secrets. Persist location
        // only; the local terminal retains the ordinary panic diagnostic.
        if let Some(location) = panic_info.location() {
            error!(
                file = location.file(),
                line = location.line(),
                "application panic"
            );
        } else {
            error!("application panic");
        }
        eprintln!("PANIC: {panic_info}");
    }));
    info!("logging initialized");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::fs::OpenOptions;

    fn paths(root: &Path) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(root.join(".doge/logs"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| run_header(p.file_name().unwrap().to_str().unwrap()).is_some())
            .collect()
    }

    #[test]
    fn restarting_preserves_prior_log_and_legacy_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".doge/logs")).unwrap();
        let legacy = root.path().join(".doge/logs/debug.log");
        std::fs::write(&legacy, b"legacy evidence").unwrap();
        let mut first = open_project_log(root.path()).unwrap();
        first.write_all(b"prior run evidence").unwrap();
        let previous = paths(root.path()).pop().unwrap();
        let bytes = std::fs::read(&previous).unwrap();
        let second = open_project_log(root.path()).unwrap();
        assert_eq!(std::fs::read(&previous).unwrap(), bytes);
        assert_eq!(std::fs::read(&legacy).unwrap(), b"legacy evidence");
        assert_eq!(paths(root.path()).len(), 2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.path().join(".doge/logs"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(previous).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(legacy).unwrap().permissions().mode() & 0o777,
                0o644
            );
        }
        drop((first, second));
    }

    #[test]
    fn concurrent_writers_share_one_byte_cap() {
        let root = tempfile::tempdir().unwrap();
        let log = open_with_limits(root.path(), 8, 1024, 256).unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let mut writer = log.clone();
                scope.spawn(move || {
                    for _ in 0..16 {
                        writer.write_all("検証".as_bytes()).unwrap();
                    }
                });
            }
        });
        let output = std::fs::read(paths(root.path()).pop().unwrap()).unwrap();
        assert!(output.len() <= 256);
        let text = String::from_utf8(output).unwrap();
        assert_eq!(
            text.matches("[diagnostic log byte limit reached]").count(),
            1
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn io_failure_permanently_suppresses_further_writes() {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        let mut log = RunLog(Arc::new(Mutex::new(CappedFile {
            file,
            bytes: 0,
            limit: 256,
            exhausted: false,
        })));
        assert!(log.write(b"first event").is_err());
        assert!(log.0.lock().unwrap().exhausted);
        assert_eq!(log.write(b"later event").unwrap(), b"later event".len());
    }

    #[cfg(unix)]
    #[test]
    fn retention_bounds_reserve_active_runs_and_preserve_unmanaged_files() {
        for (count, total) in [(3, 4096), (8, 768)] {
            let root = tempfile::tempdir().unwrap();
            let inactive = open_with_limits(root.path(), count, total, 256).unwrap();
            let old = paths(root.path()).pop().unwrap();
            drop(inactive);
            // A managed file modified outside dgc still counts at its actual
            // size, including a size larger than the normal per-run cap.
            let oversized = OpenOptions::new().write(true).open(&old).unwrap();
            oversized.set_len(600).unwrap();
            oversized
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
                .unwrap();
            drop(oversized);
            let unknown = root
                .path()
                .join(".doge/logs/run-00000000-0000-0000-0000-000000000000.log");
            std::fs::write(&unknown, "not a managed header").unwrap();
            let first = open_with_limits(root.path(), count, total, 256).unwrap();
            let second = open_with_limits(root.path(), count, total, 256).unwrap();
            let third = open_with_limits(root.path(), count, total, 256).unwrap();
            assert!(!old.exists());
            assert_eq!(
                std::fs::read_to_string(&unknown).unwrap(),
                "not a managed header"
            );
            // A restored inactive log must survive an impossible startup.
            let restored = run_header(old.file_name().unwrap().to_str().unwrap()).unwrap();
            std::fs::write(&old, &restored).unwrap();
            assert!(open_with_limits(root.path(), count, total, 256).is_err());
            assert_eq!(std::fs::read(&old).unwrap(), restored.as_bytes());
            drop(first);
            assert!(open_with_limits(root.path(), count, total, 256).is_ok());
            drop((second, third));
        }
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_ancestors_and_retention_locks_preserve_targets_without_blocking() {
        use std::os::unix::{
            ffi::OsStrExt,
            fs::{PermissionsExt, symlink},
        };
        for component in [".doge", ".doge/logs", ".doge/logs/.retention.lock"] {
            for attack in ["symlink", "dangling", "hardlink", "fifo"] {
                if attack == "hardlink" && component != ".doge/logs/.retention.lock" {
                    continue;
                }
                if attack == "fifo" && component != ".doge/logs/.retention.lock" {
                    continue;
                }
                let root = tempfile::tempdir().unwrap();
                let external = tempfile::tempdir().unwrap();
                let victim = external.path().join("victim");
                std::fs::write(&victim, b"preserve external").unwrap();
                let path = root.path().join(component);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                match attack {
                    "symlink" => symlink(&victim, &path).unwrap(),
                    "dangling" => symlink(external.path().join("missing"), &path).unwrap(),
                    "hardlink" => std::fs::hard_link(&victim, &path).unwrap(),
                    "fifo" => {
                        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                        // SAFETY: owned temporary fixture, valid NUL-terminated path.
                        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
                    }
                    _ => unreachable!(),
                }
                let start = std::time::Instant::now();
                assert!(
                    open_project_log(root.path()).is_err(),
                    "{component}/{attack}"
                );
                assert!(start.elapsed() < std::time::Duration::from_secs(2));
                assert_eq!(std::fs::read(&victim).unwrap(), b"preserve external");
                assert_eq!(
                    std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
                    0o644
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_run_candidates_are_never_deleted_or_modified() {
        use std::os::unix::{ffi::OsStrExt, fs::symlink};
        for attack in ["symlink", "hardlink", "fifo"] {
            let root = tempfile::tempdir().unwrap();
            let external = tempfile::tempdir().unwrap();
            let log = open_with_limits(root.path(), 1, 256, 256).unwrap();
            let path = paths(root.path()).pop().unwrap();
            drop(log);
            let victim = external.path().join("victim");
            std::fs::rename(&path, &victim).unwrap();
            let before = std::fs::read(&victim).unwrap();
            match attack {
                "symlink" => symlink(&victim, &path).unwrap(),
                "hardlink" => std::fs::hard_link(&victim, &path).unwrap(),
                "fifo" => {
                    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                    // SAFETY: valid owned fixture path.
                    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
                }
                _ => unreachable!(),
            }
            assert!(open_with_limits(root.path(), 1, 256, 256).is_ok());
            assert!(std::fs::symlink_metadata(&path).is_ok());
            assert_eq!(std::fs::read(&victim).unwrap(), before);
        }
    }

    #[cfg(unix)]
    #[test]
    fn root_alias_and_held_directories_do_not_redirect_after_swaps() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let alias_parent = tempfile::tempdir().unwrap();
        let alias = alias_parent.path().join("alias");
        symlink(root.path(), &alias).unwrap();
        let mut writer = open_project_log(&alias).unwrap();
        let prior = paths(root.path()).pop().unwrap();
        let held = root.path().join("held");
        std::fs::rename(root.path().join(".doge"), &held).unwrap();
        let external = tempfile::tempdir().unwrap();
        symlink(external.path(), root.path().join(".doge")).unwrap();
        writer.write_all(b"held writer").unwrap();
        assert!(std::fs::read_dir(external.path()).unwrap().next().is_none());
        assert!(
            std::fs::read(held.join("logs").join(prior.file_name().unwrap()))
                .unwrap()
                .ends_with(b"held writer")
        );
        let directory = files::LogDirectory::open(alias_parent.path()).unwrap();
        let moved = alias_parent.path().join("held-logs");
        std::fs::rename(alias_parent.path().join(".doge/logs"), &moved).unwrap();
        symlink(external.path(), alias_parent.path().join(".doge/logs")).unwrap();
        let file = directory.open_file("run-owned.log", true).unwrap();
        assert!(moved.join("run-owned.log").exists());
        drop(file);
        assert!(std::fs::read_dir(external.path()).unwrap().next().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn retention_lock_contention_and_final_identity_changes_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let log = open_project_log(root.path()).unwrap();
        let directory = files::LogDirectory::open(root.path()).unwrap();
        let lock = directory.open_file(".retention.lock", false).unwrap();
        lock.try_lock().unwrap();
        assert!(open_project_log(root.path()).is_err());
        drop(lock);
        let path = paths(root.path()).pop().unwrap();
        let name = path.file_name().unwrap().to_str().unwrap();
        let original = directory.open_existing(name).unwrap();
        let held = root.path().join("original");
        std::fs::rename(&path, &held).unwrap();
        std::fs::write(&path, b"replacement must survive").unwrap();
        assert!(directory.remove_if_same(name, &original).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement must survive");
        drop(log);
    }
}
