use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

/// Project-local diagnostic log path: `.doge/logs/debug.log`.
pub fn project_log_path(root: &Path) -> PathBuf {
    root.join(".doge").join("logs").join("debug.log")
}

/// Keep only identifier-safe characters and bound the length.
///
/// Allowed: ASCII alphanumeric plus `_ - . :`. Control characters
/// (including CR/LF), whitespace, and all other symbols are removed.
/// Output is truncated to 128 characters.
///
/// Canonical home for the shared helper so `llm::telemetry` and
/// `features::openai_subscription` do not form a module cycle.
pub fn safe_identifier(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
        .take(128)
        .collect()
}

/// Open the log under a trusted project root (root aliases may be symlinks).
///
/// Unix follows no symlinks below that root, using held directory handles.
/// Permissions and truncation affect the opened inode, not a re-resolved path.
/// Other platforms perform preflight checks without Unix race guarantees.
/// Valid existing logs are still truncated at startup; this is not rotation.
pub fn open_project_log(root: &Path) -> Result<File> {
    #[cfg(unix)]
    {
        unix_log::open(root)
    }
    #[cfg(not(unix))]
    {
        let doge = root.join(".doge");
        let dir = doge.join("logs");
        let path = dir.join("debug.log");
        for component in [&doge, &dir, &path] {
            reject_symlink(component)?;
        }
        std::fs::create_dir_all(&dir).context("create log directory")?;
        for component in [&doge, &dir, &path] {
            reject_symlink(component)?;
        }
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .context("open project log")
    }
}

#[cfg(not(unix))]
fn reject_symlink(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => anyhow::bail!("refusing symlinked log path"),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("inspect log path"),
    }
}

#[cfg(unix)]
mod unix_log {
    use super::*;
    use std::ffi::CStr;
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    };

    pub(super) fn open(root: &Path) -> Result<File> {
        // The selected root and its ancestors are trusted. Aliases retain their
        // historical meaning; everything beneath the opened root is no-follow.
        let root = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(root)
            .context("open project root for logging")?;
        let doge = child_dir(&root, c".doge")?;
        let logs = child_dir(&doge, c"logs")?;
        open_in(&logs)
    }

    pub(super) fn child_dir(parent: &File, name: &CStr) -> Result<File> {
        // SAFETY: parent stays alive; name is a valid NUL-terminated single
        // component. mkdirat returns no owned descriptor.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o777) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error).context("create project log directory");
            }
        }
        // SAFETY: the borrowed fd and C string remain valid for this call.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("open log directory without following symlinks");
        }
        // SAFETY: successful openat gives one fresh owned fd, transferred once.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub(super) fn open_in(logs: &File) -> Result<File> {
        logs.set_permissions(std::fs::Permissions::from_mode(0o700))
            .context("chmod opened log directory 0700")?;
        // No O_TRUNC: reject symlinks, nonregular files and existing hardlinks
        // before changing an inode. NONBLOCK avoids hanging on a FIFO fixture.
        // SAFETY: logs stays alive and the constant C string is valid. The
        // creation mode is passed with C vararg integer promotion.
        let fd = unsafe {
            libc::openat(
                logs.as_raw_fd(),
                c"debug.log".as_ptr(),
                libc::O_WRONLY
                    | libc::O_CREAT
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("open log file without following symlinks");
        }
        // SAFETY: this fresh fd has one owner and is closed on every error path.
        let file = unsafe { File::from_raw_fd(fd) };
        prepare_file(file)
    }

    pub(super) fn prepare_file(file: File) -> Result<File> {
        let metadata = file.metadata().context("stat opened log file")?;
        anyhow::ensure!(
            metadata.is_file() && metadata.nlink() == 1,
            "refusing nonregular or multiply linked log file"
        );
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .context("chmod opened log file 0600")?;
        file.set_len(0).context("truncate opened project log")?;
        Ok(file)
    }
}

