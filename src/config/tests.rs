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
        mode: Some("deferred".to_string()),
        search_result_limit: Some(3),
    };
    let project = PartialToolRoutingConfig {
        mode: Some("eager".to_string()),
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
    assert_eq!(routing.mode.as_deref(), Some("deferred"));
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
        mode: Some("fixed".to_string()),
        routine_effort: Some("low".to_string()),
        fixed_effort: Some("high".to_string()),
        ..Default::default()
    };
    let project = PartialReasoningConfig {
        mode: Some("off".to_string()),
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
    assert_eq!(reasoning.mode.as_deref(), Some("fixed"));
    assert_eq!(reasoning.fixed_effort.as_deref(), Some("high"));
    let resolved = merge_reasoning(None, Some(&reasoning));
    assert_eq!(resolved.mode, ReasoningMode::Fixed);
    assert_eq!(resolved.fixed_effort, ReasoningEffort::High);
    assert_eq!(resolved.routine_effort, ReasoningEffort::Low);
}

#[test]
fn test_file_config_ignores_unknown_fields() {
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
        config.is_ok(),
        "Should parse successfully confirming unknown fields are ignored"
    );
    let config = config.unwrap();
    assert_eq!(config.llm.unwrap().connect_timeout_ms, Some(1000));
}

#[test]
fn test_context_budget_merge_project_wins() {
    let file = PartialContextBudgetConfig {
        mode: Some("observe".to_string()),
    };
    let project = PartialContextBudgetConfig {
        mode: Some("off".to_string()),
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
    assert_eq!(budget.mode.as_deref(), Some("observe"));
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
