use crate::config::*;
use std::collections::BTreeMap;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_llm_config_apply_partial() {
    let mut config = LlmConfig::default();
    let partial = PartialLlmConfig {
        connect_timeout_ms: Some(999),
        max_retries: Some(5),
        ..Default::default()
    };
    config.apply_partial(&partial);
    assert_eq!(config.connect_timeout_ms, 999);
    assert_eq!(config.max_retries, 5);
    // Other fields should remain default
    assert_eq!(
        config.request_timeout_ms,
        LlmConfig::default().request_timeout_ms
    );
}

#[test]
fn test_watch_config_apply_partial() {
    let mut config = WatchConfig::default();
    let partial = PartialWatchConfig {
        debounce_delay_ms: Some(123),
        backup_enabled: Some(false),
        ..Default::default()
    };
    config.apply_partial(&partial);
    assert_eq!(config.debounce_delay_ms, Some(123));
    assert!(!config.backup_enabled.unwrap_or(true));
}

#[test]
fn test_mcp_servers_merge_logic() {
    let file_servers = vec![PartialMcpServerConfig {
        name: Some("server1".to_string()),
        enabled: Some(true),
        address: Some("1.2.3.4".to_string()),
        ..Default::default()
    }];
    let project_servers = vec![PartialMcpServerConfig {
        name: Some("server1".to_string()),
        enabled: Some(false),
        ..Default::default()
    }];

    let merged = merge_mcp_servers(Some(&file_servers), Some(&project_servers));
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].name, "server1");
    assert!(!merged[0].enabled); // Project overrides file
    assert_eq!(merged[0].address.as_deref(), Some("1.2.3.4")); // Field from file preserved
}

#[test]
fn test_mcp_structured_stdio_merge_preserves_argv_and_env_precedence() {
    let global = PartialMcpServerConfig {
        name: Some("filesystem".to_string()),
        enabled: Some(true),
        transport: Some(McpTransport::Stdio),
        command: Some("/tmp/foo server".to_string()),
        args: Some(vec![
            "--config".to_string(),
            "/tmp/foo config.json".to_string(),
        ]),
        env: Some(BTreeMap::from([
            ("DOGE_GLOBAL".to_string(), "yes".to_string()),
            ("DOGE_OVERRIDE".to_string(), "global".to_string()),
        ])),
        connect_timeout_ms: Some(11_000),
        list_timeout_ms: Some(12_000),
        call_timeout_ms: Some(13_000),
        ..Default::default()
    };
    let project = PartialMcpServerConfig {
        name: Some("filesystem".to_string()),
        args: Some(vec![
            "--project".to_string(),
            "value with spaces".to_string(),
        ]),
        env: Some(BTreeMap::from([(
            "DOGE_OVERRIDE".to_string(),
            "project".to_string(),
        )])),
        call_timeout_ms: Some(0),
        ..Default::default()
    };

    let global_servers = vec![global];
    let project_servers = vec![project];
    let merged = merge_mcp_servers(Some(&global_servers), Some(&project_servers));
    assert_eq!(merged.len(), 1);
    let server = &merged[0];
    assert_eq!(server.transport, McpTransport::Stdio);
    assert_eq!(server.command.as_deref(), Some("/tmp/foo server"));
    assert_eq!(server.args, vec!["--project", "value with spaces"]);
    assert_eq!(server.address, None);
    assert_eq!(
        server.env.get("DOGE_GLOBAL").map(String::as_str),
        Some("yes")
    );
    assert_eq!(
        server.env.get("DOGE_OVERRIDE").map(String::as_str),
        Some("project")
    );
    assert_eq!(server.connect_timeout_ms, 11_000);
    assert_eq!(server.list_timeout_ms, 12_000);
    assert_eq!(server.call_timeout_ms, 0);
    server.validate().expect("merged structured stdio config");
}

#[test]
fn test_mcp_project_structured_command_does_not_inherit_http_address() {
    let global = vec![PartialMcpServerConfig {
        name: Some("switch".to_string()),
        enabled: Some(true),
        transport: Some(McpTransport::Http),
        address: Some("http://127.0.0.1:8000/mcp".to_string()),
        ..Default::default()
    }];
    let project = vec![PartialMcpServerConfig {
        name: Some("switch".to_string()),
        command: Some("/tmp/mcp server".to_string()),
        ..Default::default()
    }];

    let merged = merge_mcp_servers(Some(&global), Some(&project));
    let server = &merged[0];
    assert_eq!(server.transport, McpTransport::Stdio);
    assert_eq!(server.command.as_deref(), Some("/tmp/mcp server"));
    assert_eq!(server.address, None);
    server
        .validate()
        .expect("structured stdio override should validate");
}

#[test]
fn test_mcp_project_http_switch_drops_inherited_stdio_fields() {
    let global = vec![PartialMcpServerConfig {
        name: Some("switch".to_string()),
        enabled: Some(true),
        transport: Some(McpTransport::Stdio),
        address: Some("legacy-server --flag".to_string()),
        command: Some("legacy-server".to_string()),
        args: Some(vec!["--flag".to_string()]),
        ..Default::default()
    }];
    let project = vec![PartialMcpServerConfig {
        name: Some("switch".to_string()),
        transport: Some(McpTransport::Http),
        ..Default::default()
    }];

    let merged = merge_mcp_servers(Some(&global), Some(&project));
    let server = &merged[0];
    assert_eq!(server.transport, McpTransport::Http);
    assert_eq!(server.address.as_deref(), Some("legacy-server --flag"));
    assert_eq!(server.command, None);
    assert!(server.args.is_empty());
    server
        .validate()
        .expect_err("legacy command text is not a valid HTTP URL");
}

