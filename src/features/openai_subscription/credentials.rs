use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const MAX_FILE: u64 = 1024 * 1024;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    pub host_id: String,
    pub active: Option<String>,
    pub accounts: Vec<Account>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Account {
    pub label: String,
    pub subject: String,
    pub client_id: String,
    pub email: Option<String>,
    pub tokens: Option<Tokens>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Tokens {
    #[serde(default)]
    pub renewal_uncertain: bool,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: String,
    pub scopes: Vec<String>,
    pub expires_at: i64,
    pub earliest_refresh_at: Option<i64>,
}
impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("accounts", &self.accounts.len())
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("label", &self.label)
            .field("tokens", &"[REDACTED]")
            .finish()
    }
}
impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tokens([REDACTED])")
    }
}

#[derive(Clone, Debug)]
pub struct CredentialStore {
    root: PathBuf,
}
impl CredentialStore {
    pub fn default_path() -> Result<Self> {
        if !cfg!(unix) {
            bail!("ChatGPT plan credentials are currently supported on macOS/Linux only");
        }
        Ok(Self::at(
            dirs::config_dir()
                .context("user configuration directory unavailable")?
                .join("doge-code/openai"),
        ))
    }
    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }
    fn prepare(&self) -> Result<()> {
        // Refuse symlinks in the complete path, including existing ancestors.
        for path in self.root.ancestors() {
            if let Ok(meta) = fs::symlink_metadata(path)
                && meta.file_type().is_symlink()
            {
                bail!("credential directory must not contain symlinks");
            }
        }
        fs::create_dir_all(&self.root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let meta = fs::metadata(&self.root)?;
            // SAFETY: geteuid has no arguments or memory preconditions.
            if meta.uid() != unsafe { libc::geteuid() } {
                bail!("credential directory has a different owner");
            }
            fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    fn open(&self, name: &str, create: bool) -> Result<File> {
        self.prepare()?;
        let path = self.root.join(name);
        let mut options = OpenOptions::new();
        options.read(true).write(create).create(create);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let meta = file.metadata()?;
            // SAFETY: geteuid has no arguments or memory preconditions.
            if meta.uid() != unsafe { libc::geteuid() }
                || meta.mode() & 0o077 != 0
                || !meta.is_file()
            {
                bail!("credential file must be an owner-only regular file");
            }
        }
        Ok(file)
    }
    pub fn load(&self) -> Result<Registry> {
        self.prepare()?;
        if !self
            .root
            .join("accounts.json")
            .symlink_metadata()
            .map(|_| true)
            .or_else(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(false)
                } else {
                    Err(e)
                }
            })?
        {
            return Ok(Registry::default());
        }
        let mut data = String::new();
        let file = self.open("accounts.json", false)?;
        if file.metadata()?.len() > MAX_FILE {
            bail!("credential file too large");
        }
        file.take(MAX_FILE + 1).read_to_string(&mut data)?;
        if data.len() as u64 > MAX_FILE {
            bail!("credential file too large");
        }
        serde_json::from_str(&data).context("invalid credential store")
    }
    pub fn save(&self, registry: &Registry) -> Result<()> {
        self.prepare()?;
        // Validate existing target, rather than replacing an insecure file silently.
        if self.root.join("accounts.json").symlink_metadata().is_ok() {
            self.open("accounts.json", false)?;
        }
        let data = serde_json::to_vec(registry)?;
        if data.len() as u64 > MAX_FILE {
            bail!("credential file too large");
        }
        let mut tmp = tempfile::NamedTempFile::new_in(&self.root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tmp.as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        tmp.write_all(&data)?;
        tmp.as_file().sync_all()?;
        tmp.persist(self.root.join("accounts.json"))
            .map_err(|_| anyhow::anyhow!("cannot persist credential update"))?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    pub async fn lock(&self, cancel: &CancellationToken) -> Result<StoreLock> {
        let file = self.open("accounts.lock", true)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            if cancel.is_cancelled() {
                bail!(crate::llm::LlmErrorKind::Cancelled);
            }
            #[cfg(unix)]
            {
                use std::os::fd::AsRawFd;
                // SAFETY: file owns a live descriptor, retained until StoreLock drops.
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                    return Ok(StoreLock(file));
                }
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::WouldBlock {
                    return Err(error.into());
                }
            }
            #[cfg(not(unix))]
            {
                bail!("protected credential locking is unsupported on this platform");
            }
            if tokio::time::Instant::now() >= deadline {
                bail!("credential lock timed out");
            }
            tokio::select! { _ = cancel.cancelled() => bail!(crate::llm::LlmErrorKind::Cancelled), _ = tokio::time::sleep(Duration::from_millis(25)) => {} }
        }
    }
}
pub struct StoreLock(File);
impl Drop for StoreLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: the owned file descriptor remains live until after drop returns.
            unsafe {
                libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}
