use crate::config::{AppConfig, ExecutionConfig, ExecutionMode};
use crate::execution::policy::{ExecutionPolicy, ProcessRequest};
use std::{collections::BTreeMap, sync::Arc};

fn request(program: &str, args: &[&str]) -> ProcessRequest {
    ProcessRequest {
        program: program.into(),
        args: args.iter().map(|v| (*v).into()).collect(),
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: None,
    }
}
fn policy(rules: &[&str]) -> ExecutionPolicy {
    ExecutionPolicy::new(Arc::new(AppConfig {
        project_root: std::env::current_dir().unwrap(),
        allowed_commands: rules.iter().map(|s| (*s).into()).collect(),
        ..Default::default()
    }))
}
#[test]
fn status_prefix() {
    let p = policy(&["git status"]);
    for args in [&["status"][..], &["status", "--short"][..]] {
        assert!(p.check_process(&request("git", args)).is_ok());
    }
    assert!(p.check_legacy_shell_command("git status --short").is_ok());
    assert!(p.check_process(&request("git", &["commit"])).is_err());
    assert!(p.check_process(&request("git", &[])).is_err());
    assert!(p.check_legacy_shell_command("git commit").is_err());
}
#[test]
fn argumentless_rule() {
    let p = policy(&["git"]);
    assert!(p.check_process(&request("git", &[])).is_ok());
    assert!(p.check_process(&request("git", &["commit"])).is_ok());
    assert!(p.check_process(&request("other", &[])).is_err());
}
#[test]
fn quoted_and_empty_literals() {
    let p = policy(&["echo 'hello world'", "printf ''"]);
    assert!(
        p.check_process(&request("echo", &["hello world", "tail"]))
            .is_ok()
    );
    assert!(
        p.check_process(&request("echo", &["hello", "world"]))
            .is_err()
    );
    assert!(p.check_process(&request("printf", &[""])).is_ok());
    assert!(p.check_process(&request("printf", &[])).is_err());
    assert!(p.check_legacy_shell_command("printf ''").is_ok());
}
#[test]
fn multiple_rules() {
    let p = policy(&["git status", "git diff --stat"]);
    assert!(
        p.check_process(&request("git", &["diff", "--stat", "HEAD"]))
            .is_ok()
    );
    assert!(p.check_process(&request("git", &["diff"])).is_err());
    assert!(p.check_process(&request("git", &["commit"])).is_err());
}
#[test]
fn shell_injection_denied() {
    let p = policy(&["git status"]);
    for s in [
        "git status; echo injected",
        "git status $(echo injected)",
        "git status | cat",
    ] {
        assert!(p.check_legacy_shell_command(s).is_err());
    }
}
#[test]
fn explicit_execution_priority() {
    for mode in [
        ExecutionMode::Allowlist,
        ExecutionMode::Unrestricted,
        ExecutionMode::Deny,
    ] {
        let p = ExecutionPolicy::new(Arc::new(AppConfig {
            project_root: std::env::current_dir().unwrap(),
            allowed_commands: vec!["git status".into()],
            execution_configured: true,
            execution: ExecutionConfig {
                mode,
                allowed_programs: vec!["git".into()],
                ..Default::default()
            },
            ..Default::default()
        }));
        assert_eq!(
            p.check_process(&request("git", &["commit"])).is_ok(),
            mode != ExecutionMode::Deny
        );
    }
}