#[test]
fn test_mcp_legacy_stdio_address_remains_valid() {
    let parsed: FileConfig = toml::from_str(
        r#"
        [[mcp_servers]]
        name = "legacy"
        enabled = true
        transport = "stdio"
        address = "server --foo bar"
        "#,
    )
    .expect("legacy MCP config should parse");
    let merged = merge_mcp_servers(parsed.mcp_servers.as_ref(), None);
    assert_eq!(merged[0].address.as_deref(), Some("server --foo bar"));
    assert!(merged[0].validate().is_ok());
}

#[test]
fn test_mcp_servers_no_duplicates() {
    let file_servers = vec![PartialMcpServerConfig {
        name: Some("server1".to_string()),
        ..Default::default()
    }];
    let project_servers = vec![PartialMcpServerConfig {
        name: Some("server2".to_string()),
        ..Default::default()
    }];

    let merged = merge_mcp_servers(Some(&file_servers), Some(&project_servers));
    assert_eq!(merged.len(), 2);
}

#[test]
fn test_local_mcp_config_defaults() {
    let merged = merge_local_mcp_server(None, None);
    assert!(!merged.enabled);
    assert_eq!(merged.address, "127.0.0.1:8000");
}

#[test]
fn test_local_mcp_config_project_overrides_global() {
    let global = PartialLocalMcpServerConfig {
        enabled: Some(true),
        address: Some("127.0.0.1:8001".to_string()),
    };
    let project = PartialLocalMcpServerConfig {
        enabled: None,
        address: Some("127.0.0.1:9000".to_string()),
    };

    let merged = merge_local_mcp_server(Some(&global), Some(&project));
    assert!(merged.enabled);
    assert_eq!(merged.address, "127.0.0.1:9000");
}

#[test]
fn test_local_mcp_config_parses_toml() {
    let toml_str = r#"
        [mcp_server]
        enabled = true
        address = "127.0.0.1:8001"

        [[mcp_servers]]
        name = "remote"
        enabled = true
        address = "http://example.com/mcp"
        transport = "http"
    "#;
    let cfg: FileConfig = toml::from_str(toml_str).expect("parse local mcp config");
    let local = cfg.mcp_server.expect("mcp_server section");
    assert_eq!(local.enabled, Some(true));
    assert_eq!(local.address.as_deref(), Some("127.0.0.1:8001"));
    let remotes = cfg.mcp_servers.expect("mcp_servers section");
    assert_eq!(remotes.len(), 1);
    assert_eq!(remotes[0].name.as_deref(), Some("remote"));
}

#[test]
fn test_execution_merge_project_wins() {
    let file = PartialExecutionConfig {
        mode: Some(ExecutionMode::Allowlist),
        allowed_programs: Some(vec!["cargo".to_string()]),
        allow_shell: Some(false),
        allowed_env: None,
    };
    let project = PartialExecutionConfig {
        mode: None,
        allowed_programs: Some(vec!["cargo".to_string(), "git".to_string()]),
        allow_shell: None,
        allowed_env: Some(vec!["RUST_BACKTRACE".to_string()]),
    };
    let merged = merge_execution(Some(&file), Some(&project)).expect("merged");
    // Project list replaces global list (documented precedence).
    assert_eq!(
        merged.allowed_programs,
        Some(vec!["cargo".to_string(), "git".to_string()])
    );
    assert_eq!(merged.mode, Some(ExecutionMode::Allowlist));
    assert_eq!(merged.allow_shell, Some(false));

    // Resolved config applies the merge.
    let mut cfg = ExecutionConfig::default();
    cfg.apply_partial(&merged);
    assert_eq!(cfg.mode, ExecutionMode::Allowlist);
    assert_eq!(
        cfg.allowed_programs,
        vec!["cargo".to_string(), "git".to_string()]
    );
    assert!(!cfg.allow_shell);
}

#[test]
fn test_execution_config_parses_toml() {
    let toml_str = r#"
        [execution]
        mode = "allowlist"
        allowed_programs = ["cargo", "git"]
        allow_shell = false
        allowed_env = ["RUST_BACKTRACE"]
    "#;
    let cfg: FileConfig = toml::from_str(toml_str).expect("parse execution config");
    let exec = cfg.execution.expect("execution section");
    assert_eq!(exec.mode, Some(ExecutionMode::Allowlist));
    assert_eq!(
        exec.allowed_programs,
        Some(vec!["cargo".to_string(), "git".to_string()])
    );
    assert_eq!(exec.allow_shell, Some(false));
}

#[test]
fn test_tool_routing_merge_project_wins() {
    let file = PartialToolRoutingConfig {
        mode: Some(ToolRoutingMode::Deferred),
        search_result_limit: Some(3),
    };
    let project = PartialToolRoutingConfig {
        mode: Some(ToolRoutingMode::Eager),
        search_result_limit: None,
    };
    let merged = merge_tool_routing(Some(&file), Some(&project));
    assert_eq!(merged.mode, ToolRoutingMode::Eager);
    assert_eq!(merged.search_result_limit, 3);
}

