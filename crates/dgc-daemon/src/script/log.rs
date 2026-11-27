//! Private append-only log with one retained rotation and byte cursors.

use std::ffi::{CStr, CString};
use std::fs::{File, OpenOptions, Permissions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex};

use dgc_protocol::ids::WorkspaceId;
use dgc_protocol::methods::ScriptLogsResult;

const NOFOLLOW: libc::c_int = libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;

pub(super) struct LogFiles {
    dir: File,
    active: CString,
    previous: CString,
}

impl LogFiles {
    pub(super) fn open(
        state_dir: &Path,
        id: &WorkspaceId,
        name: &str,
        create: bool,
    ) -> io::Result<Option<Self>> {
        let root = OpenOptions::new()
            .read(true)
            .custom_flags(NOFOLLOW | libc::O_DIRECTORY)
            .open(state_dir)?;
        let Some(scripts) = open_dir(&root, c"scripts", create)? else {
            return Ok(None);
        };
        let id = CString::new(id.as_str())?;
        let Some(dir) = open_dir(&scripts, &id, create)? else {
            return Ok(None);
        };
        Ok(Some(Self {
            dir,
            active: CString::new(format!("{name}.log"))?,
            previous: CString::new(format!("{name}.log.1"))?,
        }))
    }

    pub(super) fn page(
        &self,
        oldest: u64,
        offset: Option<u64>,
        max_bytes: u32,
        running: bool,
    ) -> io::Result<ScriptLogsResult> {
        let (mut previous, previous_len) = open_for_read(&self.dir, &self.previous)?;
        let (mut active, active_len) = open_for_read(&self.dir, &self.active)?;
        let boundary = oldest + previous_len;
        let end = boundary + active_len;
        let limit = u64::from(max_bytes);
        let start = offset
            .unwrap_or_else(|| end.saturating_sub(limit))
            .clamp(oldest, end);
        // Look back far enough to identify a character crossing the tail's
        // start while keeping genuine invalid continuation bytes lossy.
        let prefix = if offset.is_none() && limit > 0 {
            (start - oldest).min(3)
        } else {
            0
        };
        let read_start = start - prefix;
        let read_limit = limit + prefix;
        let mut bytes = Vec::new();
        let previous_bytes = boundary.saturating_sub(read_start).min(read_limit);
        if let Some(file) = previous.as_mut().filter(|_| previous_bytes > 0) {
            file.seek(SeekFrom::Start(read_start - oldest))?;
            file.take(previous_bytes).read_to_end(&mut bytes)?;
        }
        if let Some(file) = active.as_mut() {
            file.seek(SeekFrom::Start(read_start.saturating_sub(boundary)))?;
            file.take(read_limit - bytes.len() as u64)
                .read_to_end(&mut bytes)?;
        }
        let from = align_tail_start(&bytes, (prefix as usize).min(bytes.len()), running);
        let mut to = bytes.len();
        if running || read_start + (to as u64) < end {
            to -= incomplete_suffix_len(&bytes[from..]);
        }
        Ok(ScriptLogsResult {
            data: String::from_utf8_lossy(&bytes[from..to]).into_owned(),
            start_offset: read_start + from as u64,
            next_offset: read_start + to as u64,
            running,
        })
    }
}

fn continuation(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

fn align_tail_start(bytes: &[u8], start: usize, running: bool) -> usize {
    let Some(lead) = bytes[..start].iter().rposition(|&byte| !continuation(byte)) else {
        return start;
    };
    let candidate = &bytes[lead..bytes.len().min(lead + 4)];
    if let Some(character) = candidate
        .utf8_chunks()
        .next()
        .and_then(|chunk| chunk.valid().chars().next())
    {
        return start.max(lead + character.len_utf8());
    }
    if running && incomplete_suffix_len(candidate) == candidate.len() {
        // A live tail can begin inside a character still being written.
        // Keep its leading byte available for the next request.
        return lead;
    }
    start
}

fn incomplete_suffix_len(bytes: &[u8]) -> usize {
    let Some(back) = bytes
        .iter()
        .rev()
        .take(3)
        .position(|&byte| !continuation(byte))
    else {
        return 0;
    };
    let len = back + 1;
    match std::str::from_utf8(&bytes[bytes.len() - len..]) {
        Err(err) if err.error_len().is_none() => len,
        _ => 0,
    }
}

fn regular(file: &File) -> io::Result<u64> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "script log is not regular",
        ));
    }
    if metadata.permissions().mode() & 0o777 != 0o600 {
        file.set_permissions(Permissions::from_mode(0o600))?;
    }
    Ok(metadata.len())
}

