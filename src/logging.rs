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

fn ensure_dir_permissions(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("create log directory {}", dir.display()))?;
    // Fail closed before touching permissions: never chmod through a symlink.
    if std::fs::symlink_metadata(dir)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        anyhow::bail!(
            "refusing to follow symlinked log directory {}",
            dir.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(dir)
            .with_context(|| format!("stat log directory {}", dir.display()))?
            .permissions();
        if perms.mode() & 0o777 != 0o700 {
            perms.set_mode(0o700);
            std::fs::set_permissions(dir, perms)
                .with_context(|| format!("chmod 0700 {}", dir.display()))?;
        }
    }
    Ok(())
}

fn ensure_file_permissions(path: &Path) -> Result<()> {
    // Fail closed before touching permissions: never chmod through a symlink.
    // (The file itself was opened with O_NOFOLLOW on Unix; this guards the
    // non-Unix check-then-open path and post-open swaps.)
    if std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        anyhow::bail!("refusing to follow symlinked log file {}", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::metadata(path)
            .with_context(|| format!("stat log file {}", path.display()))?
            .permissions();
        if perms.mode() & 0o777 != 0o600 {
            let mut updated = perms;
            updated.set_mode(0o600);
            std::fs::set_permissions(path, updated)
                .with_context(|| format!("chmod 0600 {}", path.display()))?;
        }
    }
    let _ = path;
    Ok(())
}

/// Open the project log file without following a final symlink.
///
/// Fails closed when `.doge/logs` or `debug.log` itself is a symlink.
/// Existing files are truncated on open (matching historical behavior)
/// and their mode is tightened to `0600` on Unix.
pub fn open_project_log(root: &Path) -> Result<File> {
    let dir = root.join(".doge").join("logs");
    if let Ok(meta) = std::fs::symlink_metadata(&dir)
        && meta.file_type().is_symlink()
    {
        anyhow::bail!(
            "refusing to follow symlinked log directory {}",
            dir.display()
        );
    }
    ensure_dir_permissions(&dir)?;
    // Re-check after creation in case the path was swapped.
    if std::fs::symlink_metadata(&dir)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        anyhow::bail!(
            "refusing to follow symlinked log directory {}",
            dir.display()
        );
    }
    let path = dir.join("debug.log");
    if std::fs::symlink_metadata(&path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        anyhow::bail!("refusing to follow symlinked log file {}", path.display());
    }
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(&path)
        .with_context(|| format!("open log file {}", path.display()))?;
    ensure_file_permissions(&path)?;
    Ok(file)
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
        let file = open_project_log(dir.path()).expect("open log");
        drop(file);
        let mode = std::fs::metadata(&log_path)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
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