#[test]
fn test_tool_routing_config_parses_toml() {
    let toml_str = r#"
        [tool_routing]
        mode = "deferred"
        search_result_limit = 7
    "#;
    let cfg: FileConfig = toml::from_str(toml_str).expect("parse tool_routing config");
    let routing = cfg.tool_routing.expect("tool_routing section");
    assert_eq!(routing.mode, Some(ToolRoutingMode::Deferred));
    assert_eq!(routing.search_result_limit, Some(7));
    let resolved = merge_tool_routing(None, Some(&routing));
    assert_eq!(resolved.mode, ToolRoutingMode::Deferred);
    assert_eq!(resolved.search_result_limit, 7);
}

#[test]
fn test_tool_routing_limit_clamped_on_resolve() {
    let project = PartialToolRoutingConfig {
        mode: None,
        search_result_limit: Some(99),
    };
    let resolved = merge_tool_routing(None, Some(&project));
    assert_eq!(resolved.mode, ToolRoutingMode::Auto);
    assert_eq!(resolved.search_result_limit, MAX_TOOL_SEARCH_RESULT_LIMIT);
}

#[test]
fn test_reasoning_merge_project_wins() {
    let file = PartialReasoningConfig {
        mode: Some(ReasoningMode::Fixed),
        routine_effort: Some(ReasoningEffort::Low),
        fixed_effort: Some(ReasoningEffort::High),
        ..Default::default()
    };
    let project = PartialReasoningConfig {
        mode: Some(ReasoningMode::Off),
        ..Default::default()
    };
    let merged = merge_reasoning(Some(&file), Some(&project));
    assert_eq!(merged.mode, ReasoningMode::Off);
    // Untouched fields keep the global value.
    assert_eq!(merged.routine_effort, ReasoningEffort::Low);
    assert_eq!(merged.fixed_effort, ReasoningEffort::High);
}

#[test]
fn test_reasoning_config_parses_toml() {
    let toml_str = r#"
        [reasoning]
        mode = "fixed"
        fixed_effort = "high"
        routine_effort = "low"
    "#;
    let cfg: FileConfig = toml::from_str(toml_str).expect("parse reasoning config");
    let reasoning = cfg.reasoning.expect("reasoning section");
    assert_eq!(reasoning.mode, Some(ReasoningMode::Fixed));
    assert_eq!(reasoning.fixed_effort, Some(ReasoningEffort::High));
    let resolved = merge_reasoning(None, Some(&reasoning));
    assert_eq!(resolved.mode, ReasoningMode::Fixed);
    assert_eq!(resolved.fixed_effort, ReasoningEffort::High);
    assert_eq!(resolved.routine_effort, ReasoningEffort::Low);
}

#[test]
fn test_file_config_rejects_unknown_fields() {
    let toml_str = r#"
        [llm]
        connect_timeout_ms = 1000

        # These fields are removed config
        [verification]
        enabled = true

        [test_fix]
        max_iterations = 5
    "#;

    let config: Result<FileConfig, _> = toml::from_str(toml_str);
    assert!(
        config.is_err(),
        "Unknown sections must be rejected, not silently ignored"
    );
}

#[test]
fn test_context_budget_merge_project_wins() {
    let file = PartialContextBudgetConfig {
        mode: Some(ContextBudgetMode::Observe),
    };
    let project = PartialContextBudgetConfig {
        mode: Some(ContextBudgetMode::Off),
    };
    let merged = merge_context_budget(Some(&file), Some(&project));
    assert_eq!(merged.mode, ContextBudgetMode::Off);
}

#[test]
fn test_context_budget_config_parses_toml() {
    let toml_str = r#"
        [context_budget]
        mode = "observe"
    "#;
    let cfg: FileConfig = toml::from_str(toml_str).expect("parse context_budget config");
    let budget = cfg.context_budget.expect("context_budget section");
    assert_eq!(budget.mode, Some(ContextBudgetMode::Observe));
    let resolved = merge_context_budget(None, Some(&budget));
    assert_eq!(resolved.mode, ContextBudgetMode::Observe);
}

// --- Configuration contract v1 regression tests ---
//
// Loading is a read path: missing config uses runtime defaults without
// filesystem mutation, existing invalid config fails without overwrite, and
// `DOGE_CODE_CONFIG` (injected explicit path) is authoritative.

#[test]
fn missing_global_config_uses_defaults_without_creating_file() {
    let temp_dir = TempDir::new().unwrap();
    let missing_one = temp_dir.path().join("one").join("config.toml");
    let missing_two = temp_dir.path().join("two").join("config.toml");

    let cfg = load_file_config_from_candidates(None, &[missing_one.clone(), missing_two.clone()])
        .expect("missing config uses defaults");
    assert_eq!(cfg, FileConfig::default());

    assert!(
        !missing_one.exists(),
        "missing config must not create {:?}",
        missing_one
    );
    assert!(
        !missing_two.exists(),
        "missing config must not create {:?}",
        missing_two
    );
    assert!(
        !temp_dir.path().join("one").exists(),
        "missing config must not create parent directories"
    );
}

