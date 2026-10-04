use super::error::SessionError;
use std::{
    fs::File,
    path::{Path, PathBuf},
};

/// Exclusive process ownership. Never cloned, and never unlink the lock file:
/// its stable inode is shared by all writers even when session.json is replaced.
#[derive(Debug)]
pub(crate) struct SessionLease {
    _file: File,
    anchor: File,
    root: PathBuf,
    id: String,
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        // Release ownership when the guard ends, even if a fork/exec child
        // temporarily retains a duplicate of this open-file description.
        if let Err(error) = self._file.unlock() {
            tracing::warn!(%error, session_id = %self.id, "session lease unlock failed; closing owner handle");
        }
    }
}

impl SessionLease {
    pub(crate) fn acquire(root: &Path, id: &str) -> Result<Self, SessionError> {
        super::store::validate_id(id)?;
        let root = root.canonicalize().map_err(SessionError::LockError)?;
        let anchor = File::open(&root).map_err(SessionError::LockError)?;
        let file = open_lock(&root, id).map_err(SessionError::LockError)?;
        match file.try_lock() {
            Ok(()) => Ok(Self {
                _file: file,
                anchor,
                root,
                id: id.into(),
            }),
            Err(std::fs::TryLockError::WouldBlock) => Err(SessionError::Busy(id.into())),
            Err(std::fs::TryLockError::Error(error)) => Err(SessionError::LockError(error)),
        }
    }

    pub(crate) fn validate(&self, root: &Path, id: &str) -> Result<(), SessionError> {
        if self.id != id || self.root != root.canonicalize().map_err(SessionError::LockError)? {
            return Err(SessionError::InvalidId(
                "session lease does not match store and ID".into(),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let held = self.anchor.metadata().map_err(SessionError::LockError)?;
            let current = root.metadata().map_err(SessionError::LockError)?;
            if (held.dev(), held.ino()) != (current.dev(), current.ino()) {
                return Err(SessionError::InvalidId(
                    "session store root changed while owned".into(),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn open_lock(root: &Path, id: &str) -> std::io::Result<File> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::MetadataExt},
        },
    };
    let root_name = CString::new(root.as_os_str().as_bytes())?;
    // SAFETY: all strings are NUL terminated; successful fds transfer once to File.
    let fd = unsafe {
        libc::open(
            root_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let root_file = unsafe { File::from_raw_fd(fd) };
    let locks = c".locks";
    // Anchor creation and lookup to the open root, never following a lock-dir alias.
    let result = unsafe { libc::mkdirat(root_file.as_raw_fd(), locks.as_ptr(), 0o700) };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error);
        }
    }
    let fd = unsafe {
        libc::openat(
            root_file.as_raw_fd(),
            locks.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let locks_file = unsafe { File::from_raw_fd(fd) };
    let name = CString::new(format!("{id}.lock"))?;
    let fd = unsafe {
        libc::openat(
            locks_file.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let meta = file.metadata()?;
    if !meta.is_file() || meta.nlink() != 1 {
        return Err(std::io::Error::other("unsafe session lock file"));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_lock(_root: &Path, _id: &str) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "safe session lock paths are unsupported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_guard_release_does_not_wait_for_inherited_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let lease = SessionLease::acquire(dir.path(), "owned").unwrap();
        // fork/exec may briefly inherit the same open-file description even
        // with CLOEXEC. Its duplicate is not a separate session owner.
        let inherited = lease._file.try_clone().unwrap();
        assert!(matches!(
            SessionLease::acquire(dir.path(), "owned"),
            Err(SessionError::Busy(_))
        ));
        drop(lease);
        let next = SessionLease::acquire(dir.path(), "owned").expect("guard ended ownership");
        drop(inherited);
        next.validate(dir.path(), "owned").unwrap();
    }
}
