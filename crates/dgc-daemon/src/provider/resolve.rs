//! Locating the provider CLIs at daemon start.
//!
//! Each provider's executable is the configured path (`claude_path` /
//! `codex_path`, which must be absolute) or else the first executable named
//! `claude` / `codex` in an absolute directory of the daemon's `PATH`
//! (relative `PATH` entries are skipped). The found file is run once with
//! `--version` (bounded by [`VERSION_TIMEOUT`]); a version other than the one
//! dgc was tested against is reported with `tested = false` and a warning.
//! A provider that is not found has no path, and its agents cannot be created.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use dgc_protocol::methods::ProviderStatus;
use dgc_protocol::model::ProviderKind;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{info, warn};

use crate::config::DaemonConfig;

/// Claude Code version the adapter was recorded and tested against.
pub const CLAUDE_TESTED_VERSION: &str = "2.1.332";
/// codex-cli version the adapter was recorded and tested against.
pub const CODEX_TESTED_VERSION: &str = "0.172.0";
/// Bound on `<provider> --version`. The daemon probes before it accepts
/// connections (the socket is already bound), so this plus the rest of
/// startup must stay well below the client's `SPAWN_WAIT` and
/// `HELLO_TIMEOUT` (15 s each, in `dgc-client`).
pub const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
/// Most `--version` output read.
const VERSION_OUTPUT_LIMIT: usize = 4096;

pub(crate) fn executable_name(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Claude => "claude",
        ProviderKind::Codex => "codex",
    }
}

fn tested_version(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Claude => CLAUDE_TESTED_VERSION,
        ProviderKind::Codex => CODEX_TESTED_VERSION,
    }
}

fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// First executable `name` in the absolute directories of `path_env`.
pub fn find_in_path(name: &str, path_env: Option<&OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path_env?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

/// The version in `--version` output: the first whitespace-separated token
/// that starts with a digit and contains a dot (`2.1.332 (Claude Code)`,
/// `codex-cli 0.172.0`).
pub fn parse_version(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|token| token.starts_with(|c: char| c.is_ascii_digit()) && token.contains('.'))
        .map(str::to_owned)
}

/// Run `program --version`; `None` when it fails, times out or prints no
/// version.
async fn probe_version(program: &Path) -> Option<String> {
    let run = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    match timeout(VERSION_TIMEOUT, run).await {
        Ok(Ok(output)) if output.status.success() => {
            let end = output.stdout.len().min(VERSION_OUTPUT_LIMIT);
            parse_version(&String::from_utf8_lossy(&output.stdout[..end]))
        }
        Ok(Ok(output)) => {
            warn!(status = %output.status, "provider --version failed");
            None
        }
        Ok(Err(err)) => {
            warn!(%err, "could not run provider --version");
            None
        }
        Err(_) => {
            warn!("provider --version timed out");
            None
        }
    }
}

/// Resolve one provider from its configured path or `path_env`.
pub async fn resolve_provider(
    kind: ProviderKind,
    configured: Option<&Path>,
    path_env: Option<&OsStr>,
) -> ProviderStatus {
    let path = match configured {
        Some(path) if !path.is_absolute() => {
            warn!(
                ?kind,
                "configured provider path is not absolute; ignoring the provider"
            );
            None
        }
        Some(path) if !is_executable_file(path) => {
            warn!(?kind, path = %path.display(), "configured provider path is not an executable file");
            None
        }
        Some(path) => Some(path.to_owned()),
        None => find_in_path(executable_name(kind), path_env),
    };
    let Some(path) = path else {
        info!(?kind, "provider not found; its agents cannot be created");
        return ProviderStatus {
            kind,
            path: None,
            version: None,
            tested: false,
        };
    };
    let version = probe_version(&path).await;
    let tested = version.as_deref() == Some(tested_version(kind));
    if tested {
        info!(?kind, path = %path.display(), version = ?version, "provider found");
    } else {
        warn!(
            ?kind,
            path = %path.display(),
            version = ?version,
            tested_version = tested_version(kind),
            "provider version is not the tested one"
        );
    }
    ProviderStatus {
        kind,
        path: Some(path),
        version,
        tested,
    }
}

