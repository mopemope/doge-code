//! Per-agent append-only event log: `events.jsonl` plus one rotated generation
//! `events.1.jsonl`, one JSON `AgentEvent` per line.
//!
//! Files are created 0600 and never opened through symlinks. Corrupt lines are
//! skipped on read; warnings carry only the file name and line position.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use dgc_protocol::event::{AgentEvent, EventKind};
use dgc_protocol::ids::AgentId;
use dgc_protocol::methods::MAX_EVENTS_PAGE_BYTES;

use crate::paths::ensure_private_dir;
use crate::time::now_rfc3339;

pub(crate) const CURRENT_FILE: &str = "events.jsonl";
const ROTATED_FILE: &str = "events.1.jsonl";
const TAIL_BLOCK_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum EventLogError {
    #[error("event log I/O error at {}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("refusing to open event log through symlink {}", path.display())]
    Symlink { path: PathBuf },
    #[error("cannot serialize event: {0}")]
    Serialize(#[from] serde_json::Error),
}

fn io_error(path: &Path, source: io::Error) -> EventLogError {
    if source.raw_os_error() == Some(libc::ELOOP) {
        EventLogError::Symlink {
            path: path.to_owned(),
        }
    } else {
        EventLogError::Io {
            path: path.to_owned(),
            source,
        }
    }
}

#[derive(Debug)]
pub struct EventLog {
    current_path: PathBuf,
    rotated_path: PathBuf,
    agent_id: AgentId,
    max_bytes: u64,
    last_seq: u64,
    /// Append handle to the current file.
    file: File,
    /// Size of the current file in bytes.
    size: u64,
    /// The current file was renamed away by a rotation whose reopen failed;
    /// `file` is stale until the next rotation attempt succeeds.
    reopen_pending: bool,
    /// A failed write may have left a partial line that could not be
    /// truncated away; the next write starts with a newline to isolate it.
    needs_newline: bool,
}

impl EventLog {
    /// Open (creating if needed) the log in `agent_dir` for `agent_id`,
    /// restoring the last sequence number from existing files.
    pub fn open(
        agent_dir: &Path,
        agent_id: AgentId,
        max_bytes: u64,
    ) -> Result<Self, EventLogError> {
        ensure_private_dir(agent_dir).map_err(|e| io_error(agent_dir, e))?;
        let current_path = agent_dir.join(CURRENT_FILE);
        let rotated_path = agent_dir.join(ROTATED_FILE);

        let mut file = open_append(&current_path)?;
        let mut size = file
            .metadata()
            .map_err(|e| io_error(&current_path, e))?
            .len();
        if size > 0 && !ends_with_newline(&mut file, size, &current_path)? {
            // A crash mid-write left a partial line; terminate it so the next
            // event starts on its own line.
            file.write_all(b"\n")
                .map_err(|e| io_error(&current_path, e))?;
            size += 1;
        }

        let last_seq = match last_seq_in(&current_path)? {
            Some(seq) => seq,
            None => last_seq_in(&rotated_path)?.unwrap_or(0),
        };
        Ok(Self {
            current_path,
            rotated_path,
            agent_id,
            max_bytes,
            last_seq,
            file,
            size,
            reopen_pending: false,
            needs_newline: false,
        })
    }

    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// Stamp `kind` with the next sequence number and the current time, append
    /// it as one line, and return the stored event.
    pub fn append(&mut self, kind: EventKind) -> Result<AgentEvent, EventLogError> {
        let event = AgentEvent {
            agent_id: self.agent_id.clone(),
            seq: self.last_seq + 1,
            at: now_rfc3339(),
            kind,
        };
        let mut line = serde_json::to_vec(&event)?;
        line.push(b'\n');
        let len = line.len() as u64;

        if self.reopen_pending || (self.size > 0 && self.size + len > self.max_bytes) {
            self.rotate_with(open_append)?;
        }
        match write_line(&mut self.file, self.size, self.needs_newline, &line) {
            Ok(written) => {
                self.needs_newline = false;
                self.size += written;
            }
            Err(failure) => {
                self.needs_newline = failure.dirty;
                return Err(io_error(&self.current_path, failure.source));
            }
        }
        self.last_seq = event.seq;
        Ok(event)
    }

    /// Events with `seq > since_seq`, ascending, at most `limit`, reading the
    /// rotated generation before the current file. The page stops before its
    /// serialized JSON array would exceed [`MAX_EVENTS_PAGE_BYTES`] but always
    /// holds at least one event when any is left, so a short page does not
    /// mean the end: callers page on until an empty page.
    pub fn read_page(&self, since_seq: u64, limit: u32) -> Result<Vec<AgentEvent>, EventLogError> {
        read_page_from(
            [&self.rotated_path, &self.current_path],
            since_seq,
            limit,
            MAX_EVENTS_PAGE_BYTES,
        )
    }

    /// The newest events in ascending sequence order, up to `n` and the
    /// same byte budget as a forward page. Always keep the newest event if
    /// it alone exceeds the budget.
    pub fn read_tail(&self, n: u32) -> Result<Vec<AgentEvent>, EventLogError> {
        self.read_tail_with_bytes(n).map(|(events, _)| events)
    }

    /// Return the number of file bytes read as well, for tests.
    #[doc(hidden)]
    pub fn read_tail_with_bytes(&self, n: u32) -> Result<(Vec<AgentEvent>, u64), EventLogError> {
        read_tail_from([&self.current_path, &self.rotated_path], n)
    }

    /// Rotate the current file away and reopen a fresh one with `open`. State
    /// changes only after each step succeeds, so a failed reopen is retried by
    /// the next call without renaming again.
    fn rotate_with(
        &mut self,
        open: impl FnOnce(&Path) -> Result<File, EventLogError>,
    ) -> Result<(), EventLogError> {
        if !self.reopen_pending {
            // rename(2) replaces an existing events.1.jsonl without following it.
            fs::rename(&self.current_path, &self.rotated_path)
                .map_err(|e| io_error(&self.current_path, e))?;
            self.reopen_pending = true;
        }
        self.file = open(&self.current_path)?;
        self.size = 0;
        self.reopen_pending = false;
        self.needs_newline = false;
        Ok(())
    }
}

/// A log file that can be cut back to a known length.
trait LogFile: Write {
    fn truncate_to(&mut self, len: u64) -> io::Result<()>;
}

impl LogFile for File {
    fn truncate_to(&mut self, len: u64) -> io::Result<()> {
        // Works under O_APPEND: later writes land at the new end.
        self.set_len(len)
    }
}

#[derive(Debug)]
struct WriteFailure {
    source: io::Error,
    /// A partial line may remain on disk (truncation failed too).
    dirty: bool,
}

/// Write `line` (preceded by a newline when `needs_newline`) in one `write_all`
/// plus flush and return the bytes written. On failure, cut the file back to
/// `good_size` so no partial line stays; if even that fails, the caller must
/// isolate the leftovers with a newline on the next write.
fn write_line<F: LogFile>(
    file: &mut F,
    good_size: u64,
    needs_newline: bool,
    line: &[u8],
) -> Result<u64, WriteFailure> {
    let mut buf;
    let data = if needs_newline {
        buf = Vec::with_capacity(line.len() + 1);
        buf.push(b'\n');
        buf.extend_from_slice(line);
        &buf[..]
    } else {
        line
    };
    match file.write_all(data).and_then(|()| file.flush()) {
        Ok(()) => Ok(data.len() as u64),
        Err(source) => {
            let dirty = match file.truncate_to(good_size) {
                Ok(()) => false,
                Err(e) => {
                    tracing::warn!(kind = ?e.kind(), "cannot truncate event log after failed write");
                    true
                }
            };
            Err(WriteFailure { source, dirty })
        }
    }
}

/// Read a handoff snapshot without opening an append handle or repairing
/// files. The caller prevents a concurrent rotation across the two reads.
pub(crate) fn handoff_history(
    dir: &Path,
    limit: u32,
) -> Result<(Vec<AgentEvent>, Vec<AgentEvent>), EventLogError> {
    let current = dir.join(CURRENT_FILE);
    let rotated = dir.join(ROTATED_FILE);
    // Reserve room for one complete RPC-sized initial prompt and the event
    // headers added when it was chunked. The normal response page is smaller.
    let initial_budget = dgc_protocol::MAX_MESSAGE_BYTES + MAX_EVENTS_PAGE_BYTES;
    let first_page = read_page_from([&rotated, &current], 0, limit, initial_budget)?;
    let (tail, _) = read_tail_from([&current, &rotated], limit)?;
    Ok((first_page, tail))
}

fn read_page_from(
    paths: [&Path; 2],
    since_seq: u64,
    limit: u32,
    byte_budget: usize,
) -> Result<Vec<AgentEvent>, EventLogError> {
    let limit = limit as usize;
    let mut page = Vec::new();
    if limit == 0 {
        return Ok(page);
    }
    // `[` + `]`, then each event's line plus a separating comma.
    let mut bytes = 2usize;
    let mut full = false;
    for path in paths {
        for_each_event(path, |event, line_len| {
            if event.seq <= since_seq {
                return true;
            }
            let cost = line_len + usize::from(!page.is_empty());
            if !page.is_empty() && bytes + cost > byte_budget {
                full = true;
                return false;
            }
            bytes += cost;
            page.push(event);
            full = page.len() >= limit;
            !full
        })?;
        if full {
            break;
        }
    }
    Ok(page)
}

fn read_tail_from(paths: [&Path; 2], n: u32) -> Result<(Vec<AgentEvent>, u64), EventLogError> {
    let mut page = Vec::new();
    if n == 0 {
        return Ok((page, 0));
    }
    let mut bytes_read = 0;
    let mut page_bytes = 2;
    let mut full = false;
    for path in paths {
        bytes_read += for_each_event_reverse(path, |event, line_len| {
            let cost = line_len + usize::from(!page.is_empty());
            if !page.is_empty() && page_bytes + cost > MAX_EVENTS_PAGE_BYTES {
                full = true;
                return false;
            }
            page_bytes += cost;
            page.push(event);
            full = page.len() == n as usize || page_bytes >= MAX_EVENTS_PAGE_BYTES;
            !full
        })?;
        if full {
            break;
        }
    }
    page.reverse();
    Ok((page, bytes_read))
}

fn open_append(path: &Path) -> Result<File, EventLogError> {
    OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| io_error(path, e))
}

