//! Execution policy: structured allow/deny decisions for process and
//! shell execution, cwd validation, env validation, and legacy
//! `allowed_commands` migration.
//!
//! Design principle: the security boundary is `program + args` (structured),
//! never shell-string parsing. Shell syntax is only accepted through the
//! explicit `allow_shell` escape hatch or the conservative legacy fast path
//! (simple commands without operators; anything else is denied).

use crate::config::{AppConfig, ExecutionMode};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Why a request was denied. Surfaced to the LLM as structured data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDenial {
    /// `mode = "deny"`: everything is denied.
    GlobalDeny,
    /// Program is not in `allowed_programs`.
    ProgramNotAllowed { program: String },
    /// Program name itself is malformed (shell metachars, separators, ...).
    InvalidProgram { program: String, reason: String },
    /// Shell execution is disabled (`allow_shell = false`).
    ShellDisabled,
    /// Shell command contains dangerous syntax (legacy path only).
    UnsafeShellSyntax { reason: String },
    /// Legacy `allowed_commands` did not match.
    LegacyCommandNotAllowed { command: String },
    /// `cwd` is outside the allowed roots or not a real directory.
    CwdNotAllowed { cwd: String, reason: String },
    /// Environment override contains keys outside `allowed_env`.
    EnvNotAllowed { keys: Vec<String> },
}

impl PolicyDenial {
    pub fn message(&self) -> String {
        match self {
            Self::GlobalDeny => {
                "Process execution is disabled by execution policy (mode = deny)".to_string()
            }
            Self::ProgramNotAllowed { program } => {
                format!("Program '{program}' is not in allowed_programs")
            }
            Self::InvalidProgram { program, reason } => {
                format!("Invalid program '{program}': {reason}")
            }
            Self::ShellDisabled => "Shell execution is disabled by execution policy".to_string(),
            Self::UnsafeShellSyntax { reason } => {
                format!("Shell command rejected by execution policy: {reason}")
            }
            Self::LegacyCommandNotAllowed { command } => {
                format!("Command '{command}' is not allowed")
            }
            Self::CwdNotAllowed { cwd, reason } => {
                format!("Working directory '{cwd}' is not allowed: {reason}")
            }
            Self::EnvNotAllowed { keys } => {
                format!(
                    "Environment variables not in allowed_env: {}",
                    keys.join(", ")
                )
            }
        }
    }
}

/// Structured process request (mirrors the `execute_process` tool schema).
#[derive(Debug, Clone)]
pub struct ProcessRequest {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub timeout_ms: Option<u64>,
}