#[test]
fn invalid_global_config_fails_without_overwrite() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    let broken = "[llm\nmax_retries = ???\n";
    fs::write(&config_path, broken).unwrap();
    let before = fs::read(&config_path).unwrap();

    let result = load_file_config_from_candidates(None, std::slice::from_ref(&config_path));
    assert!(
        result.is_err(),
        "invalid config must fail, got {:?}",
        result.ok()
    );
    let err = format!("{:#}", result.expect_err("invalid config must fail"));
    assert!(
        err.contains(&config_path.display().to_string()),
        "error must include path, got: {err}"
    );
    assert!(
        !err.contains("???"),
        "error must not dump raw config contents, got: {err}"
    );

    let after = fs::read(&config_path).unwrap();
    assert_eq!(before, after, "invalid config file must not be overwritten");
}

#[test]
fn explicit_config_missing_is_error() {
    let temp_dir = TempDir::new().unwrap();
    let missing_explicit = temp_dir.path().join("explicit-missing.toml");
    let fallback = temp_dir.path().join("fallback.toml");
    fs::write(&fallback, "model = \"fallback-model\"\n").unwrap();

    let result =
        load_file_config_from_candidates(Some(&missing_explicit), std::slice::from_ref(&fallback));
    assert!(
        result.is_err(),
        "explicit missing path must error without fallback"
    );
    let err = format!("{:#}", result.expect_err("must error"));
    assert!(
        err.contains(&missing_explicit.display().to_string()),
        "error must include explicit path, got: {err}"
    );
}

#[test]
fn explicit_config_invalid_does_not_fall_back() {
    let temp_dir = TempDir::new().unwrap();
    let explicit_path = temp_dir.path().join("explicit.toml");
    let fallback_path = temp_dir.path().join("fallback.toml");
    fs::write(&explicit_path, "[llm\nmax_retries = ???\n").unwrap();
    fs::write(&fallback_path, "model = \"fallback-model\"\n").unwrap();
    let before_explicit = fs::read(&explicit_path).unwrap();
    let before_fallback = fs::read(&fallback_path).unwrap();

    let result = load_file_config_from_candidates(
        Some(&explicit_path),
        std::slice::from_ref(&fallback_path),
    );
    assert!(
        result.is_err(),
        "explicit invalid config must error without using fallback"
    );

    assert_eq!(
        fs::read(&explicit_path).unwrap(),
        before_explicit,
        "explicit file must not be overwritten"
    );
    assert_eq!(
        fs::read(&fallback_path).unwrap(),
        before_fallback,
        "fallback file must not be touched"
    );
}

#[test]
fn invalid_implicit_high_priority_does_not_fall_back() {
    let temp_dir = TempDir::new().unwrap();
    let high = temp_dir.path().join("high.toml");
    let low = temp_dir.path().join("low.toml");
    fs::write(&high, "[llm\nmax_retries = ???\n").unwrap();
    fs::write(&low, "model = \"low-model\"\n").unwrap();
    let before_high = fs::read(&high).unwrap();

    let result = load_file_config_from_candidates(None, &[high.clone(), low.clone()]);
    assert!(
        result.is_err(),
        "broken high-priority candidate must fail, not fall back"
    );
    let err = format!("{:#}", result.expect_err("must error"));
    assert!(
        err.contains(&high.display().to_string()),
        "error must include high-priority path, got: {err}"
    );
    assert_eq!(
        fs::read(&high).unwrap(),
        before_high,
        "broken candidate must not be overwritten"
    );
}

#[test]
fn absent_high_priority_falls_through_to_next() {
    let temp_dir = TempDir::new().unwrap();
    let missing = temp_dir.path().join("missing.toml");
    let low = temp_dir.path().join("low.toml");
    fs::write(&low, "model = \"low-model\"\n").unwrap();

    let cfg = load_file_config_from_candidates(None, &[missing, low.clone()])
        .expect("missing high-priority candidate falls through");
    assert_eq!(cfg.model.as_deref(), Some("low-model"));
}

#[test]
fn explicit_valid_config_loads_without_consulting_implicit() {
    let temp_dir = TempDir::new().unwrap();
    let explicit_path = temp_dir.path().join("explicit.toml");
    let implicit_path = temp_dir.path().join("implicit.toml");
    fs::write(&explicit_path, "model = \"explicit-model\"\n").unwrap();
    fs::write(&implicit_path, "model = \"implicit-model\"\n").unwrap();

    let cfg = load_file_config_from_candidates(
        Some(&explicit_path),
        std::slice::from_ref(&implicit_path),
    )
    .expect("explicit valid config loads");
    assert_eq!(cfg.model.as_deref(), Some("explicit-model"));
}

#[test]
fn legacy_exact_match_uses_current_defaults_without_mutation() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    let legacy = crate::config::loading::legacy_generated_default_content();
    assert!(
        crate::config::loading::is_legacy_generated_default(legacy),
        "fixture must be recognized as legacy"
    );
    // CRLF normalization is the only permitted equivalence.
    let crlf = legacy.replace('\n', "\r\n");
    assert!(
        crate::config::loading::is_legacy_generated_default(&crlf),
        "CRLF variant must be recognized as legacy"
    );
    fs::write(&config_path, legacy).unwrap();
    let before = fs::read(&config_path).unwrap();

    let cfg = load_file_config_from_candidates(None, std::slice::from_ref(&config_path))
        .expect("untouched legacy config is ignored with current defaults");
    assert_eq!(cfg, FileConfig::default());

    assert_eq!(
        fs::read(&config_path).unwrap(),
        before,
        "legacy file must not be rewritten, deleted, or renamed"
    );

    // Current runtime defaults apply, not stale template values.
    let mut llm = LlmConfig::default();
    if let Some(partial) = &cfg.llm {
        llm.apply_partial(partial);
    }
    assert_eq!(llm.max_retries, LlmConfig::default().max_retries);
    assert_eq!(llm.max_retries, 3);
}

