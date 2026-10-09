//! Catalog inspection is offline even with invalid config and no credentials.
use std::process::{Command, Output};
fn models(provider: &str, selection: Option<&str>) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("invalid.toml");
    std::fs::write(&config, "invalid TOML [ SECRET_FIXTURE_KEY").unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dgc"));
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path())
        .env("DOGE_CODE_CONFIG", &config)
        .current_dir(dir.path())
        .args(["models", "--provider", provider]);
    if let Some(model) = selection {
        cmd.args(["--model", model]);
    }
    let output = cmd.output().unwrap();
    assert_eq!(
        std::fs::read_to_string(config).unwrap(),
        "invalid TOML [ SECRET_FIXTURE_KEY"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("SECRET_FIXTURE_KEY"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("SECRET_FIXTURE_KEY"));
    output
}
#[test]
fn model_catalog_cli_lists_supported_and_unsupported_without_config_or_key() {
    for provider in ["opencode-go", "opencode-zen"] {
        let output = models(provider, None);
        assert!(output.status.success(), "{:?}", output);
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("offline snapshot 2026-10-09"));
        assert!(text.contains("gpt-6-luna | Responses | yes"));
        assert!(text.contains("claude-haiku-5-5 | Messages | no"));
        assert!(text.contains("availability and account access are not checked"));
    }
}
#[test]
fn model_catalog_cli_alias_unknown_and_provider_specific_adapter() {
    let output = models("opencode-go", Some("opencode-go/gpt-6-luna"));
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Canonical model ID: gpt-6-luna"));
    let output = models("opencode-go", Some("qwen3.8-max"));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("qwen3.8-max | Messages | no"));
    assert!(models("opencode-zen", Some("qwen3.8-max")).status.success());
    let output = models("opencode-go", Some("unknown-model"));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Unknown OpenCode model"));
    assert!(
        !models("opencode-go", Some("opencode/gpt-6-luna"))
            .status
            .success()
    );
}

#[test]
fn model_diagnostics_cli_is_keyless_and_reports_unknown_or_manual_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("fixture.toml");
    for (context, expected) in [
        ("", "Context capacity: unknown"),
        (
            "[llm]\ncontext_window_size = 20000\n",
            "20000 tokens (manual override",
        ),
    ] {
        std::fs::write(
            &config,
            format!(
                "provider = \"opencode-go\"\nmodel = \"gpt-6-luna\"\nmcp_servers = []\n{context}"
            ),
        )
        .unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_dgc"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", dir.path())
            .env("XDG_CONFIG_HOME", dir.path())
            .env("DOGE_CODE_CONFIG", &config)
            .current_dir(dir.path())
            .arg("diagnostics")
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains(expected), "{text}");
        assert!(text.contains("NO KEY"));
        assert!(text.contains("API: Responses"));
        assert!(text.contains("Reasoning: mode=auto"));
        if context.is_empty() {
            assert!(text.contains("effective 102400 tokens"));
        } else {
            assert!(text.contains("effective 16000 tokens"));
        }
    }
}