/// Policy decision context: resolved roots for cwd checks.
/// Process-wide flag so the legacy-fallback notice is logged once, not once
/// per `ExecutionPolicy` instance (a fresh instance is built per tool call).
static LEGACY_FALLBACK_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn warn_legacy_once(msg: &str) {
    if !LEGACY_FALLBACK_WARNED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        tracing::warn!("{msg}; legacy execution policy in effect");
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionPolicy {
    config: Arc<AppConfig>,
}

impl ExecutionPolicy {
    pub fn new(config: Arc<AppConfig>) -> Self {
        Self { config }
    }

    /// True when the new `[execution]` section is the authority (rather than
    /// legacy `allowed_commands` fallback).
    pub fn uses_new_policy(&self) -> bool {
        self.config.execution_configured || !self.uses_legacy_fallback()
    }

    fn uses_legacy_fallback(&self) -> bool {
        !self.config.execution_configured && !self.config.allowed_commands.is_empty()
    }

    // -- process checks ----------------------------------------------------

    /// Check a structured process request against the policy.
    pub fn check_process(&self, req: &ProcessRequest) -> Result<(), PolicyDenial> {
        if self.uses_legacy_fallback() {
            warn_legacy_once("legacy allowed_commands fallback in effect; configure [execution]");
        }
        // Deny mode denies everything first.
        if self.effective_mode() == ExecutionMode::Deny {
            return Err(PolicyDenial::GlobalDeny);
        }
        validate_program_syntax(&req.program)?;
        self.check_program_allowed(&req.program)?;
        self.check_cwd(&req.cwd)?;
        self.check_env(&req.env)?;
        Ok(())
    }

    fn effective_mode(&self) -> ExecutionMode {
        if self.uses_legacy_fallback() {
            // Legacy non-empty allowlist behaves like allowlist mode for
            // structured requests: only explicitly migrated programs run.
            // (Legacy entries with arg prefixes are handled at the shell layer;
            // structured requests require an exact program allow.)
            return ExecutionMode::Allowlist;
        }
        self.config.execution.mode
    }

    fn check_program_allowed(&self, program: &str) -> Result<(), PolicyDenial> {
        // Legacy fallback: allow when the program matches a migrated legacy entry.
        if self.uses_legacy_fallback() {
            for entry in &self.config.allowed_commands {
                if let Some((prog, _)) = parse_simple_command(entry)
                    && prog == program
                {
                    return Ok(());
                }
            }
            return Err(PolicyDenial::ProgramNotAllowed {
                program: program.to_string(),
            });
        }
        match self.config.execution.mode {
            ExecutionMode::Unrestricted => Ok(()),
            ExecutionMode::Deny => Err(PolicyDenial::GlobalDeny),
            ExecutionMode::Allowlist => {
                if is_program_allowed(program, &self.config.execution.allowed_programs) {
                    Ok(())
                } else {
                    Err(PolicyDenial::ProgramNotAllowed {
                        program: program.to_string(),
                    })
                }
            }
        }
    }

    // -- shell checks ------------------------------------------------------

    /// Whether shell tools (`execute_bash` / `execute_shell`) may run.
    pub fn is_shell_allowed(&self) -> bool {
        if self.uses_legacy_fallback() {
            return true;
        }
        self.config.execution.allow_shell
    }

    pub fn check_shell(&self) -> Result<(), PolicyDenial> {
        if self.config.execution.mode == ExecutionMode::Deny && !self.uses_legacy_fallback() {
            return Err(PolicyDenial::GlobalDeny);
        }
        if self.is_shell_allowed() {
            Ok(())
        } else {
            Err(PolicyDenial::ShellDisabled)
        }
    }

    /// Legacy `execute_bash` gate: shell policy + allowlist match.
    ///
    /// Returns the fast-path conversion when the command is a simple command
    /// without shell operators (so it can run without `bash -c`).
    pub fn check_legacy_shell_command(
        &self,
        command: &str,
    ) -> Result<Option<ProcessRequest>, PolicyDenial> {
        if self.uses_legacy_fallback() {
            warn_legacy_once("legacy allowed_commands fallback in effect; configure [execution]");
        }
        self.check_shell()?;
        if self.uses_legacy_fallback() {
            // Empty legacy list is handled before construction (unrestricted);
            // here the list is non-empty.
            if contains_shell_syntax(command) {
                return Err(PolicyDenial::UnsafeShellSyntax {
                    reason:
                        "shell operators/expansions are not allowed under legacy allowed_commands"
                            .to_string(),
                });
            }
            let (prog, args) =
                parse_simple_command(command).ok_or_else(|| PolicyDenial::UnsafeShellSyntax {
                    reason: "command could not be parsed as a simple command".to_string(),
                })?;
            // Match against legacy entries (program + arg-prefix match).
            for entry in &self.config.allowed_commands {
                if let Some((allowed_prog, allowed_args)) = parse_simple_command(entry)
                    && allowed_prog == prog
                    && args.len() >= allowed_args.len()
                    && args[..allowed_args.len()] == allowed_args[..]
                {
                    return Ok(Some(ProcessRequest {
                        program: prog,
                        args,
                        cwd: None,
                        env: BTreeMap::new(),
                        timeout_ms: None,
                    }));
                }
            }
            return Err(PolicyDenial::LegacyCommandNotAllowed {
                command: command.to_string(),
            });
        }
        // New policy authority: shell allowed (checked above); run as-is.
        // An empty command is meaningless — deny rather than spawn `bash -c ""`.
        if command.trim().is_empty() {
            return Err(PolicyDenial::UnsafeShellSyntax {
                reason: "empty command".to_string(),
            });
        }
        Ok(None)
    }

    // -- cwd / env ---------------------------------------------------------

    /// Resolve the effective working directory (defaults to project root).
    pub fn resolve_cwd(&self, cwd: &Option<PathBuf>) -> PathBuf {
        match cwd {
            Some(p) if p.is_absolute() => p.clone(),
            Some(p) => self.config.project_root.join(p),
            None => self.config.project_root.clone(),
        }
    }

    fn check_cwd(&self, cwd: &Option<PathBuf>) -> Result<(), PolicyDenial> {
        let resolved = self.resolve_cwd(cwd);
        let display = resolved.display().to_string();
        // Must exist and be a directory.
        let meta = std::fs::metadata(&resolved).map_err(|_| PolicyDenial::CwdNotAllowed {
            cwd: display.clone(),
            reason: "directory does not exist".to_string(),
        })?;
        if !meta.is_dir() {
            return Err(PolicyDenial::CwdNotAllowed {
                cwd: display.clone(),
                reason: "not a directory".to_string(),
            });
        }
        // Canonicalize to defeat symlink escapes.
        let canon = std::fs::canonicalize(&resolved).map_err(|_| PolicyDenial::CwdNotAllowed {
            cwd: display.clone(),
            reason: "could not canonicalize path".to_string(),
        })?;
        let mut roots: Vec<PathBuf> = vec![self.config.project_root.clone()];
        roots.extend(self.config.allowed_paths.iter().cloned());
        for root in roots {
            let canon_root = std::fs::canonicalize(&root).unwrap_or(root);
            if canon == canon_root || canon.starts_with(&canon_root) {
                return Ok(());
            }
        }
        Err(PolicyDenial::CwdNotAllowed {
            cwd: display,
            reason: "outside project root and allowed_paths".to_string(),
        })
    }

    fn check_env(&self, env: &BTreeMap<String, String>) -> Result<(), PolicyDenial> {
        if env.is_empty() {
            return Ok(());
        }
        // Legacy fallback behaves like allowlist for env: only explicitly
        // allowed keys pass.
        let allowlist =
            self.uses_legacy_fallback() || self.config.execution.mode == ExecutionMode::Allowlist;
        if !allowlist {
            return Ok(());
        }
        let bad: Vec<String> = env
            .keys()
            .filter(|k| !self.config.execution.allowed_env.iter().any(|a| a == *k))
            .cloned()
            .collect();
        if bad.is_empty() {
            Ok(())
        } else {
            Err(PolicyDenial::EnvNotAllowed { keys: bad })
        }
    }

    // -- timeouts ----------------------------------------------------------

    /// Effective timeout: `min(request, config)`. `command_timeout_ms == 0`
    /// keeps the historical unlimited semantics; a `None` request means
    /// "no per-request shrink".
    pub fn effective_timeout(&self, request_ms: Option<u64>) -> Option<Duration> {
        let cfg = self.config.command_timeout_ms;
        let effective = match (request_ms, cfg) {
            (_, 0) => request_ms.filter(|&ms| ms != 0),
            (Some(0), cfg) => Some(cfg),
            (Some(req), cfg) => Some(req.min(cfg)),
            (None, cfg) => Some(cfg),
        };
        effective.filter(|&ms| ms != 0).map(Duration::from_millis)
    }
}

// -- helpers ---------------------------------------------------------------

/// Exact program allow check. No prefix matching: `allowed = ["cargo"]` only
/// allows `program == "cargo"`. Absolute-path rules must be spelled out in
/// config and are compared after canonicalization.
/// Canonical equality for two absolute paths. `None` when either side
/// cannot be canonicalized (caller falls back to the string comparison).
fn canonical_paths_equal(a: &str, b: &str) -> Option<bool> {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => Some(x == y),
        _ => None,
    }
}

