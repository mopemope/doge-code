use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::path::Path;

const MAX_ENTRIES: usize = 4096;

#[cfg(unix)]
use std::{
    ffi::{CStr, CString},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
};

#[cfg(unix)]
pub(super) struct LogDirectory(File);

#[cfg(unix)]
pub(super) fn validate(file: &File) -> Result<()> {
    let meta = file.metadata()?;
    // SAFETY: geteuid has no arguments or memory ownership.
    anyhow::ensure!(
        meta.is_file() && meta.nlink() == 1 && meta.uid() == unsafe { libc::geteuid() },
        "refusing nonregular, multiply linked, or foreign-owned log file"
    );
    Ok(())
}

#[cfg(unix)]
fn clear_errno() -> Result<()> {
    // SAFETY: libc exposes a thread-local errno slot for each ABI family.
    #[cfg(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "hurd",
        target_os = "redox",
        target_os = "emscripten"
    ))]
    {
        unsafe {
            *libc::__errno_location() = 0;
        }
        Ok(())
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "watchos",
        target_os = "visionos",
        target_os = "freebsd"
    ))]
    {
        unsafe {
            *libc::__error() = 0;
        }
        Ok(())
    }
    #[cfg(any(
        target_os = "android",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "cygwin",
        target_os = "nuttx"
    ))]
    {
        unsafe {
            *libc::__errno() = 0;
        }
        Ok(())
    }
    #[cfg(any(target_os = "solaris", target_os = "illumos"))]
    {
        unsafe {
            *libc::___errno() = 0;
        }
        Ok(())
    }
    #[cfg(target_os = "haiku")]
    {
        unsafe {
            *libc::_errnop() = 0;
        }
        Ok(())
    }
    #[cfg(target_os = "aix")]
    {
        unsafe {
            *libc::_Errno() = 0;
        }
        Ok(())
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "hurd",
        target_os = "redox",
        target_os = "emscripten",
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "watchos",
        target_os = "visionos",
        target_os = "freebsd",
        target_os = "android",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "cygwin",
        target_os = "nuttx",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "haiku",
        target_os = "aix"
    )))]
    {
        anyhow::bail!("safe log enumeration is unsupported on this Unix target")
    }
}

#[cfg(unix)]
impl LogDirectory {
    pub(super) fn open(root: &Path) -> Result<Self> {
        // The selected project root may be an intentional alias.
        let root = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(root)?;
        let doge = Self::child(&root, c".doge")?;
        let logs = Self::child(&doge, c"logs")?;
        logs.set_permissions(std::fs::Permissions::from_mode(0o700))?;
        Ok(Self(logs))
    }

