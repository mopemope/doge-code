//! JSON entity store: one file per entity under `<state>/{projects,workspaces,agents}/`.
//!
//! Files hold `{"version":1,"data":{...}}`. Writes are atomic (temp file in the
//! same directory, fsync, rename), created 0600, and never follow symlinks.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use dgc_protocol::ids::{AgentId, ProjectId, WorkspaceId};
use dgc_protocol::model::{Agent, Project, Workspace};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::paths::ensure_private_dir;

/// Version written to, and the only one accepted from, entity files.
pub const STORE_VERSION: u32 = 1;

const PROJECTS_DIR: &str = "projects";
const WORKSPACES_DIR: &str = "workspaces";
const AGENTS_DIR: &str = "agents";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("store I/O error at {}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("refusing to write through symlink {}", path.display())]
    Symlink { path: PathBuf },
    #[error("invalid store file {}: {source}", path.display())]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("store file {} has unsupported version {found} (expected {STORE_VERSION})", path.display())]
    UnsupportedVersion { path: PathBuf, found: u64 },
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> StoreError + '_ {
    move |source| StoreError::Io {
        path: path.to_owned(),
        source,
    }
}

#[derive(Serialize)]
struct EnvelopeRef<'a, T> {
    version: u32,
    data: &'a T,
}

#[derive(serde::Deserialize)]
struct EnvelopeIn {
    version: u64,
    data: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct Store {
    state_dir: PathBuf,
}

impl Store {
    /// Open the store under `state_dir`, creating it and the entity directories
    /// with mode 0700. Symlinked or foreign-owned directories are refused.
    pub fn open(state_dir: &Path) -> Result<Self, StoreError> {
        ensure_private_dir(state_dir).map_err(io_err(state_dir))?;
        for sub in [PROJECTS_DIR, WORKSPACES_DIR, AGENTS_DIR] {
            let dir = state_dir.join(sub);
            ensure_private_dir(&dir).map_err(io_err(&dir))?;
        }
        Ok(Self {
            state_dir: state_dir.to_owned(),
        })
    }

    fn file_path(&self, sub: &str, id: &str) -> PathBuf {
        // Ids are validated on construction (prefix + hex), so they cannot
        // contain path separators.
        self.state_dir.join(sub).join(format!("{id}.json"))
    }

    fn put<T: Serialize>(&self, sub: &str, id: &str, value: &T) -> Result<(), StoreError> {
        let path = self.file_path(sub, id);
        let bytes = serde_json::to_vec_pretty(&EnvelopeRef {
            version: STORE_VERSION,
            data: value,
        })
        .map_err(|source| StoreError::Json {
            path: path.clone(),
            source,
        })?;
        write_atomic(&path, &bytes)
    }

    fn get<T: DeserializeOwned>(&self, sub: &str, id: &str) -> Result<Option<T>, StoreError> {
        read_entity(&self.file_path(sub, id))
    }

    fn list<T: DeserializeOwned>(&self, sub: &str) -> Result<Vec<T>, StoreError> {
        let dir = self.state_dir.join(sub);
        let mut files: Vec<OsString> = Vec::new();
        for entry in fs::read_dir(&dir).map_err(io_err(&dir))? {
            let entry = entry.map_err(io_err(&dir))?;
            let name = entry.file_name();
            if Path::new(&name).extension().is_some_and(|e| e == "json") {
                files.push(name);
            }
        }
        // Ids embed a v7 uuid, so name order is creation order.
        files.sort();
        let mut out = Vec::with_capacity(files.len());
        for name in files {
            if let Some(value) = read_entity(&dir.join(name))? {
                out.push(value);
            }
        }
        Ok(out)
    }

    fn remove(&self, sub: &str, id: &str) -> Result<bool, StoreError> {
        let path = self.file_path(sub, id);
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(io_err(&path)(err)),
        }
    }

    pub fn put_project(&self, project: &Project) -> Result<(), StoreError> {
        self.put(PROJECTS_DIR, project.id.as_str(), project)
    }
    pub fn get_project(&self, id: &ProjectId) -> Result<Option<Project>, StoreError> {
        self.get(PROJECTS_DIR, id.as_str())
    }
    pub fn list_projects(&self) -> Result<Vec<Project>, StoreError> {
        self.list(PROJECTS_DIR)
    }
    /// Returns whether a file was removed.
    pub fn remove_project(&self, id: &ProjectId) -> Result<bool, StoreError> {
        self.remove(PROJECTS_DIR, id.as_str())
    }