#[test]
fn modified_legacy_invalid_config_is_error_without_mutation() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("config.toml");
    let legacy = crate::config::loading::legacy_generated_default_content();
    let mut modified = legacy.to_string();
    modified.push_str("# user touched\n");
    assert!(
        !crate::config::loading::is_legacy_generated_default(&modified),
        "edited legacy content must not be classified as untouched"
    );
    fs::write(&config_path, &modified).unwrap();
    let before = fs::read(&config_path).unwrap();

    let result = load_file_config_from_candidates(None, std::slice::from_ref(&config_path));
    assert!(
        result.is_err(),
        "edited legacy invalid config must fail, not be silently ignored"
    );

    assert_eq!(
        fs::read(&config_path).unwrap(),
        before,
        "user-edited file must not be overwritten"
    );
}

#[test]
fn missing_config_resolves_retry_defaults_from_runtime() {
    let temp_dir = TempDir::new().unwrap();
    let missing = temp_dir.path().join("missing.toml");
    let cfg = load_file_config_from_candidates(None, std::slice::from_ref(&missing))
        .expect("missing config loads defaults");

    let mut llm = LlmConfig::default();
    if let Some(partial) = &cfg.llm {
        llm.apply_partial(partial);
    }
    assert_eq!(llm.max_retries, LlmConfig::default().max_retries);
    assert_eq!(llm.max_retries, 3);
    assert_ne!(
        llm.max_retries, 100,
        "stale template max_retries=100 must never apply on fresh installs"
    );
}

#[test]
fn dangling_symlink_candidate_is_error_not_missing() {
    let temp_dir = TempDir::new().unwrap();
    let target = temp_dir.path().join("nonexistent-target.toml");
    let link = temp_dir.path().join("link.toml");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    #[cfg(not(unix))]
    std::os::windows::fs::symlink_file(&target, &link).unwrap();

    let result = load_file_config_from_candidates(None, std::slice::from_ref(&link));
    assert!(
        result.is_err(),
        "dangling symlink must fail, not be skipped as missing"
    );
    let err = format!("{:#}", result.expect_err("must error"));
    assert!(
        err.contains(&link.display().to_string()),
        "error must include symlink path, got: {err}"
    );
}

#[test]
fn test_subagent_zero_rejected_by_config_loaders_without_mutation() {
    let dir = TempDir::new().expect("tempdir");
    let global = dir.path().join("user.toml");
    fs::create_dir(dir.path().join(".doge")).expect("mkdir");
    let project = dir.path().join(".doge/config.toml");
    for field in [
        "max_iterations",
        "max_tool_calls",
        "max_elapsed_ms",
        "max_total_tokens",
    ] {
        let text = format!("[subagent]\n{field}=0\n");
        fs::write(&global, &text).expect("global");
        fs::write(&project, &text).expect("project");
        let error = load_file_config_from_candidates(Some(&global), &[]).expect_err("reject zero");
        assert!(format!("{error:#}").contains(field));
        assert!(load_project_config(dir.path()).is_err());
        assert_eq!(fs::read_to_string(&global).expect("read"), text);
        assert_eq!(fs::read_to_string(&project).expect("read"), text);
    }
}

#[test]
fn test_subagent_global_project_field_wise_loading() {
    let dir = TempDir::new().expect("tempdir");
    let global = dir.path().join("user.toml");
    fs::create_dir(dir.path().join(".doge")).expect("mkdir");
    fs::write(
        &global,
        "[subagent]\nmax_iterations=8\nmax_tool_calls=12\nmax_total_tokens=5000",
    )
    .expect("write");
    fs::write(
        dir.path().join(".doge/config.toml"),
        "[subagent]\nmax_iterations=2\nmax_elapsed_ms=1000",
    )
    .expect("write");
    let user = load_file_config_from_candidates(Some(&global), &[]).expect("user");
    let project = load_project_config(dir.path()).expect("project");
    let merged = merge_subagent(user.subagent.as_ref(), project.subagent.as_ref()).expect("merge");
    assert_eq!(
        (
            merged.max_iterations,
            merged.max_tool_calls,
            merged.max_elapsed_ms,
            merged.max_total_tokens
        ),
        (2, 12, 1000, Some(5000))
    );
}

#[test]
fn test_agent_budget_zero_rejected_by_config_loaders_without_mutation() {
    let dir = TempDir::new().expect("tempdir");
    let global = dir.path().join("user.toml");
    fs::create_dir(dir.path().join(".doge")).expect("mkdir");
    let project = dir.path().join(".doge/config.toml");
    for field in [
        "max_iterations",
        "max_tool_calls",
        "max_elapsed_ms",
        "max_total_tokens",
    ] {
        let text = format!("[agent_budget]\n{field}=0\n");
        fs::write(&global, &text).expect("global");
        fs::write(&project, &text).expect("project");
        let error = load_file_config_from_candidates(Some(&global), &[]).expect_err("reject zero");
        assert!(format!("{error:#}").contains(field));
        assert!(load_project_config(dir.path()).is_err());
        assert_eq!(fs::read_to_string(&global).expect("read"), text);
        assert_eq!(fs::read_to_string(&project).expect("read"), text);
    }
}

