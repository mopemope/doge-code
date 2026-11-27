//! Single-instance guard: an exclusive `flock` on `<state>/daemon.lock`.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
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

        if !try_exclusive(&file)? {
            return Err(LockError::AlreadyRunning);
        }

        file.set_len(0)?;
        let mut writer = &file;
        writeln!(writer, "{}", std::process::id())?;
        Ok(Self { _file: file })
    }
}

/// Request graceful shutdown when a protocol mismatch prevents an RPC stop.
pub fn terminate_running_daemon(state_dir: &Path) -> io::Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(state_dir.join("daemon.lock"))?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "daemon.lock is not a regular file",
        ));
    }

    // Read and probe the same file without modifying the pid record. Check the
    // lock immediately before using the pid because the daemon can be exiting.
    const PID_READ_LIMIT: u64 = 32;
    let mut recorded = String::new();
    (&file).take(PID_READ_LIMIT).read_to_string(&mut recorded)?;
    let pid = recorded
        .trim()
        .parse::<libc::pid_t>()
        .ok()
        .filter(|&pid| pid > 1 && (recorded.len() as u64) < PID_READ_LIMIT);
    if try_exclusive(&file)? {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no daemon holds the instance lock",
        ));
    }
    let pid = pid.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "daemon.lock has no valid daemon pid",
        )
    })?;
    // SAFETY: a positive pid greater than 1 targets a single process. The
    // currently held instance lock is required before using the recorded pid.
    if unsafe { libc::kill(pid, libc::SIGTERM) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn try_exclusive(file: &File) -> io::Result<bool> {
    loop {
        // SAFETY: the fd is valid for the duration of the call.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(true);
        }
        let err = io::Error::last_os_error();
        match err.kind() {
            io::ErrorKind::WouldBlock => return Ok(false),
            io::ErrorKind::Interrupted => continue,
            _ => return Err(err),
        }
    }
}