    pub fn put_workspace(&self, workspace: &Workspace) -> Result<(), StoreError> {
        self.put(WORKSPACES_DIR, workspace.id.as_str(), workspace)
    }
    pub fn get_workspace(&self, id: &WorkspaceId) -> Result<Option<Workspace>, StoreError> {
        self.get(WORKSPACES_DIR, id.as_str())
    }
    pub fn list_workspaces(&self) -> Result<Vec<Workspace>, StoreError> {
        self.list(WORKSPACES_DIR)
    }
    /// Returns whether a file was removed.
    pub fn remove_workspace(&self, id: &WorkspaceId) -> Result<bool, StoreError> {
        self.remove(WORKSPACES_DIR, id.as_str())
    }

    pub fn put_agent(&self, agent: &Agent) -> Result<(), StoreError> {
        self.put(AGENTS_DIR, agent.id.as_str(), agent)
    }
    pub fn get_agent(&self, id: &AgentId) -> Result<Option<Agent>, StoreError> {
        self.get(AGENTS_DIR, id.as_str())
    }
    pub fn list_agents(&self) -> Result<Vec<Agent>, StoreError> {
        self.list(AGENTS_DIR)
    }
    /// Returns whether a file was removed.
    pub fn remove_agent(&self, id: &AgentId) -> Result<bool, StoreError> {
        self.remove(AGENTS_DIR, id.as_str())
    }
}

/// Read one entity file; `None` if it does not exist. Symlinks are refused.
fn read_entity<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, StoreError> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) if err.raw_os_error() == Some(libc::ELOOP) => {
            return Err(StoreError::Symlink {
                path: path.to_owned(),
            });
        }
        Err(err) => return Err(io_err(path)(err)),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(io_err(path))?;

    let json_err = |source| StoreError::Json {
        path: path.to_owned(),
        source,
    };
    let envelope: EnvelopeIn = serde_json::from_slice(&bytes).map_err(json_err)?;
    if envelope.version != u64::from(STORE_VERSION) {
        return Err(StoreError::UnsupportedVersion {
            path: path.to_owned(),
            found: envelope.version,
        });
    }
    serde_json::from_value(envelope.data)
        .map(Some)
        .map_err(json_err)
}

fn refuse_symlink(path: &Path) -> Result<(), StoreError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(StoreError::Symlink {
            path: path.to_owned(),
        }),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(io_err(path)(err)),
    }
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Replace `path` with `bytes` atomically: write a 0600 temp file next to it
/// (`O_NOFOLLOW | O_CREAT | O_EXCL`), fsync, then rename over the target.
///
/// Fails, leaving everything untouched, if `path` or its parent directory is a
/// symlink (a plain rename would otherwise silently replace the link).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io_err(path)(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path has no parent directory or file name",
        )));
    };
    refuse_symlink(parent)?;
    refuse_symlink(path)?;

    let mut attempt = 0;
    let (temp_path, mut file) = loop {
        let mut temp_name = OsString::from(".tmp-");
        temp_name.push(name);
        temp_name.push(format!(
            ".{}.{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let temp_path = parent.join(temp_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temp_path)
        {
            Ok(file) => break (temp_path, file),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists && attempt < 8 => attempt += 1,
            Err(err) => return Err(io_err(&temp_path)(err)),
        }
    };

    let result = write_and_rename(&mut file, &temp_path, path, bytes);
    if result.is_err() {
        // Best effort: the original error is what matters.
        let _ = fs::remove_file(&temp_path);
    }
    result?;

    // Persist the rename itself.
    File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(io_err(parent))
}

fn write_and_rename(
    file: &mut File,
    temp_path: &Path,
    target: &Path,
    bytes: &[u8],
) -> Result<(), StoreError> {
    file.write_all(bytes).map_err(io_err(temp_path))?;
    file.sync_all().map_err(io_err(temp_path))?;
    // Re-check just before the rename: it would replace a symlink silently.
    refuse_symlink(target)?;
    fs::rename(temp_path, target).map_err(io_err(target))
}