#[test]
fn test_agent_budget_global_project_field_wise_loading() {
    let dir = TempDir::new().expect("tempdir");
    let global = dir.path().join("user.toml");
    fs::create_dir(dir.path().join(".doge")).expect("mkdir");
    fs::write(
        &global,
        "[agent_budget]\nmax_iterations=128\nmax_total_tokens=800000",
    )
    .expect("write");
    fs::write(
        dir.path().join(".doge/config.toml"),
        "[agent_budget]\nmax_elapsed_ms=900000",
    )
    .expect("write");
    let user = load_file_config_from_candidates(Some(&global), &[]).expect("user");
    let project = load_project_config(dir.path()).expect("project");
    let merged = merge_agent_budget(user.agent_budget.as_ref(), project.agent_budget.as_ref())
        .expect("merge");
    assert_eq!(merged.max_iterations, 128);
    assert_eq!(merged.max_total_tokens, Some(800000));
    assert_eq!(merged.max_elapsed_ms, Some(900000));
    assert_eq!(merged.max_tool_calls, None);
    // Project partial table must not erase user fields.
    assert_eq!(
        (
            merged.max_iterations,
            merged.max_tool_calls,
            merged.max_elapsed_ms,
            merged.max_total_tokens
        ),
        (128, None, Some(900000), Some(800000))
    );
}

fn write_temp_config(dir: &TempDir, name: &str, content: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, content).unwrap();
    path
}

fn assert_load_error_without_mutation(path: &std::path::Path, before: &[u8], err: &str) {
    assert!(
        err.contains(&path.display().to_string()),
        "error must include config path, got: {err}"
    );
    let after = fs::read(path).unwrap();
    assert_eq!(
        after, before,
        "invalid config file must remain byte-for-byte unchanged"
    );
}

#[test]
fn unknown_top_level_key_is_error_without_mutation() {
    let dir = TempDir::new().unwrap();
    let content = "modle = \"gpt-5\"\napi_key = \"secret-value-123\"\n";
    let path = write_temp_config(&dir, "config.toml", content);
    let before = fs::read(&path).unwrap();
    let result = load_file_config_from_candidates(Some(&path), &[]);
    assert!(result.is_err(), "unknown top-level key must fail");
    let err = format!("{:#}", result.expect_err("must error"));
    assert_load_error_without_mutation(&path, &before, &err);
    assert!(
        !err.contains("secret-value-123"),
        "error must not leak secret, got: {err}"
    );
}

#[test]
fn unknown_top_level_section_is_error() {
    let dir = TempDir::new().unwrap();
    let content = "[project]\nexclude_patterns = [\"target\"]\n";
    let path = write_temp_config(&dir, "config.toml", content);
    let before = fs::read(&path).unwrap();
    let result = load_file_config_from_candidates(Some(&path), &[]);
    assert!(result.is_err(), "[project] section must fail");
    let err = format!("{:#}", result.expect_err("must error"));
    assert_load_error_without_mutation(&path, &before, &err);
}

#[test]
fn execution_unknown_key_cannot_fall_back_to_unrestricted() {
    for content in [
        "[execution]\nmod = \"deny\"\n",
        "[execution]\nmode = \"deny\"\nallow_shelll = false\n",
    ] {
        let dir = TempDir::new().unwrap();
        let path = write_temp_config(&dir, "config.toml", content);
        let before = fs::read(&path).unwrap();
        let result = load_file_config_from_candidates(Some(&path), &[]);
        assert!(
            result.is_err(),
            "execution typo must fail, got content: {content}"
        );
        let err = format!("{:#}", result.expect_err("must error"));
        assert_load_error_without_mutation(&path, &before, &err);
    }
}

#[test]
fn tool_routing_invalid_mode_is_error() {
    let dir = TempDir::new().unwrap();
    let content = "[tool_routing]\nmode = \"defered\"\n";
    let path = write_temp_config(&dir, "config.toml", content);
    let before = fs::read(&path).unwrap();
    let result = load_file_config_from_candidates(Some(&path), &[]);
    assert!(result.is_err(), "invalid tool_routing mode must fail");
    let err = format!("{:#}", result.expect_err("must error"));
    assert_load_error_without_mutation(&path, &before, &err);
}

#[test]
fn tool_routing_result_limit_out_of_range_is_error() {
    for limit in [0, 11] {
        let dir = TempDir::new().unwrap();
        let content = format!("[tool_routing]\nsearch_result_limit = {limit}\n");
        let path = write_temp_config(&dir, "config.toml", &content);
        let before = fs::read(&path).unwrap();
        let result = load_file_config_from_candidates(Some(&path), &[]);
        assert!(
            result.is_err(),
            "search_result_limit={limit} must fail, not clamp"
        );
        let err = format!("{:#}", result.expect_err("must error"));
        assert_load_error_without_mutation(&path, &before, &err);
    }
}

#[test]
fn context_budget_invalid_mode_is_error() {
    let dir = TempDir::new().unwrap();
    let content = "[context_budget]\nmode = \"atuo\"\n";
    let path = write_temp_config(&dir, "config.toml", content);
    let before = fs::read(&path).unwrap();
    let result = load_file_config_from_candidates(Some(&path), &[]);
    assert!(result.is_err(), "invalid context_budget mode must fail");
    let err = format!("{:#}", result.expect_err("must error"));
    assert_load_error_without_mutation(&path, &before, &err);
}