pub fn init_logging() -> Result<()> {
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let log_file = std::sync::Arc::new(open_project_log(&root)?);
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .with_ansi(false)
        .with_file(true)
        .with_line_number(true)
        .with_writer(log_file)
        .finish();

    tracing::subscriber::set_global_default(subscriber)?;

    // Set up panic hook to log panics to the same tracing subscriber
    std::panic::set_hook(Box::new(|panic_info| {
        let panic_msg = format!("PANIC: {}", panic_info);
        error!("{}", panic_msg);

        // Also print to stderr to ensure it's always visible
        eprintln!("{}", panic_msg);
    }));

    info!("logging initialized");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_log_path_is_project_local() {
        let root = Path::new("/tmp/example-project");
        assert_eq!(
            project_log_path(root),
            Path::new("/tmp/example-project/.doge/logs/debug.log")
        );
        assert!(
            !project_log_path(root).ends_with("debug.log")
                || project_log_path(root)
                    .to_string_lossy()
                    .contains(".doge/logs/debug.log")
        );
        // Must not be the legacy repository-root file.
        assert_ne!(
            project_log_path(root),
            Path::new("/tmp/example-project/debug.log")
        );
        assert_ne!(project_log_path(root), PathBuf::from("./debug.log"));
    }

    #[test]
    fn open_project_log_creates_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = open_project_log(dir.path()).expect("open log");
        drop(file);
        assert!(project_log_path(dir.path()).is_file());
    }

    #[cfg(unix)]
    #[test]
    fn open_project_log_enforces_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let file = open_project_log(dir.path()).expect("open log");
        drop(file);
        let log_dir = dir.path().join(".doge").join("logs");
        let dir_mode = std::fs::metadata(&log_dir)
            .expect("dir meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "log directory must be 0700");
        let log_path = project_log_path(dir.path());
        let file_mode = std::fs::metadata(&log_path)
            .expect("file meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600, "log file must be 0600");
    }

    #[cfg(unix)]
    #[test]
    fn open_project_log_repairs_existing_permissive_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let log_dir = dir.path().join(".doge").join("logs");
        std::fs::create_dir_all(&log_dir).expect("mkdir");
        let log_path = log_dir.join("debug.log");
        std::fs::write(&log_path, "old").expect("seed");
        std::fs::set_permissions(&log_path, std::fs::Permissions::from_mode(0o644))
            .expect("chmod seed");
        let doge = dir.path().join(".doge");
        std::fs::set_permissions(&doge, std::fs::Permissions::from_mode(0o750)).unwrap();
        std::fs::set_permissions(&log_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut file = open_project_log(dir.path()).expect("open log");
        assert_eq!(
            file.metadata().unwrap().len(),
            0,
            "startup truncation is preserved"
        );
        std::io::Write::write_all(&mut file, b"new log").unwrap();
        drop(file);
        assert_eq!(std::fs::read(&log_path).unwrap(), b"new log");
        assert_eq!(
            std::fs::metadata(&doge).unwrap().permissions().mode() & 0o777,
            0o750
        );
        assert_eq!(
            std::fs::metadata(&log_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let mode = std::fs::metadata(&log_path)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn open_project_log_rejects_doge_ancestor_symlink_without_touching_target() {
        use std::os::unix::{fs::PermissionsExt, fs::symlink};
        let project = tempfile::tempdir().expect("project");
        let external = tempfile::tempdir().expect("isolated external target");
        let logs = external.path().join("logs");
        std::fs::create_dir(&logs).expect("logs");
        let victim = logs.join("debug.log");
        std::fs::write(&victim, "external-fixture-content").expect("fixture");
        std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();
        symlink(external.path(), project.path().join(".doge")).expect("ancestor symlink");
        let outcome = open_project_log(project.path());
        let contents = std::fs::read_to_string(&victim).unwrap();
        let directory_mode = std::fs::metadata(&logs).unwrap().permissions().mode() & 0o777;
        let file_mode = std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777;
        assert!(
            outcome.is_err()
                && contents == "external-fixture-content"
                && directory_mode == 0o755
                && file_mode == 0o644,
            "ancestor symlink: accepted={}, preserved_bytes={}, directory_mode={directory_mode:o}, file_mode={file_mode:o}",
            outcome.is_ok(),
            contents.len()
        );
    }

    #[cfg(unix)]
    #[test]
    fn open_project_log_rejects_dangling_symlinks_at_each_component() {
        for component in [".doge", ".doge/logs", ".doge/logs/debug.log"] {
            let project = tempfile::tempdir().unwrap();
            let external = tempfile::tempdir().unwrap();
            let missing = external.path().join("must-not-be-created");
            let link = project.path().join(component);
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&missing, &link).unwrap();
            assert!(
                open_project_log(project.path()).is_err(),
                "component={component}"
            );
            assert!(!missing.exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn open_project_log_rejects_hardlinked_file_without_chmod_or_truncate() {
        use std::os::unix::fs::PermissionsExt;
        let project = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let logs = project.path().join(".doge/logs");
        std::fs::create_dir_all(&logs).unwrap();
        let victim = external.path().join("victim");
        std::fs::write(&victim, "keep hardlink content").unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::hard_link(&victim, logs.join("debug.log")).unwrap();
        assert!(open_project_log(project.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "keep hardlink content"
        );
        assert_eq!(
            std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[cfg(unix)]
    #[test]
    fn open_project_log_rejects_fifo_without_waiting_for_a_reader() {
        use std::os::unix::{ffi::OsStrExt, fs::FileTypeExt};
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".doge/logs")).unwrap();
        let fifo = project_log_path(project.path());
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: temporary fixture path is a valid NUL-terminated string.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let path = project.path().to_owned();
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker =
            std::thread::spawn(move || sender.send(open_project_log(&path).is_err()).unwrap());
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("FIFO must fail without waiting for a reader")
        );
        worker.join().unwrap();
        assert!(
            std::fs::symlink_metadata(&fifo)
                .unwrap()
                .file_type()
                .is_fifo()
        );
    }

    #[cfg(unix)]
    #[test]
    fn open_project_log_preserves_trusted_root_aliases() {
        let parent = tempfile::tempdir().unwrap();
        let project = parent.path().join("project");
        let alias = parent.path().join("alias");
        std::fs::create_dir(&project).unwrap();
        std::os::unix::fs::symlink(&project, &alias).unwrap();
        drop(open_project_log(&alias).unwrap());
        assert!(project_log_path(&project).is_file());
    }

    #[cfg(unix)]
    #[test]
    fn opened_log_handles_do_not_redirect_after_ancestor_or_final_swaps() {
        use std::os::unix::{fs::PermissionsExt, fs::symlink};
        for component in ["doge", "logs", "file"] {
            let project = tempfile::tempdir().unwrap();
            let external = tempfile::tempdir().unwrap();
            let target_logs = external.path().join("logs");
            std::fs::create_dir(&target_logs).unwrap();
            let victim = target_logs.join("debug.log");
            std::fs::write(&victim, "external unchanged").unwrap();
            std::fs::set_permissions(&target_logs, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();
            let root = File::open(project.path()).unwrap();
            let doge = unix_log::child_dir(&root, c".doge").unwrap();
            if component == "doge" {
                std::fs::rename(project.path().join(".doge"), project.path().join("held")).unwrap();
                symlink(external.path(), project.path().join(".doge")).unwrap();
                let logs = unix_log::child_dir(&doge, c"logs").unwrap();
                drop(unix_log::open_in(&logs).unwrap());
                assert!(project.path().join("held/logs/debug.log").is_file());
            } else {
                let logs = unix_log::child_dir(&doge, c"logs").unwrap();
                let local = project_log_path(project.path());
                std::fs::write(&local, "old log").unwrap();
                if component == "logs" {
                    std::fs::rename(local.parent().unwrap(), project.path().join("held")).unwrap();
                    symlink(&target_logs, local.parent().unwrap()).unwrap();
                    drop(unix_log::open_in(&logs).unwrap());
                    assert!(
                        std::fs::read(project.path().join("held/debug.log"))
                            .unwrap()
                            .is_empty()
                    );
                } else {
                    let file = OpenOptions::new().write(true).open(&local).unwrap();
                    let held = project.path().join("held-log");
                    std::fs::rename(&local, &held).unwrap();
                    symlink(&victim, &local).unwrap();
                    drop(unix_log::prepare_file(file).unwrap());
                    assert!(std::fs::read(&held).unwrap().is_empty());
                }
            }
            assert_eq!(
                std::fs::read_to_string(&victim).unwrap(),
                "external unchanged"
            );
            assert_eq!(
                std::fs::metadata(&target_logs)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
            assert_eq!(
                std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
                0o644
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn open_project_log_rejects_final_symlink() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log_dir = dir.path().join(".doge").join("logs");
        std::fs::create_dir_all(&log_dir).expect("mkdir");
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, "victim-content").expect("victim");
        let link = log_dir.join("debug.log");
        std::os::unix::fs::symlink(&victim, &link).expect("symlink");
        let err = open_project_log(dir.path()).unwrap_err();
        assert!(
            format!("{err:?}").contains("symlink"),
            "must refuse symlinked log file, got {err:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&victim).expect("read victim"),
            "victim-content",
            "victim must not be truncated"
        );
    }

    #[cfg(unix)]
    #[test]
    fn open_project_log_rejects_symlinked_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let doge = dir.path().join(".doge");
        std::fs::create_dir_all(&doge).expect("mkdir");
        let target = dir.path().join("elsewhere");
        std::fs::create_dir_all(&target).expect("target");
        let link = doge.join("logs");
        std::os::unix::fs::symlink(&target, &link).expect("symlink dir");
        let err = open_project_log(dir.path()).unwrap_err();
        assert!(
            format!("{err:?}").contains("symlink"),
            "must refuse symlinked log directory, got {err:?}"
        );
    }
}
