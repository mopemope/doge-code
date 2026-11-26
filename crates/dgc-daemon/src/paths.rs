//! Filesystem locations used by the daemon, resolved from the environment.

use std::ffi::OsString;
use std::fs::{self, DirBuilder, OpenOptions, Permissions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Longest socket path accepted, in bytes. `sockaddr_un.sun_path` is 104 bytes
/// on macOS and 108 on Linux; stay below both with room for the terminator.
pub const SOCKET_PATH_MAX_BYTES: usize = 100;

#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    #[error("HOME is unset or not an absolute path, and no absolute XDG directory applies")]
    HomeUnset,
    #[error(
        "socket path {} is {len} bytes, over the {SOCKET_PATH_MAX_BYTES}-byte limit; \
         set a shorter XDG_RUNTIME_DIR",
        path.display()
    )]
    SocketPathTooLong { path: PathBuf, len: usize },
}

/// Where the daemon keeps its socket, state, configuration and worktrees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonPaths {
    pub socket: PathBuf,
    pub state_dir: PathBuf,
    pub config_file: PathBuf,
    pub worktrees_dir: PathBuf,
}

impl DaemonPaths {
    /// Resolve from the process environment (XDG variables, then `$HOME`).
    pub fn from_env() -> Result<Self, PathsError> {
        Self::from_vars(|name| std::env::var_os(name))
    }

    /// Like [`Self::from_env`] with an injectable variable lookup. Empty and
    /// relative values are ignored, as the XDG specification requires.
    pub fn from_vars(get: impl Fn(&str) -> Option<OsString>) -> Result<Self, PathsError> {
        let absolute = |name: &str| get(name).map(PathBuf::from).filter(|p| p.is_absolute());
        let under_home = |relative: &str| {
            absolute("HOME")
                .map(|home| home.join(relative))
                .ok_or(PathsError::HomeUnset)
        };

        let state_dir = match absolute("XDG_STATE_HOME") {
            Some(base) => base.join("dgc"),
            None => under_home(".local/state/dgc")?,
        };
        let config_file = match absolute("XDG_CONFIG_HOME") {
            Some(base) => base.join("dgc/config.toml"),
            None => under_home(".config/dgc/config.toml")?,
        };
        let worktrees_dir = match absolute("XDG_DATA_HOME") {
            Some(base) => base.join("dgc/worktrees"),
            None => under_home(".local/share/dgc/worktrees")?,
        };
        let socket = match absolute("XDG_RUNTIME_DIR") {
            Some(base) => base.join("dgc/daemon.sock"),
            None => state_dir.join("run/daemon.sock"),
        };

        let len = socket.as_os_str().as_bytes().len();
        if len > SOCKET_PATH_MAX_BYTES {
            return Err(PathsError::SocketPathTooLong { path: socket, len });
        }
        Ok(Self {
            socket,
            state_dir,
            config_file,
            worktrees_dir,
        })
    }

    /// Everything under `root`; for tests. The socket length is not checked.
    pub fn under(root: &Path) -> Self {
        Self {
            socket: root.join("run/daemon.sock"),
            state_dir: root.join("state"),
            config_file: root.join("config/config.toml"),
            worktrees_dir: root.join("data/worktrees"),
        }
    }
}

fn refused(path: &Path, why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("refusing to use directory {}: {why}", path.display()),
    )
}

/// Create `path` (and missing parents) with mode 0700, or verify and tighten an
/// existing directory. Fails if `path` is a symlink, is not a directory, or is
/// owned by another user.
pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(refused(path, "it is a symlink"));
        }
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            DirBuilder::new().recursive(true).mode(0o700).create(path)?;
        }
        Err(err) => return Err(err),
    }

    // Open without following symlinks so the checks below apply to the very
    // directory we then chmod, even if the path is swapped in between.
    let dir = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(path)?;
    let meta = dir.metadata()?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if meta.uid() != euid {
        return Err(refused(path, "it is owned by another user"));
    }
    if meta.permissions().mode() & 0o777 != 0o700 {
        dir.set_permissions(Permissions::from_mode(0o700))?;
    }
    Ok(())
}
