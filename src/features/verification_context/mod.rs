//! Frozen, minimal metadata collected before a verification starts.
//! No arbitrary output, environment values or executable paths are durable.
use crate::config::AppConfig;
use crate::execution::{
    ExecutionPolicy, ManagedProcessSpec, ManagedProcessTermination, ManagedRunOptions,
    ProcessRequest,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionContext {
    #[serde(deserialize_with = "read_format_version")]
    pub format_version: u32,
    pub os_family: OsFamily,
    pub architecture: Architecture,
    pub tool: Option<Tool>,
    pub version: VersionObservation,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OsFamily {
    Linux,
    Macos,
    Windows,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    X86_64,
    Aarch64,
    X86,
    Arm,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    Cargo,
    Go,
    Python3,
    Node,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum VersionObservation {
    Observed { major: u32, minor: u32, patch: u32 },
    Unsupported,
    UnsafeExecutable,
    PolicyDenied,
    Unavailable,
    TimedOut,
    Cancelled,
    InvalidOutput,
}
fn read_format_version<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let version = u32::deserialize(deserializer)?;
    if version == 1 {
        Ok(version)
    } else {
        Err(serde::de::Error::custom(
            "unsupported execution context format",
        ))
    }
}

impl ExecutionContext {
    fn new(program: &str) -> Self {
        let tool = match Path::new(program).file_name().and_then(|v| v.to_str()) {
            Some("cargo") => Some(Tool::Cargo),
            Some("go") => Some(Tool::Go),
            Some("python3") => Some(Tool::Python3),
            Some("node") => Some(Tool::Node),
            _ => None,
        };
        Self {
            format_version: 1,
            os_family: match std::env::consts::OS {
                "linux" => OsFamily::Linux,
                "macos" => OsFamily::Macos,
                "windows" => OsFamily::Windows,
                _ => OsFamily::Unknown,
            },
            architecture: match std::env::consts::ARCH {
                "x86_64" => Architecture::X86_64,
                "aarch64" => Architecture::Aarch64,
                "x86" => Architecture::X86,
                "arm" => Architecture::Arm,
                _ => Architecture::Unknown,
            },
            tool,
            version: VersionObservation::Unsupported,
        }
    }
}

/// LLM requests and the extra probe must independently pass the existing policy.
pub async fn capture_for_request(
    config: Arc<AppConfig>,
    request: &ProcessRequest,
    cancel: Option<CancellationToken>,
) -> ExecutionContext {
    let policy = ExecutionPolicy::new(config.clone());
    let mut context = ExecutionContext::new(&request.program);
    if policy.check_process(request).is_err() {
        context.version = VersionObservation::PolicyDenied;
        return context;
    }
    capture(
        context,
        &request.program,
        &policy.resolve_cwd(&request.cwd),
        &config.project_root,
        &request.env,
        Some(&policy),
        cancel,
    )
    .await
}
/// Trusted /test and /lint keep their existing independent execution boundary.
pub async fn capture_trusted(
    root: &Path,
    program: &str,
    cancel: Option<CancellationToken>,
) -> ExecutionContext {
    capture(
        ExecutionContext::new(program),
        program,
        root,
        root,
        &BTreeMap::new(),
        None,
        cancel,
    )
    .await
}
#[allow(clippy::too_many_arguments)] // explicit transient inputs; none are persisted
async fn capture(
    mut context: ExecutionContext,
    program: &str,
    cwd: &Path,
    root: &Path,
    overrides: &BTreeMap<String, String>,
    policy: Option<&ExecutionPolicy>,
    cancel: Option<CancellationToken>,
) -> ExecutionContext {
    let Some(tool) = context.tool else {
        return context;
    };
    // Go can delegate via go.mod/go.work and inherited GOTOOLCHAIN. A local
    // driver probe cannot identify that selected verification toolchain.
    if tool == Tool::Go {
        return context;
    }
    if cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
        context.version = VersionObservation::Cancelled;
        return context;
    }
    if !overrides.is_empty() {
        context.version = VersionObservation::UnsafeExecutable;
        return context;
    }
    let Some(executable) = resolve_executable(program, cwd) else {
        context.version = VersionObservation::Unavailable;
        return context;
    };
    if !trusted_executable(&executable, root, tool) {
        context.version = VersionObservation::UnsafeExecutable;
        return context;
    }
    let Ok(directory) = tempfile::tempdir() else {
        context.version = VersionObservation::Unavailable;
        return context;
    };
    let args: Vec<String> = match tool {
        Tool::Cargo | Tool::Node => vec!["--version".into()],
        Tool::Go => vec!["version".into()],
        Tool::Python3 => vec!["-I".into(), "-S".into(), "--version".into()],
    };
    let mut env = BTreeMap::new();
    if tool == Tool::Go {
        env.insert("GOTOOLCHAIN".into(), "local".into());
    }
    let probe = ProcessRequest {
        program: executable.to_string_lossy().into_owned(),
        args: args.clone(),
        cwd: Some(directory.path().into()),
        env: env.clone(),
        timeout_ms: Some(500),
    };
    if policy.is_some_and(|p| p.check_process(&probe).is_err()) {
        context.version = VersionObservation::PolicyDenied;
        return context;
    }
    let result = crate::execution::runner::run_managed_probe(
        ManagedProcessSpec::new(probe.program, args, directory.path().into()).with_env(env),
        ManagedRunOptions::new(policy.map_or(Some(Duration::from_millis(500)), |p| {
            p.effective_timeout(Some(500))
        }))
        .with_cancellation(cancel),
        1024,
    )
    .await;
    context.version = match result {
        Err(_) => VersionObservation::Unavailable,
        Ok(output) => match output.termination {
            ManagedProcessTermination::Cancelled => VersionObservation::Cancelled,
            ManagedProcessTermination::TimedOut => VersionObservation::TimedOut,
            ManagedProcessTermination::Exited
                if output.success()
                    && !output.capture_truncated
                    && output.stderr.is_empty()
                    && output.warnings.is_empty() =>
            {
                parse_version(tool, &output.stdout).unwrap_or(VersionObservation::InvalidOutput)
            }
            _ => VersionObservation::InvalidOutput,
        },
    };
    context
}
fn resolve_executable(program: &str, cwd: &Path) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return std::fs::canonicalize(if path.is_absolute() {
            path.to_owned()
        } else {
            cwd.join(path)
        })
        .ok();
    }
    let search = std::env::var_os("PATH")?;
    // Inspect the first executable the actual command would select. Never skip an
    // unsafe shim and substitute a different installed tool.
    std::env::split_paths(&search)
        .map(|p| {
            if p.is_absolute() {
                p.join(path)
            } else {
                cwd.join(p).join(path)
            }
        })
        .find(|p| is_executable(p))
        .and_then(|p| std::fs::canonicalize(p).ok())
}
fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}
fn trusted_executable(path: &Path, root: &Path, tool: Tool) -> bool {
    #[cfg(unix)]
    {
        use std::{io::Read, os::unix::fs::MetadataExt};
        if !path.starts_with("/usr/bin")
            || std::fs::canonicalize(root).is_ok_and(|r| path.starts_with(r))
        {
            return false;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let correct = match tool {
            Tool::Cargo => name == "cargo",
            Tool::Go => name == "go",
            Tool::Node => name == "node",
            Tool::Python3 => {
                name == "python3"
                    || name
                        .strip_prefix("python3.")
                        .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
            }
        };
        if !correct
            || path
                .ancestors()
                .any(|p| !std::fs::metadata(p).is_ok_and(|m| m.uid() == 0 && m.mode() & 0o022 == 0))
        {
            return false;
        }
        let mut magic = [0u8; 4];
        std::fs::File::open(path)
            .and_then(|mut f| f.read_exact(&mut magic))
            .is_ok()
            && magic == *b"\x7fELF"
    }
    #[cfg(not(unix))]
    {
        let _ = (path, root, tool);
        false
    }
}
fn parse_version(tool: Tool, output: &str) -> Option<VersionObservation> {
    if output.len() > 128 || output.trim().contains(['\n', '\r']) {
        return None;
    }
    let text = output.trim();
    let numeric = match tool {
        Tool::Python3 => text.strip_prefix("Python ")?,
        Tool::Node => text.strip_prefix('v')?,
        Tool::Go => text.strip_prefix("go version go")?.split_once(' ')?.0,
        Tool::Cargo => text.strip_prefix("cargo ")?.split(' ').next()?,
    };
    // Only a numeric identity is persisted. Reject injected text even where a
    // recognized tool normally appends build metadata/platform identifiers.
    if tool == Tool::Go {
        let suffix = text.strip_prefix("go version go")?.split_once(' ')?.1;
        if !matches!(
            suffix,
            "linux/amd64" | "linux/arm64" | "darwin/amd64" | "darwin/arm64" | "windows/amd64"
        ) {
            return None;
        }
    }
    if tool == Tool::Cargo && text != format!("cargo {numeric}") {
        let suffix = text
            .strip_prefix(&format!("cargo {numeric} ("))?
            .strip_suffix(')')?;
        let (hash, date) = suffix.split_once(' ')?;
        if !(7..=40).contains(&hash.len())
            || !hash.bytes().all(|b| b.is_ascii_hexdigit())
            || date.len() != 10
            || !date.bytes().enumerate().all(|(i, b)| {
                if i == 4 || i == 7 {
                    b == b'-'
                } else {
                    b.is_ascii_digit()
                }
            })
        {
            return None;
        }
    }
    let parts: Vec<&str> = numeric.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|v| v.is_empty() || v.len() > 6 || !v.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    Some(VersionObservation::Observed {
        major: parts[0].parse().ok()?,
        minor: parts[1].parse().ok()?,
        patch: parts[2].parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_nested_format_is_rejected() {
        let mut value = serde_json::to_value(ExecutionContext::new("cargo")).expect("context");
        value["format_version"] = serde_json::json!(2);
        assert!(serde_json::from_value::<ExecutionContext>(value).is_err());
    }
    #[tokio::test]
    async fn original_permission_does_not_authorize_extra_probe() {
        // This invokes no probe: only the original program/cwd is allowed.
        let root = tempfile::tempdir().expect("root");
        let mut config = AppConfig {
            project_root: root.path().into(),
            execution_configured: true,
            ..Default::default()
        };
        config.execution.mode = crate::config::ExecutionMode::Allowlist;
        config.execution.allowed_programs = vec!["cargo".into()];
        config.allowed_paths.clear();
        let policy = ExecutionPolicy::new(Arc::new(config));
        let original = ProcessRequest {
            program: "cargo".into(),
            args: vec!["test".into()],
            cwd: None,
            env: BTreeMap::new(),
            timeout_ms: None,
        };
        assert!(policy.check_process(&original).is_ok());
        let extra = ProcessRequest {
            program: "/usr/bin/cargo".into(),
            args: vec!["--version".into()],
            cwd: Some(root.path().into()),
            env: BTreeMap::new(),
            timeout_ms: Some(500),
        };
        assert!(policy.check_process(&extra).is_err());
        let temp = tempfile::tempdir().expect("probe cwd");
        let extra = ProcessRequest {
            program: "cargo".into(),
            cwd: Some(temp.path().into()),
            ..original
        };
        assert!(policy.check_process(&extra).is_err());
    }
    #[test]
    fn version_output_is_typed_and_rejects_private_or_multiline_data() {
        for (tool, text) in [
            (Tool::Cargo, "cargo 1.99.0 (abc1234 2026-10-01)"),
            (Tool::Python3, "Python 3.13.3"),
            (Tool::Node, "v22.3.0"),
            (Tool::Go, "go version go1.24.0 linux/amd64"),
        ] {
            assert!(matches!(
                parse_version(tool, text),
                Some(VersionObservation::Observed { .. })
            ));
        }
        for text in [
            "Python 3.13.3 /home/secret",
            "Python 3.13.3\nTOKEN=secret",
            "Python +3.13.3",
            "Python 3.13.3-rc1",
        ] {
            assert!(parse_version(Tool::Python3, text).is_none());
        }
        assert!(parse_version(Tool::Cargo, "cargo 1.99.0 (/home/me secret)").is_none());
    }
    #[tokio::test]
    async fn deny_policy_and_cancel_never_spawn() {
        let root = tempfile::tempdir().expect("root");
        let mut config = AppConfig {
            project_root: root.path().into(),
            ..Default::default()
        };
        config.execution.mode = crate::config::ExecutionMode::Deny;
        let request = ProcessRequest {
            program: "python3".into(),
            args: vec![],
            cwd: None,
            env: BTreeMap::new(),
            timeout_ms: None,
        };
        assert_eq!(
            capture_for_request(Arc::new(config), &request, None)
                .await
                .version,
            VersionObservation::PolicyDenied
        );
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            capture_trusted(root.path(), "python3", Some(cancel))
                .await
                .version,
            VersionObservation::Cancelled
        );
    }
    #[tokio::test]
    async fn overrides_and_project_executables_remain_unknown() {
        let root = tempfile::tempdir().expect("root");
        let path = root.path().join("python3");
        std::fs::write(&path, b"\x7fELF").expect("fixture");
        assert!(!trusted_executable(&path, root.path(), Tool::Python3));
        let env = BTreeMap::from([("TOKEN".into(), "private-value".into())]);
        let context = capture(
            ExecutionContext::new("python3"),
            "python3",
            root.path(),
            root.path(),
            &env,
            None,
            None,
        )
        .await;
        let json = serde_json::to_string(&context).expect("json");
        assert_eq!(context.version, VersionObservation::UnsafeExecutable);
        assert!(!json.contains("private-value"));
        assert!(!json.contains("TOKEN"));
        assert!(!json.contains("/home"));
    }
}