fn ends_with_newline(file: &mut File, size: u64, path: &Path) -> Result<bool, EventLogError> {
    let mut last = [0u8; 1];
    file.seek(SeekFrom::Start(size - 1))
        .and_then(|_| file.read_exact(&mut last))
        .map_err(|e| io_error(path, e))?;
    // O_APPEND writes always go to the end regardless of the read position.
    Ok(last[0] == b'\n')
}

/// Sequence number of the last valid line in `path`, if the file exists and has one.
fn last_seq_in(path: &Path) -> Result<Option<u64>, EventLogError> {
    let mut last = None;
    for_each_event_reverse(path, |event, _| {
        last = Some(event.seq);
        false
    })?;
    Ok(last)
}

fn open_read(path: &Path) -> Result<Option<File>, EventLogError> {
    match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(path, e)),
    }
}

/// Visit valid events from the end of one file, returning the bytes read.
fn for_each_event_reverse(
    path: &Path,
    mut f: impl FnMut(AgentEvent, usize) -> bool,
) -> Result<u64, EventLogError> {
    let Some(mut file) = open_read(path)? else {
        return Ok(0);
    };
    let mut offset = file.seek(SeekFrom::End(0)).map_err(|e| io_error(path, e))?;
    let end = offset;
    let mut block = vec![0; TAIL_BLOCK_BYTES];
    // Keep a line spanning blocks in reverse byte order until its start is
    // found. Each fragment is appended once, even for very long lines.
    let mut suffix = Vec::new();
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    while offset > 0 {
        let block_start = offset.saturating_sub(TAIL_BLOCK_BYTES as u64);
        let bytes = &mut block[..(offset - block_start) as usize];
        file.seek(SeekFrom::Start(block_start))
            .and_then(|_| file.read_exact(bytes))
            .map_err(|e| io_error(path, e))?;
        let mut line_end = offset;
        offset = block_start;
        for part in bytes.rsplit(|&byte| byte == b'\n') {
            let start = line_end - part.len() as u64;
            if start == offset && offset > 0 {
                suffix.extend(part.iter().rev());
                break;
            }
            let line = if suffix.is_empty() {
                part
            } else {
                suffix.extend(part.iter().rev());
                suffix.reverse();
                &suffix
            }
            .trim_ascii();
            if !line.is_empty() {
                match serde_json::from_slice::<AgentEvent>(line) {
                    Ok(event) => {
                        if !f(event, line.len()) {
                            return Ok(end - offset);
                        }
                    }
                    Err(_) => {
                        tracing::warn!(file = %name, offset = start, "skipping corrupt event log line")
                    }
                }
            }
            suffix.clear();
            line_end = start.saturating_sub(1);
        }
    }
    Ok(end)
}

