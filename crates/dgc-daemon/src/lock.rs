//! Single-instance guard: an exclusive `flock` on `<state>/daemon.lock`.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("another dgc daemon already holds the instance lock")]
    AlreadyRunning,
    #[error("instance lock I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Holds the instance lock for as long as the value lives; the kernel releases
/// it when the file is closed, including on crash.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    /// Take the lock in `state_dir` (which must exist) and record our pid.
    pub fn acquire(state_dir: &Path) -> Result<Self, LockError> {
        // No truncate on open: that would wipe the running daemon's pid.
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(state_dir.join("daemon.lock"))?;

        loop {
            // SAFETY: the fd is valid for the duration of the call.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                break;
            }
            let err = io::Error::last_os_error();
            match err.kind() {
                io::ErrorKind::WouldBlock => return Err(LockError::AlreadyRunning),
                io::ErrorKind::Interrupted => continue,
                _ => return Err(err.into()),
            }
        }

        file.set_len(0)?;
        let mut writer = &file;
        writeln!(writer, "{}", std::process::id())?;
        Ok(Self { _file: file })
    }
}
