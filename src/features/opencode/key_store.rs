//! Owner-only storage for the OpenCode API key (`dgc auth login opencode`).
//! One key serves both Go and Zen; the billing route is still chosen explicitly.
use anyhow::{Context, Result, bail};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const FILE_NAME: &str = "opencode_api_key";

/// `DOGE_CODE_CONFIG` is authoritative: the key lives next to that file so an
/// injected configuration never reads the real user key.
pub fn default_path() -> Result<PathBuf> {
    if let Some(config) = crate::config::explicit_config_path() {
        let parent = config
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        return Ok(parent.join(FILE_NAME));
    }
    if !cfg!(unix) {
        bail!(
            "the stored OpenCode key is currently supported on macOS/Linux only; use OPENCODE_API_KEY"
        );
    }
    if cfg!(test) {
        bail!("tests must set DOGE_CODE_CONFIG before touching the key store");
    }
    // Same directory as the user config so README paths hold on every unix.
    Ok(crate::config::user_config_dir()
        .context("user configuration directory unavailable (set HOME or XDG_CONFIG_HOME)")?
        .join(FILE_NAME))
}

/// Pre-XDG location (`dirs::config_dir()`, e.g. `~/Library/Application Support`
/// on macOS). Read as a fallback and cleared by `logout`, never written.
fn legacy_path() -> Option<PathBuf> {
    if cfg!(test) || !cfg!(unix) || crate::config::explicit_config_path().is_some() {
        return None;
    }
    let legacy = dirs::config_dir()?.join("doge-code").join(FILE_NAME);
    (default_path().ok()? != legacy).then_some(legacy)
}

/// Missing file is `Ok(None)`; an unsafe or unreadable file is an error so a
/// credential is never used from a path other users could have written.
pub fn load_at(path: &Path) -> Result<Option<String>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("cannot open {}", path.display())),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = file.metadata()?;
        // SAFETY: geteuid has no arguments or memory preconditions.
        if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            bail!(
                "{} must be an owner-only regular file (chmod 600)",
                path.display()
            );
        }
    }
    let mut key = String::new();
    file.take(16 * 1024).read_to_string(&mut key)?;
    let key = key.trim();
    Ok((!key.is_empty()).then(|| key.to_owned()))
}

pub fn save_at(path: &Path, key: &str) -> Result<()> {
    let key = key.trim();
    if key.is_empty() || key.chars().any(char::is_whitespace) {
        bail!("API key must be a single non-empty token");
    }
    let dir = path.parent().context("key path has no parent directory")?;
    if fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) {
        bail!("key directory must not be a symlink");
    }
    if !dir.exists() {
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
                .with_context(|| format!("cannot restrict {}", dir.display()))?;
        }
    }
    let tmp = dir.join(format!(".{FILE_NAME}.{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let result = (|| {
        let mut file = options.open(&tmp)?;
        file.write_all(key.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.with_context(|| format!("cannot write {}", path.display()))
}

/// Returns whether a key was present.
pub fn clear_at(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("cannot remove {}", path.display())),
    }
}

/// Stored key for the running process; problems are logged, not fatal.
pub fn stored_key() -> Option<String> {
    let path = default_path().ok()?;
    load_with_legacy(&path, legacy_path().as_deref())
        .inspect_err(|error| tracing::warn!(%error, "ignoring stored OpenCode API key"))
        .ok()
        .flatten()
}

/// The current location wins; the legacy one is consulted only when it holds
/// no key. An unusable file at either location is an error, never skipped.
fn load_with_legacy(path: &Path, legacy: Option<&Path>) -> Result<Option<String>> {
    match load_at(path)? {
        Some(key) => Ok(Some(key)),
        None => legacy.map_or(Ok(None), load_at),
    }
}

/// Environment first (explicit shell intent), then the stored key.
pub fn available_key() -> Option<String> {
    std::env::var("OPENCODE_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .or_else(stored_key)
}

/// Read a key from stdin; hidden when attached to a terminal.
fn read_key() -> Result<String> {
    use std::io::IsTerminal;
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        use std::io::BufRead;
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        return Ok(line);
    }
    eprint!("OpenCode API key (input hidden; from https://opencode.ai/auth): ");
    std::io::stderr().flush()?;
    crossterm::terminal::enable_raw_mode()?;
    let line = (|| -> Result<String> {
        use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, read};
        let mut line = String::new();
        loop {
            let Event::Key(key) = read()? else { continue };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            match key.code {
                KeyCode::Enter => return Ok(line),
                KeyCode::Esc => bail!("cancelled"),
                KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    bail!("cancelled")
                }
                KeyCode::Backspace => {
                    line.pop();
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    line.push(c)
                }
                _ => {}
            }
        }
    })();
    let _ = crossterm::terminal::disable_raw_mode();
    eprintln!();
    line
}

pub fn login() -> Result<()> {
    let path = default_path()?;
    let key = read_key()?;
    save_at(&path, &key)?;
    println!(
        "Saved the OpenCode API key to {} (owner-only). It is used for both Go and Zen; OPENCODE_API_KEY and --api-key still take precedence.",
        path.display()
    );
    println!(
        "Start with: dgc --provider go   (subscription)  or  dgc --provider zen   (pay-as-you-go)"
    );
    Ok(())
}

pub fn status() -> Result<()> {
    let path = default_path()?;
    let env = std::env::var("OPENCODE_API_KEY").is_ok_and(|k| !k.trim().is_empty());
    let stored = match load_with_legacy(&path, legacy_path().as_deref()) {
        Ok(key) => key.is_some(),
        Err(error) => {
            println!("Stored key is unusable and ignored: {error:#}");
            false
        }
    };
    println!(
        "OPENCODE_API_KEY environment: {}",
        if env { "set" } else { "not set" }
    );
    println!(
        "Stored key ({}): {}",
        path.display(),
        if stored { "present" } else { "none" }
    );
    println!("Key presence does not verify authentication or Go/Zen access.");
    Ok(())
}

pub fn logout() -> Result<()> {
    let removed = clear_at(&default_path()?)?;
    let removed_legacy = match legacy_path() {
        Some(legacy) => clear_at(&legacy)?,
        None => false,
    };
    if removed || removed_legacy {
        println!("Removed the stored OpenCode API key.");
    } else {
        println!("No stored OpenCode API key.");
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod legacy_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn current_location_wins_and_legacy_is_a_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let current = dir.path().join("new").join(FILE_NAME);
        let legacy = dir.path().join("old").join(FILE_NAME);

        assert_eq!(
            load_with_legacy(&current, Some(&legacy)).expect("none"),
            None
        );
        save_at(&legacy, "sk-legacy").expect("save legacy");
        assert_eq!(
            load_with_legacy(&current, Some(&legacy))
                .expect("fallback")
                .as_deref(),
            Some("sk-legacy")
        );
        assert_eq!(
            load_with_legacy(&current, None).expect("no legacy"),
            None,
            "without a legacy path nothing is read"
        );

        save_at(&current, "sk-current").expect("save current");
        assert_eq!(
            load_with_legacy(&current, Some(&legacy))
                .expect("current")
                .as_deref(),
            Some("sk-current")
        );
    }

    #[test]
    fn unsafe_legacy_file_is_an_error_not_a_silent_skip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let current = dir.path().join("new").join(FILE_NAME);
        let legacy = dir.path().join("old").join(FILE_NAME);
        save_at(&legacy, "sk-legacy").expect("save legacy");
        std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o644))
            .expect("loosen permissions");
        assert!(load_with_legacy(&current, Some(&legacy)).is_err());
    }
}
