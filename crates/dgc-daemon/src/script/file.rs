//! Load and validate one workspace script configuration snapshot.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Deserializer};

const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_COMMAND_BYTES: usize = 4 * 1024;
const MAX_SCRIPTS: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum ScriptFileError {
    #[error("missing")]
    Missing,
    #[error("not_regular")]
    NotRegular,
    #[error("too_large")]
    TooLarge,
    #[error("invalid: {reason}")]
    Invalid { reason: &'static str },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptDef {
    pub command: String,
    #[serde(default, deserialize_with = "relative_path")]
    pub cwd: Option<PathBuf>,
}

fn relative_path<'de, D: Deserializer<'de>>(de: D) -> Result<Option<PathBuf>, D::Error> {
    PathBuf::deserialize(de).map(Some)
}

#[derive(Debug)]
pub struct ScriptFile {
    pub bytes: Vec<u8>,
    pub scripts: BTreeMap<String, ScriptDef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Format {
    version: u32,
    scripts: BTreeMap<String, ScriptDef>,
}

fn invalid(reason: &'static str) -> ScriptFileError {
    ScriptFileError::Invalid { reason }
}

pub(super) fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl ScriptFile {
    pub fn read(workspace_root: &Path) -> Result<Self, ScriptFileError> {
        let path = workspace_root.join("dgc.json");
        match fs::symlink_metadata(&path) {
            Ok(info) if !info.is_file() => return Err(ScriptFileError::NotRegular),
            Ok(_) => {}
            Err(err) => return Err(open_error(err)),
        }
        // NONBLOCK keeps a FIFO swapped in after the preliminary check from
        // blocking the daemon. Check the opened file as well.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(open_error)?;
        let info = file
            .metadata()
            .map_err(|_| invalid("cannot stat dgc.json"))?;
        if !info.is_file() {
            return Err(ScriptFileError::NotRegular);
        }
        if info.len() > MAX_FILE_BYTES {
            return Err(ScriptFileError::TooLarge);
        }
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid("cannot read dgc.json"))?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(ScriptFileError::TooLarge);
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| invalid("dgc.json must be UTF-8"))?;
        // Serde errors can echo user-controlled keys or values.
        let format: Format =
            serde_json::from_str(text).map_err(|_| invalid("invalid dgc.json structure"))?;
        if format.version != 1 {
            return Err(invalid("unsupported dgc.json version"));
        }
        if format.scripts.len() > MAX_SCRIPTS {
            return Err(invalid("dgc.json has more than 32 scripts"));
        }
        for (name, def) in &format.scripts {
            if !valid_name(name) {
                return Err(invalid("invalid script name"));
            }
            if def.command.is_empty()
                || def.command.len() > MAX_COMMAND_BYTES
                || def.command.contains('\0')
            {
                return Err(invalid("command must have 1 to 4096 bytes and no NUL"));
            }
            if let Some(cwd) = &def.cwd {
                resolve_cwd(workspace_root, cwd)?;
            }
        }
        Ok(Self {
            bytes,
            scripts: format.scripts,
        })
    }
}

fn open_error(error: io::Error) -> ScriptFileError {
    if error.kind() == io::ErrorKind::NotFound {
        ScriptFileError::Missing
    } else if error.raw_os_error() == Some(libc::ELOOP) {
        ScriptFileError::NotRegular
    } else {
        invalid("cannot open dgc.json")
    }
}

fn resolve_cwd(root: &Path, cwd: &Path) -> Result<(PathBuf, PathBuf), ScriptFileError> {
    if cwd.as_os_str().is_empty()
        || !cwd
            .components()
            .all(|c| matches!(c, Component::CurDir | Component::Normal(_)))
    {
        return Err(invalid("cwd must be relative with no parent traversal"));
    }
    let root = root
        .canonicalize()
        .map_err(|_| invalid("cannot resolve workspace path"))?;
    let resolved = root
        .join(cwd)
        .canonicalize()
        .map_err(|_| invalid("cannot resolve cwd"))?;
    let relative = resolved
        .strip_prefix(&root)
        .map_err(|_| invalid("cwd escapes workspace"))?;
    if !resolved.is_dir() {
        return Err(invalid("cwd must be a directory"));
    }
    Ok((root, relative.to_owned()))
}

/// Pin the verified directory for spawn. Resolve internal symlinks first, then
/// open each canonical component relative to its parent without following links.
pub(super) fn open_cwd(root: &Path, cwd: Option<&Path>) -> Result<File, ScriptFileError> {
    let (root, relative) = resolve_cwd(root, cwd.unwrap_or(Path::new(".")))?;
    const FLAGS: libc::c_int = libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let error = |_| invalid("cannot open verified cwd");
    let mut dir = OpenOptions::new()
        .read(true)
        .custom_flags(FLAGS)
        .open(root)
        .map_err(error)?;
    for component in relative.iter() {
        let name = CString::new(component.as_bytes()).map_err(|_| invalid("invalid cwd"))?;
        // SAFETY: dir is open and name is NUL-terminated. On success openat
        // returns a new owned descriptor that is closed by File.
        let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | FLAGS) };
        if fd < 0 {
            return Err(error(io::Error::last_os_error()));
        }
        // SAFETY: fd was just opened successfully and is owned here.
        dir = unsafe { File::from_raw_fd(fd) };
    }
    Ok(dir)
}
