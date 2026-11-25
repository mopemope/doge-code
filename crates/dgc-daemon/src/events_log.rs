//! Per-agent append-only event log: `events.jsonl` plus one rotated generation
//! `events.1.jsonl`, one JSON `AgentEvent` per line.
//!
//! Files are created 0600 and never opened through symlinks. Corrupt lines are
//! skipped on read; warnings carry the file name and line number only, never
//! content.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use dgc_protocol::event::{AgentEvent, EventKind};
use dgc_protocol::ids::AgentId;

use crate::paths::ensure_private_dir;
use crate::time::now_rfc3339;

const CURRENT_FILE: &str = "events.jsonl";
const ROTATED_FILE: &str = "events.1.jsonl";

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

        if self.size > 0 && self.size + len > self.max_bytes {
            self.rotate()?;
        }
        self.file
            .write_all(&line)
            .and_then(|()| self.file.flush())
            .map_err(|e| io_error(&self.current_path, e))?;
        self.size += len;
        self.last_seq = event.seq;
        Ok(event)
    }

    /// Events with `seq > since_seq`, ascending, at most `limit`, reading the
    /// rotated generation before the current file.
    pub fn read_page(&self, since_seq: u64, limit: u32) -> Result<Vec<AgentEvent>, EventLogError> {
        let limit = limit as usize;
        let mut page = Vec::new();
        if limit == 0 {
            return Ok(page);
        }
        for path in [&self.rotated_path, &self.current_path] {
            for_each_event(path, |event| {
                if event.seq > since_seq {
                    page.push(event);
                }
                page.len() < limit
            })?;
            if page.len() >= limit {
                break;
            }
        }
        Ok(page)
    }

    fn rotate(&mut self) -> Result<(), EventLogError> {
        // rename(2) replaces an existing events.1.jsonl without following it.
        fs::rename(&self.current_path, &self.rotated_path)
            .map_err(|e| io_error(&self.current_path, e))?;
        self.file = open_append(&self.current_path)?;
        self.size = 0;
        Ok(())
    }
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
    for_each_event(path, |event| {
        last = Some(event.seq);
        true
    })?;
    Ok(last)
}

/// Call `f` for each valid event in `path`, in file order, until it returns
/// false. A missing file is empty; a symlink is an error.
fn for_each_event(path: &Path, mut f: impl FnMut(AgentEvent) -> bool) -> Result<(), EventLogError> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_error(path, e)),
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
                if !f(event) {
                    return Ok(());
                }
            }
            Err(_) => {
                tracing::warn!(file = %name, line = line_no, "skipping corrupt event log line")
            }
        }
    }
}