fn is_program_allowed(program: &str, allowed: &[String]) -> bool {
    for rule in allowed {
        let both_absolute = Path::new(rule).is_absolute() && Path::new(program).is_absolute();
        if rule == program {
            // Exact match. For absolute rules, also canonicalize both sides
            // so `/opt/tool` and `/opt/tool/`-style variants behave.
            if !both_absolute {
                return true;
            }
            match canonical_paths_equal(rule, program) {
                Some(true) => return true,
                Some(false) => continue,
                None => return true,
            }
        } else if both_absolute && canonical_paths_equal(rule, program) == Some(true) {
            // Absolute rule with canonical equality even if string forms differ.
            return true;
        }
    }
    false
}

/// Reject program names that cannot be plain executable names.
///
/// Rules: non-empty, no path separators (unless the whole program is an
/// absolute path that the caller explicitly allowed), no whitespace, no shell
/// metacharacters, no control characters.
fn validate_program_syntax(program: &str) -> Result<(), PolicyDenial> {
    let fail = |reason: &str| {
        Err(PolicyDenial::InvalidProgram {
            program: program.to_string(),
            reason: reason.to_string(),
        })
    };
    if program.is_empty() {
        return fail("empty program");
    }
    if program.len() > 512 {
        return fail("program name too long");
    }
    if program.contains('\0') || program.chars().any(|c| c.is_control()) {
        return fail("control characters are not allowed");
    }
    // Absolute path: allow the character set, exact-allow decided later.
    if program.starts_with('/') {
        if program.contains('\n') || program.contains('\r') {
            return fail("invalid characters in path");
        }
        // Reject embedded shell metachars even in absolute paths.
        const BAD: &[char] = &[
            ';', '&', '|', '>', '<', '$', '`', '"', '\'', '\\', '(', ')', '{', '}', '!', '#', '~',
            '*', '?', '[', ']',
        ];
        if program.chars().any(|c| BAD.contains(&c)) {
            return fail("shell metacharacters are not allowed");
        }
        return Ok(());
    }
    // Bare name: no separators at all.
    if program.contains('/') || program.contains('\\') {
        return fail("path separators require an explicit absolute-path rule");
    }
    if program.starts_with('.') {
        return fail("relative executable paths are not allowed");
    }
    if program.chars().any(|c| c.is_whitespace()) {
        return fail("whitespace is not allowed in program name");
    }
    const BAD: &[char] = &[
        ';', '&', '|', '>', '<', '$', '`', '"', '\'', '(', ')', '{', '}', '!', '#', '~', '*', '?',
        '[', ']', '=', ',', ':',
    ];
    if program.chars().any(|c| BAD.contains(&c)) {
        return fail("shell metacharacters are not allowed");
    }
    Ok(())
}

