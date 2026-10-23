//! Bounded private project-local input recall; canonical sessions are independent.
use anyhow::{Context, Result};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub(crate) const MAX_BYTES: usize = 1024 * 1024;
const MAX_ENTRIES: usize = 1000;
#[cfg(not(unix))]
const NAME: &str = "input_history.json";

pub(crate) struct InputHistoryStore(PathBuf);
impl InputHistoryStore {
    pub(crate) fn at(root: &Path) -> Self {
        Self(root.into())
    }
    pub(crate) fn load(&self) -> Result<Vec<String>> {
        let Some(directory) = Directory::open(&self.0, false)? else {
            return Ok(Vec::new());
        };
        let Some(file) = directory.existing()? else {
            return Ok(Vec::new());
        };
        validate(&file, false)?;
        anyhow::ensure!(
            file.metadata()?.len() <= MAX_BYTES as u64,
            "input history exceeds byte limit"
        );
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() <= MAX_BYTES, "input history exceeds byte limit");
        let entries: Vec<String> =
            serde_json::from_slice(&bytes).context("invalid input history JSON")?;
        Ok(entries
            .into_iter()
            .rev()
            .take(MAX_ENTRIES)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect())
    }
    pub(crate) fn save(&self, entries: &[String]) -> Result<()> {
        // Serialize through a bounded writer before touching persistent state.
        struct Bytes(Vec<u8>);
        impl Write for Bytes {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.0.len().saturating_add(bytes.len()) > MAX_BYTES {
                    return Err(std::io::Error::other("input history exceeds byte limit"));
                }
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut bytes = Bytes(Vec::new());
        serde_json::to_writer(
            &mut bytes,
            &entries[entries.len().saturating_sub(MAX_ENTRIES)..],
        )?;
        let directory =
            Directory::open(&self.0, true)?.context("input history directory missing")?;
        if let Some(existing) = directory.existing()? {
            validate(&existing, true)?;
        }
        directory.replace(&bytes.0)
    }
}

fn validate(file: &std::fs::File, writing: bool) -> Result<()> {
    let meta = file.metadata()?;
    anyhow::ensure!(
        meta.is_file() && (!writing || !meta.permissions().readonly()),
        "unsafe or read-only input history"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid does not access memory.
        anyhow::ensure!(
            meta.nlink() == 1 && meta.uid() == unsafe { libc::geteuid() },
            "unsafe input history ownership or links"
        );
    }
    Ok(())
}

#[cfg(unix)]
struct Directory(std::fs::File);
#[cfg(unix)]
impl Directory {
    fn open(root: &Path, create: bool) -> Result<Option<Self>> {
        use std::os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::{MetadataExt, OpenOptionsExt},
        };
        let mut parent = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(root)?;
        for name in [c".doge", c"sessions"] {
            if create {
                // SAFETY: held parent descriptor and a fixed single component.
                if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(error.into());
                    }
                }
            }
            // SAFETY: a successful openat yields one newly owned descriptor.
            let fd = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                let error = std::io::Error::last_os_error();
                if !create && error.kind() == std::io::ErrorKind::NotFound {
                    return Ok(None);
                }
                return Err(error).context("unsafe input history directory");
            }
            // SAFETY: fd is owned exactly once after successful openat.
            parent = unsafe { std::fs::File::from_raw_fd(fd) };
            anyhow::ensure!(
                parent.metadata()?.uid() == unsafe { libc::geteuid() },
                "foreign-owned input history directory"
            );
        }
        Ok(Some(Self(parent)))
    }
    fn existing(&self) -> Result<Option<std::fs::File>> {
        use std::os::fd::{AsRawFd, FromRawFd};
        // O_NONBLOCK prevents a malicious FIFO from blocking even before validation.
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                c"input_history.json".as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(error).context("unsafe input history file");
        }
        Ok(Some(unsafe { std::fs::File::from_raw_fd(fd) }))
    }
    fn replace(&self, bytes: &[u8]) -> Result<()> {
        use std::os::fd::{AsRawFd, FromRawFd};
        let name = std::ffi::CString::new(format!(".input-history-{}.tmp", uuid::Uuid::new_v4()))?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        };
        anyhow::ensure!(
            fd >= 0,
            "create private input history: {}",
            std::io::Error::last_os_error()
        );
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        let result = (|| -> Result<()> {
            file.write_all(bytes)?;
            file.sync_all()?;
            if let Some(existing) = self.existing()? {
                validate(&existing, true)?;
            }
            if unsafe {
                libc::renameat(
                    self.0.as_raw_fd(),
                    name.as_ptr(),
                    self.0.as_raw_fd(),
                    c"input_history.json".as_ptr(),
                )
            } < 0
            {
                return Err(std::io::Error::last_os_error()).context("replace input history");
            }
            self.0
                .sync_all()
                .context("input history replaced, directory durability unconfirmed")
        })();
        // Removing the unique unpublished sibling is harmless after a successful rename.
        unsafe {
            libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), 0);
        }
        result
    }
}