/// Resolve Claude and Codex (in that order) concurrently.
pub async fn resolve_providers(
    config: &DaemonConfig,
    path_env: Option<&OsStr>,
) -> Vec<ProviderStatus> {
    let (claude, codex) = tokio::join!(
        resolve_provider(
            ProviderKind::Claude,
            config.claude_path.as_deref(),
            path_env
        ),
        resolve_provider(ProviderKind::Codex, config.codex_path.as_deref(), path_env),
    );
    vec![claude, codex]
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use tempfile::tempdir;

    use super::*;

    fn script(dir: &Path, name: &str, body: &str, mode: u32) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
        path
    }

    #[test]
    fn parses_both_version_formats() {
        assert_eq!(
            parse_version("2.1.332 (Claude Code)\n").as_deref(),
            Some("2.1.332")
        );
        assert_eq!(
            parse_version("codex-cli 0.172.0\n").as_deref(),
            Some("0.172.0")
        );
        assert_eq!(parse_version("no version here"), None);
    }

    #[test]
    fn path_search_skips_relative_dirs_and_non_executables() {
        let tmp = tempdir().expect("tempdir");
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir_all(&a).expect("mkdir");
        std::fs::create_dir_all(&b).expect("mkdir");
        script(&a, "claude", "true", 0o600);
        let found = script(&b, "claude", "true", 0o700);
        let path_env = std::env::join_paths([PathBuf::from("rel"), a, b]).expect("join");
        assert_eq!(find_in_path("claude", Some(&path_env)), Some(found));
        assert_eq!(find_in_path("codex", Some(&path_env)), None);
        assert_eq!(find_in_path("claude", None), None);
    }

    #[tokio::test]
    async fn configured_path_wins_and_version_decides_tested() {
        let tmp = tempdir().expect("tempdir");
        let tested = script(tmp.path(), "c1", "echo '2.1.332 (Claude Code)'", 0o700);
        let status = resolve_provider(ProviderKind::Claude, Some(&tested), None).await;
        assert_eq!(status.path.as_deref(), Some(tested.as_path()));
        assert_eq!(status.version.as_deref(), Some("2.1.332"));
        assert!(status.tested);

        let other = script(tmp.path(), "c2", "echo 'codex-cli 0.171.0'", 0o700);
        let status = resolve_provider(ProviderKind::Codex, Some(&other), None).await;
        assert_eq!(status.version.as_deref(), Some("0.171.0"));
        assert!(!status.tested);
        assert!(
            status.path.is_some(),
            "an untested provider is still usable"
        );

        let broken = script(tmp.path(), "c3", "exit 1", 0o700);
        let status = resolve_provider(ProviderKind::Codex, Some(&broken), None).await;
        assert!(status.path.is_some() && status.version.is_none() && !status.tested);
    }

    #[tokio::test]
    async fn relative_or_missing_configured_path_means_unavailable() {
        let tmp = tempdir().expect("tempdir");
        // A PATH hit must not be used when a path is configured.
        script(tmp.path(), "claude", "echo '2.1.332 (Claude Code)'", 0o700);
        let path_env = OsString::from(tmp.path());
        for configured in [PathBuf::from("claude"), tmp.path().join("missing")] {
            let status =
                resolve_provider(ProviderKind::Claude, Some(&configured), Some(&path_env)).await;
            assert_eq!(status.path, None, "{}", configured.display());
            assert!(!status.tested);
        }
        let status = resolve_provider(ProviderKind::Claude, None, Some(&path_env)).await;
        assert_eq!(status.path, Some(tmp.path().join("claude")));
    }
}