/// Conservative shell-syntax detector for the legacy path.
///
/// Anything that is not a plain simple command is rejected. This is NOT a
/// shell parser used as a security boundary for execution — the boundary is
/// "deny unless provably simple"; simple commands run without a shell.
fn contains_shell_syntax(cmd: &str) -> bool {
    // Newlines / carriage returns always indicate compound commands.
    if cmd.contains('\n') || cmd.contains('\r') {
        return true;
    }
    let bytes = cmd.as_bytes();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_single {
            if c == '\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            match c {
                '"' => in_double = false,
                '\\' => i += 1, // skip escaped char
                '$' | '`' => return true,
                _ => {}
            }
            i += 1;
            continue;
        }
        match c {
            '\'' => in_single = true,
            '"' => in_double = true,
            ';' | '&' | '|' | '>' | '<' | '(' | ')' | '`' | '$' | '\\' | '!' | '#' | '~' | '*'
            | '?' | '[' | ']' | '{' | '}' => {
                return true;
            }
            _ => {}
        }
        i += 1;
    }
    // Unbalanced quotes are not simple commands.
    if in_single || in_double {
        return true;
    }
    false
}

/// Tokenize a simple shell command into (program, args).
///
/// Returns `None` when the command is not a provably simple command
/// (operators, expansions, quotes with metachars, ...). Supports single and
/// double quotes for grouping; anything else suspicious is rejected.
fn parse_simple_command(cmd: &str) -> Option<(String, Vec<String>)> {
    if contains_shell_syntax(cmd) {
        return None;
    }
    let mut args: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut in_token = false;
    for c in cmd.chars() {
        if in_single {
            if c == '\'' {
                in_single = false;
            } else {
                cur.push(c);
            }
            continue;
        }
        if in_double {
            if c == '"' {
                in_double = false;
            } else {
                cur.push(c);
            }
            continue;
        }
        match c {
            '\'' => {
                in_single = true;
                in_token = true;
            }
            '"' => {
                in_double = true;
                in_token = true;
            }
            c if c.is_whitespace() => {
                if in_token {
                    args.push(std::mem::take(&mut cur));
                    in_token = false;
                }
            }
            _ => {
                cur.push(c);
                in_token = true;
            }
        }
    }
    if in_single || in_double {
        return None;
    }
    if in_token {
        args.push(cur);
    }
    if args.is_empty() {
        return None;
    }
    let program = args.remove(0);
    // Program itself must be syntactically valid.
    if validate_program_syntax(&program).is_err() {
        // Absolute legacy entries (e.g. `/bin/ls`) are allowed through here;
        // the allow check happens later. Only reject whitespace/control junk.
        if program.is_empty()
            || program.contains('\0')
            || program.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return None;
        }
    }
    Some((program, args))
}