#[cfg(not(unix))]
struct Directory(PathBuf);
#[cfg(not(unix))]
impl Directory {
    fn open(root: &Path, create: bool) -> Result<Option<Self>> {
        let mut path = root.canonicalize()?;
        for name in [".doge", "sessions"] {
            path.push(name);
            if create
                && let Err(error) = std::fs::create_dir(&path)
                && error.kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(error.into());
            }
            match std::fs::symlink_metadata(&path) {
                Ok(meta) => anyhow::ensure!(
                    meta.is_dir() && !meta.file_type().is_symlink(),
                    "unsafe input history directory"
                ),
                Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(None);
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(Some(Self(path)))
    }
    fn existing(&self) -> Result<Option<std::fs::File>> {
        let path = self.0.join(NAME);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) => anyhow::ensure!(
                meta.is_file() && !meta.file_type().is_symlink(),
                "unsafe input history"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        Ok(Some(std::fs::File::open(path)?))
    }
    fn replace(&self, bytes: &[u8]) -> Result<()> {
        let mut temporary = tempfile::NamedTempFile::new_in(&self.0)?;
        temporary.write_all(bytes)?;
        temporary.as_file().sync_all()?;
        if let Some(file) = self.existing()? {
            validate(&file, true)?;
        }
        temporary.persist(self.0.join(NAME)).map_err(|e| e.error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_retention_capacity_and_failed_save_preserve_old_history() {
        let root = tempfile::tempdir().expect("fixture");
        let store = InputHistoryStore::at(root.path());
        assert!(store.load().expect("missing").is_empty());
        let entries: Vec<_> = (0..1002)
            .map(|i| format!("日本語 {i}\nsecond line"))
            .collect();
        store.save(&entries).expect("save");
        assert_eq!(store.load().expect("load"), entries[2..]);
        let file = root.path().join(".doge/sessions/input_history.json");
        let old = std::fs::read(&file).expect("old");
        assert!(store.save(&["x".repeat(MAX_BYTES)]).is_err());
        assert_eq!(std::fs::read(&file).expect("preserved"), old);
        std::fs::write(&file, vec![b'x'; MAX_BYTES + 1]).expect("oversized");
        assert!(store.load().is_err());
    }
    #[cfg(unix)]
    #[test]
    fn linked_and_special_history_never_reads_or_overwrites_target() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().expect("fixture");
        let store = InputHistoryStore::at(root.path());
        store.save(&["initial".into()]).expect("save");
        let file = root.path().join(".doge/sessions/input_history.json");
        assert_eq!(
            std::fs::metadata(&file).expect("stat").permissions().mode() & 0o777,
            0o600
        );
        let victim = root.path().join("victim");
        std::fs::write(&victim, b"USER BYTES").expect("victim");
        std::fs::remove_file(&file).expect("unlink");
        symlink(&victim, &file).expect("link");
        assert!(store.load().is_err());
        assert!(store.save(&["replacement".into()]).is_err());
        assert_eq!(std::fs::read(&victim).expect("victim"), b"USER BYTES");
        std::fs::remove_file(&file).expect("unlink");
        std::fs::hard_link(&victim, &file).expect("hardlink");
        assert!(store.save(&[]).is_err());
        std::fs::remove_file(&file).expect("unlink");
        let fifo = std::ffi::CString::new(file.as_os_str().as_encoded_bytes()).expect("path");
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(store.load().is_err());
        assert!(store.save(&[]).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn parent_links_and_readonly_destination_are_rejected() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        for component in [".doge", ".doge/sessions"] {
            let root = tempfile::tempdir().expect("fixture");
            let outside = tempfile::tempdir().expect("outside");
            std::fs::create_dir_all(root.path().join(".doge")).expect("parent");
            if component == ".doge" {
                std::fs::remove_dir(root.path().join(".doge")).expect("remove");
            }
            symlink(outside.path(), root.path().join(component)).expect("link");
            let store = InputHistoryStore::at(root.path());
            assert!(store.load().is_err());
            assert!(store.save(&[]).is_err());
            assert_eq!(
                std::fs::read_dir(outside.path()).expect("outside").count(),
                0
            );
        }
        let root = tempfile::tempdir().expect("fixture");
        let store = InputHistoryStore::at(root.path());
        store.save(&["keep".into()]).expect("save");
        let file = root.path().join(".doge/sessions/input_history.json");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o400)).expect("mode");
        assert!(store.save(&["replace".into()]).is_err());
        assert_eq!(store.load().expect("load"), ["keep"]);
    }
}
