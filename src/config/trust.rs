//! Guard untrusted project configuration before resolving credentials or clients.
use anyhow::{Result, ensure};
use std::path::Path;

use super::app::{merge_local_mcp_server, merge_mcp_servers};
use super::{ExecutionConfig, ExecutionMode, FileConfig, merge_execution};

/// Origin labels contain no credential values, URLs, or user paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfigOrigin {
    #[default]
    Default,
    Cli,
    Environment,
    Global,
    Project,
}

pub(crate) fn project_denied(setting: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "untrusted project configuration cannot change {setting}; configure it explicitly in the user/global config (DOGE_CODE_CONFIG), or use the corresponding CLI/environment option for inference endpoints"
    )
}

/// Conservative subset rules: ambiguous changes fail at startup, never silently
/// override a global restriction. A trust store and richer intersections are
/// intentionally outside this initial boundary.
pub(crate) fn check_project_policy(
    global: &FileConfig,
    project: &FileConfig,
    cwd: &Path,
) -> Result<()> {
    let mut trusted = ExecutionConfig::default();
    let global_execution = merge_execution(global.execution.as_ref(), None);
    if let Some(ref execution) = global_execution {
        trusted.apply_partial(execution);
    }
    let mut effective = ExecutionConfig::default();
    if let Some(execution) = merge_execution(global.execution.as_ref(), project.execution.as_ref())
    {
        effective.apply_partial(&execution);
    }
    if trusted.mode == ExecutionMode::Deny && effective.mode != ExecutionMode::Deny {
        return Err(project_denied("execution.mode (global deny)"));
    }
    if !trusted.allow_shell && effective.allow_shell {
        return Err(project_denied("execution.allow_shell"));
    }
    if trusted.mode == ExecutionMode::Allowlist && effective.mode != ExecutionMode::Deny {
        ensure!(
            effective.mode == ExecutionMode::Allowlist,
            project_denied("execution.mode")
        );
        ensure!(
            effective
                .allowed_programs
                .iter()
                .all(|p| trusted.allowed_programs.contains(p)),
            project_denied("execution.allowed_programs")
        );
    }
    // Environment overrides are a separate capability even in unrestricted mode.
    ensure!(
        effective
            .allowed_env
            .iter()
            .all(|p| trusted.allowed_env.contains(p)),
        project_denied("execution.allowed_env")
    );
    if global_execution.is_none()
        && let Some(legacy) = global
            .allowed_commands
            .as_ref()
            .filter(|rules| !rules.is_empty())
    {
        if merge_execution(None, project.execution.as_ref()).is_some()
            && effective.mode != ExecutionMode::Deny
        {
            return Err(project_denied("execution (global legacy allowed_commands)"));
        }
        if let Some(rules) = &project.allowed_commands {
            ensure!(
                !rules.is_empty() && rules.iter().all(|r| legacy.contains(r)),
                project_denied("allowed_commands")
            );
        }
    }
    if let Some(paths) = &project.allowed_paths {
        let trusted_paths = global.allowed_paths.as_deref().unwrap_or_default();
        ensure!(
            paths.iter().all(|p| trusted_paths.contains(p)),
            project_denied("allowed_paths")
        );
    }
    if let Some(root) = &project.project_root {
        let trusted_root = global.project_root.as_deref().unwrap_or(cwd);
        let resolve = |path: &Path| {
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                cwd.join(path)
            }
        };
        let trusted_root = std::fs::canonicalize(resolve(trusted_root))?;
        let project_root = std::fs::canonicalize(resolve(root))?;
        ensure!(project_root == trusted_root, project_denied("project_root"));
    }
    let trusted_servers = merge_mcp_servers(global.mcp_servers.as_ref(), None);
    for server in merge_mcp_servers(global.mcp_servers.as_ref(), project.mcp_servers.as_ref())
        .iter()
        .filter(|s| s.enabled)
    {
        let known = trusted_servers
            .iter()
            .find(|s| s.name == server.name && s.enabled);
        ensure!(
            known.is_some_and(|s| s.transport == server.transport
                && s.address == server.address
                && s.command == server.command
                && s.args == server.args
                && s.env == server.env),
            project_denied("mcp_servers")
        );
    }
    let trusted_listener = merge_local_mcp_server(global.mcp_server.as_ref(), None);
    let listener = merge_local_mcp_server(global.mcp_server.as_ref(), project.mcp_server.as_ref());
    ensure!(
        !listener.enabled
            || (trusted_listener.enabled && listener.address == trusted_listener.address),
        project_denied("mcp_server")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(text: &str) -> FileConfig {
        toml::from_str(text).expect("synthetic configuration")
    }
    #[test]
    fn project_trust_policy_prevents_broadening_and_allows_narrowing() {
        let root = tempfile::tempdir().expect("root");
        for (global, project, allowed) in [
            (
                "[execution]\nmode='deny'",
                "[execution]\nmode='unrestricted'",
                false,
            ),
            ("[execution]\nmode='deny'", "[execution]\nmode='deny'", true),
            (
                "[execution]\nmode='allowlist'\nallowed_programs=['git']",
                "[execution]\nmode='unrestricted'",
                false,
            ),
            (
                "[execution]\nmode='allowlist'\nallowed_programs=['git']",
                "[execution]\nallowed_programs=['git','sh']",
                false,
            ),
            (
                "[execution]\nmode='allowlist'\nallowed_programs=['git','cargo']",
                "[execution]\nallowed_programs=['git']",
                true,
            ),
            (
                "[execution]\nallow_shell=false",
                "[execution]\nallow_shell=true",
                false,
            ),
            (
                "[execution]\nallowed_env=['RUST_LOG']",
                "[execution]\nallowed_env=['PATH']",
                false,
            ),
            (
                "[execution]\nallowed_env=['RUST_LOG']",
                "[execution]\nallowed_env=[]",
                true,
            ),
            (
                "allowed_commands=['git status']",
                "allowed_commands=[]",
                false,
            ),
            (
                "allowed_commands=['git status']",
                "allowed_commands=['git']",
                false,
            ),
            (
                "allowed_commands=['git status','cargo test']",
                "allowed_commands=['git status']",
                true,
            ),
            (
                "allowed_commands=['git status']",
                "[execution]\nmode='allowlist'\nallowed_programs=['git']",
                false,
            ),
            (
                "allowed_commands=['git status']",
                "[execution]\nmode='deny'",
                true,
            ),
            ("allowed_paths=['/tmp']", "allowed_paths=['/']", false),
            ("allowed_paths=['/tmp']", "allowed_paths=[]", true),
            ("", "allowed_paths=['/tmp']", false),
            (
                "[[mcp_servers]]\nname='blocked'\nenabled=false",
                "[[mcp_servers]]\nname='blocked'\nenabled=true\naddress='http://127.0.0.1:1'",
                false,
            ),
            (
                "[[mcp_servers]]\nname='known'\nenabled=true\naddress='http://127.0.0.1:1'",
                "[[mcp_servers]]\nname='known'\naddress='http://127.0.0.1:2'",
                false,
            ),
            (
                "[[mcp_servers]]\nname='known'\nenabled=true\ncommand='fixture'\nargs=['safe']",
                "[[mcp_servers]]\nname='known'\nargs=['unsafe']",
                false,
            ),
            (
                "[[mcp_servers]]\nname='known'\nenabled=true\ncommand='fixture'",
                "[[mcp_servers]]\nname='known'\n[ mcp_servers.env ]\nTOKEN='synthetic'",
                false,
            ),
            (
                "[[mcp_servers]]\nname='known'\nenabled=true\naddress='http://127.0.0.1:1'",
                "[[mcp_servers]]\nname='known'\nenabled=false",
                true,
            ),
            (
                "[mcp_server]\nenabled=false",
                "[mcp_server]\nenabled=true",
                false,
            ),
            (
                "[mcp_server]\nenabled=true\naddress='127.0.0.1:8000'",
                "[mcp_server]\naddress='0.0.0.0:8000'",
                false,
            ),
            (
                "[mcp_server]\nenabled=true",
                "[mcp_server]\nenabled=false",
                true,
            ),
        ] {
            let result = check_project_policy(&config(global), &config(project), root.path());
            assert_eq!(
                result.is_ok(),
                allowed,
                "global={global} project={project}: {result:?}"
            );
        }
    }
    #[test]
    fn project_trust_root_cannot_escape_by_symlink() {
        let root = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::create_dir(root.path().join("sub")).expect("sub");
        assert!(
            check_project_policy(
                &FileConfig::default(),
                &config("project_root='sub'"),
                root.path()
            )
            .is_err()
        );
        let project = FileConfig {
            project_root: Some(outside.path().into()),
            ..Default::default()
        };
        assert!(check_project_policy(&FileConfig::default(), &project, root.path()).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).expect("link");
            assert!(
                check_project_policy(
                    &FileConfig::default(),
                    &config("project_root='escape'"),
                    root.path()
                )
                .is_err()
            );
        }
    }
}