/// Call `f` for each valid event in `path` (with the byte length of its
/// trimmed line, which is its serialized size), in file order, until it
/// returns false. A missing file is empty; a symlink is an error.
fn for_each_event(
    path: &Path,
    mut f: impl FnMut(AgentEvent, usize) -> bool,
) -> Result<(), EventLogError> {
    let Some(file) = open_read(path)? else {
        return Ok(());
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    let mut line_no = 0usize;
    loop {
        buf.clear();
        let n = reader
            .read_until(b'\n', &mut buf)
            .map_err(|e| io_error(path, e))?;
        if n == 0 {
            return Ok(());
        }
        line_no += 1;
        let line = buf.trim_ascii();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice::<AgentEvent>(line) {
            Ok(event) => {
                if !f(event, line.len()) {
                    return Ok(());
                }
            }
            Err(_) => {
                tracing::warn!(file = %name, line = line_no, "skipping corrupt event log line")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory log file whose writes can fail after a short write.
    struct FakeFile {
        data: Vec<u8>,
        /// Accept at most this many more bytes, then fail with ENOSPC.
        budget: usize,
        truncate_fails: bool,
    }

    impl FakeFile {
        fn new(data: &[u8], budget: usize, truncate_fails: bool) -> Self {
            Self {
                data: data.to_vec(),
                budget,
                truncate_fails,
            }
        }
    }

    impl Write for FakeFile {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.budget == 0 {
                return Err(io::Error::from_raw_os_error(libc::ENOSPC));
            }
            let n = buf.len().min(self.budget);
            self.data.extend_from_slice(&buf[..n]);
            self.budget -= n;
            Ok(n)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl LogFile for FakeFile {
        fn truncate_to(&mut self, len: u64) -> io::Result<()> {
            if self.truncate_fails {
                return Err(io::Error::other("truncate refused"));
            }
            self.data.truncate(len as usize);
            Ok(())
        }
    }

    #[test]
    fn short_write_is_truncated_away() {
        let good = b"{\"a\":1}\n";
        let mut file = FakeFile::new(good, 5, false);
        let failure = write_line(&mut file, good.len() as u64, false, b"{\"b\":2}\n")
            .expect_err("write must fail");
        assert!(!failure.dirty);
        assert_eq!(file.data, good, "partial bytes must be removed");

        // Space returns: the next write lands cleanly after the good line.
        file.budget = usize::MAX;
        let n = write_line(&mut file, good.len() as u64, false, b"{\"c\":3}\n").expect("write");
        assert_eq!(n, 8);
        assert_eq!(file.data, b"{\"a\":1}\n{\"c\":3}\n");
    }

    #[test]
    fn failed_truncate_marks_dirty_and_next_write_isolates_leftovers() {
        let good = b"{\"a\":1}\n";
        let mut file = FakeFile::new(good, 5, true);
        let failure = write_line(&mut file, good.len() as u64, false, b"{\"b\":2}\n")
            .expect_err("write must fail");
        assert!(failure.dirty);
        assert_eq!(file.data.len(), good.len() + 5, "partial bytes remain");

        file.budget = usize::MAX;
        let n = write_line(&mut file, good.len() as u64, true, b"{\"c\":3}\n").expect("write");
        assert_eq!(n, 9, "newline prefix is counted");
        let text = String::from_utf8(file.data).expect("utf8");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "{\"a\":1}");
        assert_eq!(lines[2], "{\"c\":3}", "new event is on its own line");
    }

    #[test]
    fn failed_reopen_is_retried_without_renaming_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log = EventLog::open(dir.path(), AgentId::generate(), 1 << 20).expect("open");
        log.append(EventKind::TurnStarted).expect("append");

        let err = log
            .rotate_with(|_| {
                Err(EventLogError::Serialize(serde::de::Error::custom(
                    "injected",
                )))
            })
            .expect_err("reopen must fail");
        assert!(matches!(err, EventLogError::Serialize(_)));
        assert!(log.reopen_pending);
        assert!(dir.path().join(ROTATED_FILE).exists());

        // The next append retries only the reopen and then succeeds.
        let ev = log.append(EventKind::TurnStarted).expect("append");
        assert_eq!(ev.seq, 2);
        assert!(!log.reopen_pending);
        let seqs: Vec<u64> = log
            .read_page(0, 10)
            .expect("read")
            .iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(seqs, vec![1, 2]);
        let rotated = fs::read_to_string(dir.path().join(ROTATED_FILE)).expect("rotated");
        let current = fs::read_to_string(dir.path().join(CURRENT_FILE)).expect("current");
        assert_eq!(rotated.lines().count(), 1);
        assert_eq!(current.lines().count(), 1);
    }

    #[test]
    fn truncate_works_on_real_append_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CURRENT_FILE);
        let mut file = open_append(&path).expect("open");
        file.write_all(b"good\npartial").expect("write");
        file.truncate_to(5).expect("truncate");
        file.write_all(b"next\n").expect("write");
        assert_eq!(fs::read(&path).expect("read"), b"good\nnext\n");
    }
}