#[test]
fn reasoning_invalid_mode_is_error() {
    let dir = TempDir::new().unwrap();
    let content = "[reasoning]\nmode = \"atuo\"\n";
    let path = write_temp_config(&dir, "config.toml", content);
    let before = fs::read(&path).unwrap();
    let result = load_file_config_from_candidates(Some(&path), &[]);
    assert!(result.is_err(), "invalid reasoning mode must fail");
    let err = format!("{:#}", result.expect_err("must error"));
    assert_load_error_without_mutation(&path, &before, &err);
}

#[test]
fn reasoning_invalid_effort_is_error() {
    for field in [
        "initial_effort",
        "routine_effort",
        "deliberative_effort",
        "recovery_effort",
        "fixed_effort",
    ] {
        let dir = TempDir::new().unwrap();
        let content = format!("[reasoning]\n{field} = \"medum\"\n");
        let path = write_temp_config(&dir, "config.toml", &content);
        let before = fs::read(&path).unwrap();
        let result = load_file_config_from_candidates(Some(&path), &[]);
        assert!(result.is_err(), "invalid {field} must fail, not fallback");
        let err = format!("{:#}", result.expect_err("must error"));
        assert_load_error_without_mutation(&path, &before, &err);
    }
}

#[test]
fn watch_stale_keys_are_rejected() {
    let dir = TempDir::new().unwrap();
    let content = "[watch]\nenabled = true\ndebounce_ms = 500\npatterns = [\"*.rs\"]\n";
    let path = write_temp_config(&dir, "config.toml", content);
    let before = fs::read(&path).unwrap();
    let result = load_file_config_from_candidates(Some(&path), &[]);
    assert!(result.is_err(), "stale watch keys must fail");
    let err = format!("{:#}", result.expect_err("must error"));
    assert_load_error_without_mutation(&path, &before, &err);
}

#[test]
fn llm_unknown_key_is_error() {
    let dir = TempDir::new().unwrap();
    let content = "[llm]\nmax_retry = 3\n";
    let path = write_temp_config(&dir, "config.toml", content);
    let before = fs::read(&path).unwrap();
    let result = load_file_config_from_candidates(Some(&path), &[]);
    assert!(result.is_err(), "llm typo must fail");
    let err = format!("{:#}", result.expect_err("must error"));
    assert_load_error_without_mutation(&path, &before, &err);
}

#[test]
fn mcp_unknown_keys_are_rejected() {
    let dir = TempDir::new().unwrap();
    let outbound = "[[mcp_servers]]\nname = \"x\"\nenabled = true\ncall_timeout = 1000\n";
    let path = write_temp_config(&dir, "config.toml", outbound);
    let before = fs::read(&path).unwrap();
    let result = load_file_config_from_candidates(Some(&path), &[]);
    assert!(result.is_err(), "outbound MCP typo must fail");
    let err = format!("{:#}", result.expect_err("must error"));
    assert_load_error_without_mutation(&path, &before, &err);

    let dir = TempDir::new().unwrap();
    let local = "[mcp_server]\nenabled = false\nport = 8000\n";
    let path = write_temp_config(&dir, "config.toml", local);
    let before = fs::read(&path).unwrap();
    let result = load_file_config_from_candidates(Some(&path), &[]);
    assert!(result.is_err(), "local MCP typo must fail");
    let err = format!("{:#}", result.expect_err("must error"));
    assert_load_error_without_mutation(&path, &before, &err);
}

#[test]
fn mcp_env_arbitrary_keys_are_allowed() {
    let dir = TempDir::new().unwrap();
    let content = "[[mcp_servers]]\nname = \"x\"\nenabled = true\ntransport = \"stdio\"\ncommand = \"/bin/echo\"\n\n[mcp_servers.env]\nDOGE_CUSTOM_KEY = \"value\"\nANOTHER_KEY = \"another\"\n";
    let path = write_temp_config(&dir, "config.toml", content);
    let cfg = load_file_config_from_candidates(Some(&path), &[])
        .expect("MCP env arbitrary keys must parse");
    let servers = cfg.mcp_servers.expect("servers");
    assert_eq!(servers[0].env.as_ref().unwrap().len(), 2);
}

#[test]
fn committed_repo_config_parses_under_strict_schema() {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let before = fs::read(manifest_dir.join(".doge/config.toml")).expect("read repo config");
    let cfg = load_project_config(&manifest_dir).expect("repo config must parse");
    let after = fs::read(manifest_dir.join(".doge/config.toml")).expect("reread repo config");
    assert_eq!(before, after, "repo config test must not rewrite the file");
    let _ = cfg;
}

fn model_context_fixture(model: &str) -> (TempDir, AppConfig) {
    let root = TempDir::new().expect("tempdir");
    let cfg = AppConfig {
        model: model.into(),
        project_root: root.path().to_path_buf(),
        ..Default::default()
    };
    (root, cfg)
}

#[test]
fn verified_gpt_4_1_mini_aliases_and_snapshot_use_documented_capacity() {
    for model in [
        "gpt-4.1-mini",
        "gpt-4.1-mini-2025-04-14",
        "openai/gpt-4.1-mini",
        "openai/gpt-4.1-mini-2025-04-14",
    ] {
        let (_root, cfg) = model_context_fixture(model);
        assert_eq!(cfg.get_context_window_size(), Some(1_047_576), "{model}");
        assert_eq!(
            cfg.get_effective_compaction_limit(),
            250_000,
            "keep the configured threshold, not the full capacity"
        );
    }
}