fn open_relative(parent: &File, name: &CStr, flags: libc::c_int) -> io::Result<File> {
    // SAFETY: parent is open, name is NUL-terminated, and the mode is passed
    // using an integer type that satisfies C variadic argument promotions.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | NOFOLLOW,
            0o600 as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd was just opened successfully and is owned here.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn open_dir(parent: &File, name: &CStr, create: bool) -> io::Result<Option<File>> {
    if create {
        // SAFETY: parent is open and name is NUL-terminated.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
    }
    let file = match open_relative(parent, name, libc::O_RDONLY | libc::O_DIRECTORY) {
        Ok(file) => file,
        Err(error) if !create && error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    // SAFETY: geteuid has no preconditions.
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "script log directory is owned by another user",
        ));
    }
    if metadata.permissions().mode() & 0o777 != 0o700 {
        file.set_permissions(Permissions::from_mode(0o700))?;
    }
    Ok(Some(file))
}

fn open_for_read(dir: &File, name: &CStr) -> io::Result<(Option<File>, u64)> {
    match open_relative(dir, name, libc::O_RDONLY) {
        Ok(file) => {
            let len = regular(&file)?;
            Ok((Some(file), len))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok((None, 0)),
        Err(err) => Err(err),
    }
}

fn open_for_append(dir: &File, name: &CStr, new: bool) -> io::Result<File> {
    let flags = libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT;
    open_relative(dir, name, flags | if new { libc::O_EXCL } else { 0 })
}

pub(super) struct LogWriter {
    files: LogFiles,
    file: File,
    len: u64,
    previous_len: u64,
    limit: u64,
    /// Byte position of the oldest retained log data. The lock serializes
    /// writes and rotation with log reads, including across separate runs.
    oldest: Arc<Mutex<u64>>,
}

impl LogWriter {
    pub(super) fn open(files: LogFiles, limit: u64, oldest: Arc<Mutex<u64>>) -> io::Result<Self> {
        let (_, previous_len) = open_for_read(&files.dir, &files.previous)?;
        let file = open_for_append(&files.dir, &files.active, false)?;
        let len = regular(&file)?;
        Ok(Self {
            files,
            file,
            len,
            previous_len,
            limit,
            oldest,
        })
    }

    pub(super) fn append(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        let mut oldest = self.oldest.lock().unwrap_or_else(|err| err.into_inner());
        while !bytes.is_empty() {
            if self.len >= self.limit {
                // Refuse a replaced symlink or special file before rotating.
                let files = &self.files;
                open_for_read(&files.dir, &files.active)?;
                open_for_read(&files.dir, &files.previous)?;
                // SAFETY: the directory descriptor is open and both names
                // are NUL-terminated. renameat does not follow the final entry.
                let dir = files.dir.as_raw_fd();
                if unsafe {
                    libc::renameat(dir, files.active.as_ptr(), dir, files.previous.as_ptr())
                } != 0
                {
                    return Err(io::Error::last_os_error());
                }
                *oldest += self.previous_len;
                self.previous_len = self.len;
                self.file = open_for_append(&files.dir, &files.active, true)?;
                self.len = regular(&self.file)?;
            }
            let count = ((self.limit - self.len) as usize).min(bytes.len());
            self.file.write_all(&bytes[..count])?;
            self.len += count as u64;
            bytes = &bytes[count..];
        }
        Ok(())
    }
}