    pub(super) fn child(parent: &File, name: &CStr) -> Result<File> {
        // SAFETY: valid held directory descriptor and NUL-terminated component.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error.into());
            }
        }
        // SAFETY: openat returns a fresh fd, transferred once below.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        anyhow::ensure!(
            fd >= 0,
            "open log directory without following symlinks: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: successful openat transferred one owned descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        // SAFETY: geteuid has no arguments or memory ownership.
        anyhow::ensure!(
            file.metadata()?.uid() == unsafe { libc::geteuid() },
            "foreign-owned log directory"
        );
        Ok(file)
    }

    fn open_flags(&self, name: &str, flags: libc::c_int) -> Result<File> {
        let name = CString::new(name)?;
        // SAFETY: held fd, single component from enumeration or fixed/generated name.
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("open log without following symlinks");
        }
        // SAFETY: fresh owned descriptor from successful openat.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub(super) fn open_file(&self, name: &str, exclusive: bool) -> Result<File> {
        self.open_flags(
            name,
            libc::O_RDWR | libc::O_CREAT | if exclusive { libc::O_EXCL } else { 0 },
        )
    }

    pub(super) fn open_existing(&self, name: &str) -> Result<File> {
        self.open_flags(name, libc::O_RDONLY)
    }

    pub(super) fn names(&self) -> Result<Vec<String>> {
        // A new open-file description avoids shared readdir offsets.
        let scan = self.open_flags(".", libc::O_RDONLY | libc::O_DIRECTORY)?;
        use std::os::fd::IntoRawFd;
        let fd = scan.into_raw_fd();
        // SAFETY: fd ownership passes to fdopendir on success.
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            // SAFETY: fdopendir failed, so caller still owns fd.
            unsafe {
                libc::close(fd);
            }
            return Err(std::io::Error::last_os_error()).context("scan log directory");
        }
        struct Entries(*mut libc::DIR);
        impl Drop for Entries {
            fn drop(&mut self) {
                unsafe {
                    libc::closedir(self.0);
                }
            }
        }
        let entries = Entries(stream);
        let mut names = Vec::new();
        let mut scanned = 0;
        loop {
            clear_errno()?;
            // SAFETY: stream is owned and no concurrent operations use it.
            let entry = unsafe { libc::readdir(entries.0) };
            if entry.is_null() {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(0) {
                    return Err(error).context("read log directory entry");
                }
                break;
            }
            scanned += 1;
            anyhow::ensure!(
                scanned <= MAX_ENTRIES,
                "too many entries in diagnostic log directory"
            );
            // SAFETY: readdir's name is NUL-terminated until next readdir.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if let Ok(name) = name.to_str() {
                names.push(name.to_owned());
            }
        }
        Ok(names)
    }

    pub(super) fn remove_if_same(&self, name: &str, held: &File) -> Result<()> {
        validate(held)?;
        let current = self.open_existing(name)?;
        validate(&current)?;
        let a = held.metadata()?;
        let b = current.metadata()?;
        anyhow::ensure!(
            (a.dev(), a.ino()) == (b.dev(), b.ino()),
            "run log changed during retention"
        );
        let name = CString::new(name)?;
        // SAFETY: held directory fd and checked single component. Cooperating
        // dgc processes hold the retention lock; hostile inode swaps after the
        // identity check are outside this filesystem guarantee.
        if unsafe { libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), 0) } < 0 {
            return Err(std::io::Error::last_os_error()).context("remove inactive run log");
        }
        Ok(())
    }
}

// Other platforms retain preflight checks; they cannot offer the Unix dirfd
// race guarantee. Refuse automatic deletion without stable inode identity.
#[cfg(not(unix))]
pub(super) struct LogDirectory(std::path::PathBuf);
#[cfg(not(unix))]
pub(super) fn validate(file: &File) -> Result<()> {
    anyhow::ensure!(file.metadata()?.is_file(), "nonregular log file");
    Ok(())
}
#[cfg(not(unix))]
impl LogDirectory {
    pub(super) fn open(root: &Path) -> Result<Self> {
        let mut path = root.to_owned();
        for part in [".doge", "logs"] {
            path.push(part);
            match std::fs::symlink_metadata(&path) {
                Ok(meta) => anyhow::ensure!(
                    meta.is_dir() && !meta.file_type().is_symlink(),
                    "unsafe log directory"
                ),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(&path)?,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(Self(path))
    }
    pub(super) fn open_existing(&self, name: &str) -> Result<File> {
        let path = self.0.join(name);
        let meta = std::fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "unsafe log file"
        );
        Ok(OpenOptions::new().read(true).write(true).open(path)?)
    }
    pub(super) fn open_file(&self, name: &str, exclusive: bool) -> Result<File> {
        if !exclusive && self.0.join(name).exists() {
            return self.open_existing(name);
        }
        Ok(OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(self.0.join(name))?)
    }
    pub(super) fn names(&self) -> Result<Vec<String>> {
        let entries: Vec<_> = std::fs::read_dir(&self.0)?
            .take(MAX_ENTRIES + 1)
            .collect::<std::io::Result<_>>()?;
        anyhow::ensure!(
            entries.len() <= MAX_ENTRIES,
            "too many diagnostic directory entries"
        );
        Ok(entries
            .into_iter()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect())
    }
    pub(super) fn remove_if_same(&self, _: &str, _: &File) -> Result<()> {
        anyhow::bail!(
            "automatic run log retention requires Unix inode identity; archive inactive run logs manually"
        )
    }
}
