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
        assert!(text.contains("offline snapshot 2026-10-17"));
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
fn model_catalog_cli_warns_for_free_models_but_keeps_supported_ones_selectable() {
    for model in [
        "glm-5.3-flash-free",
        "big-pickle",
        "muse-spark-1.3-contributor-free",
    ] {
        let output = models("opencode-zen", Some(model));
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("Free-tier access"), "{text}");
        assert!(text.contains("403 FreeTierError"), "{text}");
        assert!(
            text.contains("adapter support does not guarantee access"),
            "{text}"
        );
    }
    let output = models("opencode-zen", Some("glm-5.3-flash"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Free-tier access"));
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

/// Blank values mean "unset" for every source, so they cannot shadow a
/// lower-precedence value or fail startup on their own.
#[test]
fn blank_environment_and_file_values_are_treated_as_unset() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("fixture.toml");
    std::fs::write(
        &config,
        "provider = \"opencode-go\"\nmodel = \"gpt-6-luna\"\napi_key = \"  \"\nmcp_servers = []\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dgc"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path())
        .env("DOGE_CODE_CONFIG", &config)
        .env("OPENCODE_BASE_URL", "")
        .env("OPENCODE_MODEL", " ")
        .env("OPENCODE_API_KEY", "")
        .current_dir(dir.path())
        .args(["--api-key", "", "diagnostics"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("gpt-6-luna / NO KEY"), "{text}");
    assert!(text.contains("API key=none"), "{text}");
    assert!(text.contains("endpoint=Default"), "{text}");
}

fn openrouter_models(arguments: &[&str], cached: bool) -> Output {
    let dir = tempfile::tempdir().expect("isolated CLI root");
    let config = dir.path().join("invalid.toml");
    let invalid = "invalid TOML [ SYNTHETIC_PRIVATE_CONFIG";
    std::fs::write(&config, invalid).unwrap();
    if cached {
        let path = dir.path().join("doge-code/openrouter/models-v1.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::json!({
            "schema_version":1,"source":"https://openrouter.ai/api/v1/models", "fetched_at":chrono::Utc::now(),"excluded_entries":0,
            "models":[{"id":"Publisher/Model:free","name":"Fixture model", "input_modalities":["text"], "output_modalities":["text"],"supported_parameters":["tools"],"context_window":8192,
            "prices":{"prompt":null,"completion":null,"request":null,"conditional":null},"reasoning":{"supported_efforts":"Unknown","mandatory":null}}]
        }).to_string()).unwrap();
    }
    let out = Command::new(env!("CARGO_BIN_EXE_dgc"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path())
        .env("XDG_CACHE_HOME", dir.path())
        .env("DOGE_CODE_CONFIG", &config)
        .env("OPENAI_API_KEY", "SYNTHETIC_OPENAI_PRIVATE")
        .env("OPENCODE_API_KEY", "SYNTHETIC_OPENCODE_PRIVATE")
        .current_dir(dir.path())
        .args(arguments)
        .output()
        .unwrap();
    assert_eq!(std::fs::read_to_string(&config).unwrap(), invalid);
    for secret in [
        "SYNTHETIC_PRIVATE_CONFIG",
        "SYNTHETIC_OPENAI_PRIVATE",
        "SYNTHETIC_OPENCODE_PRIVATE",
    ] {
        assert!(!String::from_utf8_lossy(&out.stdout).contains(secret));
        assert!(!String::from_utf8_lossy(&out.stderr).contains(secret));
    }
    out
}
#[test]
fn openrouter_model_catalog_cli_is_keyless_and_config_independent() {
    let output = openrouter_models(&["models", "--provider", "openrouter", "--offline"], true);
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("Publisher/Model:free"));
    assert!(text.contains("offline"));
}
#[test]
fn openrouter_model_catalog_cli_flags_and_exact_lookup() {
    for args in [
        vec![
            "models",
            "--provider",
            "openrouter",
            "--offline",
            "--refresh",
        ],
        vec!["models", "--provider", "go", "--offline"],
        vec!["models", "--provider", "zen", "--refresh"],
    ] {
        assert!(!openrouter_models(&args, true).status.success());
    }
    assert!(
        openrouter_models(
            &[
                "models",
                "--provider",
                "openrouter",
                "--offline",
                "--model",
                "Publisher/Model:free"
            ],
            true
        )
        .status
        .success()
    );
    assert!(
        !openrouter_models(
            &[
                "models",
                "--provider",
                "openrouter",
                "--offline",
                "--model",
                "publisher/model:free"
            ],
            true
        )
        .status
        .success()
    );
    assert!(
        !openrouter_models(
            &[
                "models",
                "--provider",
                "openrouter",
                "--offline",
                "--model",
                "Publisher/Model"
            ],
            true
        )
        .status
        .success()
    );
    assert!(
        !openrouter_models(&["models", "--provider", "openrouter", "--offline"], false)
            .status
            .success()
    );
}
#[test]
fn openrouter_model_catalog_cli_help_explains_scope_and_selection() {
    let out = openrouter_models(&["models", "--help"], false);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for term in ["openrouter", "--offline", "--refresh", "--model"] {
        assert!(text.contains(term), "{text}");
    }
}