/// Warn-once helper for dual-config situations.
static DUAL_CONFIG_WARNED: OnceLock<()> = OnceLock::new();

/// Emit a one-time warning when both `[execution]` and legacy
/// `allowed_commands` are configured (new policy is authoritative).
pub fn warn_if_dual_config(execution_configured: bool, legacy_non_empty: bool) {
    if execution_configured && legacy_non_empty {
        DUAL_CONFIG_WARNED.get_or_init(|| {
            tracing::warn!(
                "both [execution] and legacy allowed_commands are configured; [execution] is authoritative"
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, ExecutionConfig};
    use tempfile::TempDir;

    fn config_with_execution(exec: ExecutionConfig) -> Arc<AppConfig> {
        let dir = TempDir::new().expect("tempdir");
        // Leak the tempdir path into the config by keeping the dir alive is
        // tricky; instead use the path (dirs live for test duration via keep).
        let root = dir.keep();
        Arc::new(AppConfig {
            project_root: root,
            execution: exec,
            execution_configured: true,
            ..Default::default()
        })
    }

    fn allowlist_cfg(programs: &[&str], env: &[&str]) -> Arc<AppConfig> {
        let exec = ExecutionConfig {
            mode: ExecutionMode::Allowlist,
            allowed_programs: programs.iter().map(|s| s.to_string()).collect(),
            allowed_env: env.iter().map(|s| s.to_string()).collect(),
            allow_shell: false,
        };
        config_with_execution(exec)
    }

    fn req(program: &str, args: &[&str]) -> ProcessRequest {
        ProcessRequest {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: None,
            env: BTreeMap::new(),
            timeout_ms: None,
        }
    }

    #[test]
    fn test_allowlist_success() {
        let cfg = allowlist_cfg(&["cargo"], &[]);
        let policy = ExecutionPolicy::new(cfg);
        assert!(policy.check_process(&req("cargo", &["--version"])).is_ok());
        assert!(
            policy
                .check_process(&req("cargo", &["clippy", "--all"]))
                .is_ok()
        );
    }

    #[test]
    fn test_program_mismatch_denied() {
        let cfg = allowlist_cfg(&["cargo"], &[]);
        let policy = ExecutionPolicy::new(cfg);
        let err = policy.check_process(&req("git", &["status"])).unwrap_err();
        assert!(matches!(err, PolicyDenial::ProgramNotAllowed { .. }));
    }

    #[test]
    fn test_program_name_injection_denied() {
        let cfg = allowlist_cfg(&["cargo"], &[]);
        let policy = ExecutionPolicy::new(cfg);
        for evil in [
            "cargo;rm",
            "cargo && rm",
            "cargo|rm",
            "cargo$(rm)",
            "cargo`rm`",
            "cargo rm",
        ] {
            assert!(
                policy.check_process(&req(evil, &[])).is_err(),
                "should deny {evil}"
            );
        }
    }

    #[test]
    fn test_relative_executable_bypass_denied() {
        let cfg = allowlist_cfg(&["cargo"], &[]);
        let policy = ExecutionPolicy::new(cfg);
        for evil in ["./cargo", "../cargo", "sub/cargo", ".\\cargo"] {
            assert!(
                policy.check_process(&req(evil, &[])).is_err(),
                "should deny {evil}"
            );
        }
    }

    #[test]
    fn test_absolute_executable_bypass_denied() {
        let cfg = allowlist_cfg(&["cargo"], &[]);
        let policy = ExecutionPolicy::new(cfg);
        assert!(policy.check_process(&req("/tmp/cargo", &[])).is_err());
        assert!(policy.check_process(&req("/usr/bin/cargo", &[])).is_err());
    }

    #[test]
    fn test_absolute_rule_allows_exact_path() {
        let dir = TempDir::new().unwrap();
        let tool = dir.path().join("tool");
        std::fs::write(&tool, "x").unwrap();
        let exec = ExecutionConfig {
            mode: ExecutionMode::Allowlist,
            allowed_programs: vec![tool.display().to_string()],
            allow_shell: false,
            ..ExecutionConfig::default()
        };
        let cfg = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            execution: exec,
            execution_configured: true,
            ..Default::default()
        });
        let policy = ExecutionPolicy::new(cfg);
        assert!(
            policy
                .check_process(&ProcessRequest {
                    program: tool.display().to_string(),
                    args: vec![],
                    cwd: None,
                    env: BTreeMap::new(),
                    timeout_ms: None,
                })
                .is_ok()
        );
        // A different absolute path is still denied.
        assert!(policy.check_process(&req("/tmp/other", &[])).is_err());
    }

    #[test]
    fn test_deny_mode_denies_everything() {
        let exec = ExecutionConfig {
            mode: ExecutionMode::Deny,
            ..ExecutionConfig::default()
        };
        let policy = ExecutionPolicy::new(config_with_execution(exec));
        assert!(matches!(
            policy.check_process(&req("cargo", &[])).unwrap_err(),
            PolicyDenial::GlobalDeny
        ));
        assert!(policy.check_shell().is_err());
    }

    #[test]
    fn test_unrestricted_allows() {
        // AppConfig::default has project_root = real CWD; use explicit temp root.
        let dir = TempDir::new().unwrap();
        let cfg = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            ..Default::default()
        });
        let policy = ExecutionPolicy::new(cfg);
        assert!(policy.check_process(&req("anything", &[])).is_ok());
        assert!(policy.check_shell().is_ok());
        let _ = policy;
    }

    #[test]
    fn test_shell_disabled() {
        let cfg = allowlist_cfg(&["cargo"], &[]);
        let policy = ExecutionPolicy::new(cfg);
        assert!(!policy.is_shell_allowed());
        assert!(matches!(
            policy.check_shell().unwrap_err(),
            PolicyDenial::ShellDisabled
        ));
    }

    #[test]
    fn test_env_policy() {
        let cfg = allowlist_cfg(&["cargo"], &["RUST_BACKTRACE"]);
        let policy = ExecutionPolicy::new(cfg);
        let mut ok_env = BTreeMap::new();
        ok_env.insert("RUST_BACKTRACE".to_string(), "1".to_string());
        let ok_req = ProcessRequest {
            program: "cargo".to_string(),
            args: vec![],
            cwd: None,
            env: ok_env,
            timeout_ms: None,
        };
        assert!(policy.check_process(&ok_req).is_ok());

        let mut bad_env = BTreeMap::new();
        bad_env.insert("PATH".to_string(), "/tmp/evil".to_string());
        let bad_req = ProcessRequest {
            program: "cargo".to_string(),
            args: vec![],
            cwd: None,
            env: bad_env,
            timeout_ms: None,
        };
        let err = policy.check_process(&bad_req).unwrap_err();
        assert!(matches!(err, PolicyDenial::EnvNotAllowed { .. }));
        // Values never appear in the denial message.
        assert!(!err.message().contains("/tmp/evil"));
    }

    #[test]
    fn test_effective_timeout_min_and_unlimited() {
        let dir = TempDir::new().unwrap();
        let cfg = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            command_timeout_ms: 300_000,
            ..Default::default()
        });
        let policy = ExecutionPolicy::new(cfg);
        assert_eq!(
            policy.effective_timeout(Some(1_000)),
            Some(Duration::from_millis(1_000))
        );
        assert_eq!(
            policy.effective_timeout(Some(999_999)),
            Some(Duration::from_millis(300_000))
        );
        assert_eq!(
            policy.effective_timeout(None),
            Some(Duration::from_millis(300_000))
        );
        // Config 0 == unlimited: request passes through, None stays unlimited.
        let cfg0 = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            command_timeout_ms: 0,
            ..Default::default()
        });
        let policy0 = ExecutionPolicy::new(cfg0);
        assert_eq!(
            policy0.effective_timeout(Some(5_000)),
            Some(Duration::from_millis(5_000))
        );
        assert_eq!(policy0.effective_timeout(None), None);
    }

    #[test]
    fn test_legacy_injection_rejected() {
        let dir = TempDir::new().unwrap();
        let cfg = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            allowed_commands: vec!["cargo".to_string()],
            ..Default::default()
        });
        let policy = ExecutionPolicy::new(cfg);
        // Simple command fast path is allowed.
        let fast = policy
            .check_legacy_shell_command("cargo test")
            .expect("fast path");
        assert!(fast.is_some());
        let fast = fast.unwrap();
        assert_eq!(fast.program, "cargo");
        assert_eq!(fast.args, vec!["test".to_string()]);
        // Injections are denied.
        for evil in [
            "cargo test; echo hacked",
            "cargo test && echo hacked",
            "cargo test | cat",
            "cargo test > result.txt",
            "cargo test $(echo foo)",
            "cargo test `echo foo`",
            "cargo test\n echo hacked",
            "cargo test\recho hacked",
        ] {
            assert!(
                policy.check_legacy_shell_command(evil).is_err(),
                "should deny {evil:?}"
            );
        }
    }

    #[test]
    fn test_legacy_arg_prefix() {
        let dir = TempDir::new().unwrap();
        let cfg = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            allowed_commands: vec!["git status".to_string()],
            ..Default::default()
        });
        let policy = ExecutionPolicy::new(cfg);
        assert!(policy.check_legacy_shell_command("git status").is_ok());
        assert!(
            policy
                .check_legacy_shell_command("git status --short")
                .is_ok()
        );
        assert!(policy.check_legacy_shell_command("git commit").is_err());
    }

    #[test]
    fn test_cwd_policy() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let sub = root.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let outside = TempDir::new().unwrap();
        let cfg = Arc::new(AppConfig {
            project_root: root.clone(),
            ..Default::default()
        });
        let policy = ExecutionPolicy::new(cfg);
        assert!(policy.check_cwd(&None).is_ok());
        assert!(policy.check_cwd(&Some(sub)).is_ok());
        assert!(policy.check_cwd(&Some(root.clone())).is_ok());
        assert!(
            policy
                .check_cwd(&Some(outside.path().to_path_buf()))
                .is_err()
        );
        // allowed_paths grants an extra root.
        let cfg2 = Arc::new(AppConfig {
            project_root: root.clone(),
            allowed_paths: vec![outside.path().to_path_buf()],
            ..Default::default()
        });
        let policy2 = ExecutionPolicy::new(cfg2);
        assert!(
            policy2
                .check_cwd(&Some(outside.path().to_path_buf()))
                .is_ok()
        );
    }

    #[test]
    fn test_new_config_is_authoritative_over_legacy() {
        use crate::config::ExecutionMode;
        let dir = TempDir::new().unwrap();
        // Both set: [execution] wins, legacy list ignored.
        let exec = crate::config::ExecutionConfig {
            mode: ExecutionMode::Allowlist,
            allowed_programs: vec!["git".to_string()],
            allow_shell: false,
            ..crate::config::ExecutionConfig::default()
        };
        let cfg = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            execution: exec,
            execution_configured: true,
            allowed_commands: vec!["cargo".to_string()],
            ..Default::default()
        });
        let policy = ExecutionPolicy::new(cfg);
        assert!(policy.uses_new_policy());
        // cargo (legacy) denied, git (new) allowed.
        assert!(policy.check_process(&req("cargo", &[])).is_err());
        assert!(policy.check_process(&req("git", &[])).is_ok());
        assert!(policy.check_shell().is_err());
    }

    #[test]
    fn test_legacy_empty_means_unrestricted_with_warning_once() {
        let dir = TempDir::new().unwrap();
        let cfg = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            allowed_commands: vec![],
            ..Default::default()
        });
        let policy = ExecutionPolicy::new(cfg);
        // Unrestricted legacy: shell allowed, any program allowed.
        assert!(policy.check_shell().is_ok());
        assert!(policy.check_process(&req("anything", &[])).is_ok());
        assert!(policy.check_legacy_shell_command("rm -rf /tmp/x").is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn test_cwd_symlink_escape_denied() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let outside = TempDir::new().unwrap();
        let link = root.join("evil-link");
        symlink(outside.path(), &link).unwrap();
        let cfg = Arc::new(AppConfig {
            project_root: root,
            ..Default::default()
        });
        let policy = ExecutionPolicy::new(cfg);
        assert!(policy.check_cwd(&Some(link)).is_err());
    }
}