#[test]
fn verified_model_does_not_compact_an_ordinary_calibrated_prompt() {
    use crate::llm::context_budget::{
        BudgetPressure, ContextBudgetGovernor, TokenEstimate, TokenEstimateSource,
    };
    let (_root, cfg) = model_context_fixture("openai/gpt-4.1-mini");
    let pressure = ContextBudgetGovernor::new(Default::default()).classify(
        TokenEstimate {
            prompt_tokens: 10_000,
            source: TokenEstimateSource::Calibrated,
        },
        u64::from(cfg.get_effective_compaction_limit()),
    );
    assert_eq!(pressure, BudgetPressure::Healthy);
    assert!(!crate::llm::context_budget::should_compact_for_pressure(
        pressure,
        TokenEstimateSource::Calibrated
    ));
}

#[test]
fn verified_model_capacity_keeps_explicit_context_and_threshold_overrides() {
    let (_root, mut cfg) = model_context_fixture("openai/gpt-4.1-mini");
    cfg.llm.context_window_size = Some(20_000);
    assert_eq!(cfg.get_context_window_size(), Some(20_000));
    assert_eq!(cfg.get_effective_compaction_limit(), 16_000);
    cfg.auto_compact_prompt_token_threshold_overrides
        .insert(cfg.model.clone(), 900);
    assert_eq!(cfg.get_effective_compaction_limit(), 900);
}

#[test]
fn context_catalog_does_not_expand_unknown_model_names_or_native_provider() {
    for model in [
        "acme/gpt-4.1-mini",
        "gpt-4.1-mini-future",
        "gpt-4.1-mini-2026-10-07",
        "prefix-gpt-4.1-mini",
        "OPENAI/GPT-4.1-MINI",
    ] {
        let (_root, cfg) = model_context_fixture(model);
        assert_eq!(
            cfg.get_context_window_size(),
            None,
            "unknown model metadata must remain unknown for {model}"
        );
    }
    let (_root, unknown) = model_context_fixture("unknown-private-model");
    assert_eq!(unknown.get_context_window_size(), None);
    assert_eq!(unknown.get_effective_compaction_limit(), 102_400);
    let (_root, mut native) = model_context_fixture("gpt-4.1-mini");
    native.provider = crate::features::openai_subscription::ProviderKind::Openai;
    assert_eq!(
        native.get_context_window_size(),
        None,
        "API metadata must not infer a subscription model's capacity"
    );
}

#[test]
fn capability_custom_endpoints_do_not_inherit_capacity() {
    for endpoint in [
        "https://custom.invalid/v1",
        "https://api.openai.com.example/v1",
    ] {
        let (_root, mut cfg) = model_context_fixture("gpt-4.1-mini");
        cfg.base_url = endpoint.into();
        assert_eq!(cfg.get_context_window_size(), None, "{endpoint}");
        cfg.llm.context_window_size = Some(20_000);
        assert_eq!(cfg.get_context_window_size(), Some(20_000));
    }
}

#[test]
fn capability_unknown_model_names_do_not_inherit_legacy_capacity() {
    for model in ["acme/gpt-4o", "prefix-gpt-4", "gpt-4.1-mini-future"] {
        let (_root, cfg) = model_context_fixture(model);
        assert_eq!(cfg.get_context_window_size(), None, "{model}");
    }
}

#[test]
fn capability_model_selection_preserves_overrides_and_compaction_limits() {
    let (_root, mut main) = model_context_fixture("gpt-4.1-mini");
    main.base_url = "https://custom.invalid/v1".into();
    main.llm.context_window_size = Some(20_000);
    main.auto_compact_prompt_token_threshold_overrides
        .insert("worker-private".into(), 900);
    let mut worker = main.clone();
    worker.model = "worker-private".into();
    assert_eq!(main.get_context_window_size(), Some(20_000));
    assert_eq!(worker.get_context_window_size(), Some(20_000));
    assert_eq!(main.get_effective_compaction_limit(), 16_000);
    assert_eq!(worker.get_effective_compaction_limit(), 900);
    assert_eq!(worker.base_url, main.base_url);
    assert_eq!(worker.provider, main.provider);
    assert_eq!(worker.reasoning.mode, main.reasoning.mode);
}

#[test]
fn capability_native_unknown_capacity_preserves_threshold_validation() {
    let (_root, mut cfg) = model_context_fixture("gpt-4.1-mini");
    cfg.provider = crate::features::openai_subscription::ProviderKind::Openai;
    assert_eq!(cfg.get_context_window_size(), None);
    assert_eq!(cfg.get_effective_compaction_limit(), 102_400);
    let client = crate::llm::OpenAIClient::new("http://127.0.0.1:1", "fixture").expect("client");
    let client = client
        .with_responses_compact_threshold(Some(cfg.get_effective_compaction_limit()))
        .expect("valid native fallback");
    assert_eq!(client.responses_compact_threshold(), Some(102_400));
    cfg.llm.context_window_size = Some(20_000);
    assert_eq!(cfg.get_effective_compaction_limit(), 16_000);
    cfg.auto_compact_prompt_token_threshold_overrides
        .insert(cfg.model.clone(), 999);
    assert!(
        client
            .with_responses_compact_threshold(Some(cfg.get_effective_compaction_limit()))
            .is_err()
    );
}
