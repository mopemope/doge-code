//! Project trust regressions use synthetic credentials and loopback only.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::thread;
use std::time::Duration;

struct Endpoint {
    url: String,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Endpoint {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback fixture");
        listener.set_nonblocking(true).expect("nonblocking");
        let url = format!("http://{}/v1", listener.local_addr().expect("address"));
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let count = requests.clone();
        let stopped = stop.clone();
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        count.fetch_add(1, Ordering::SeqCst);
                        stream.set_nonblocking(false).expect("blocking");
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .expect("timeout");
                        // Consume the framed request, discard headers/body (no key capture).
                        let mut bytes = Vec::new();
                        let mut buffer = [0; 4096];
                        while let Ok(n) = stream.read(&mut buffer) {
                            if n == 0 {
                                break;
                            }
                            bytes.extend_from_slice(&buffer[..n]);
                            if let Some(header_end) =
                                bytes.windows(4).position(|b| b == b"\r\n\r\n")
                            {
                                let header = String::from_utf8_lossy(&bytes[..header_end]);
                                let length = header
                                    .lines()
                                    .find_map(|line| {
                                        let (key, value) = line.split_once(':')?;
                                        key.eq_ignore_ascii_case("content-length")
                                            .then(|| value.trim().parse::<usize>().ok())
                                            .flatten()
                                    })
                                    .unwrap_or(0);
                                if bytes.len() >= header_end + 4 + length {
                                    break;
                                }
                            }
                        }
                        let body = r#"{"id":"fixture","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"fixture complete"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("accept fixture: {e}"),
                }
            }
        });
        Self {
            url,
            requests,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker
            .take()
            .expect("worker")
            .join()
            .expect("joined fixture");
    }
}
struct Fixture {
    root: tempfile::TempDir,
    global: std::path::PathBuf,
}
impl Fixture {
    fn new(global: &str, project: &str) -> Self {
        let root = tempfile::tempdir().expect("isolated config");
        std::fs::create_dir(root.path().join(".doge")).expect("project config dir");
        let file = root.path().join("global.toml");
        std::fs::write(&file, format!("model='fixture'\nmcp_servers=[]\n{global}\n[llm]\nmax_retries=0\nrequest_timeout_ms=2000\ntimeout_ms=2000\n")).expect("global fixture");
        std::fs::write(root.path().join(".doge/config.toml"), project).expect("project fixture");
        Self { root, global: file }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dgc"));
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("NO_PROXY", "*")
            .env("HOME", self.root.path())
            .env("XDG_CONFIG_HOME", self.root.path())
            .env("DOGE_CODE_CONFIG", &self.global)
            .current_dir(self.root.path())
            .arg("--no-repomap");
        command
    }
    fn run(&self, env: &[(&str, &str)], args: &[&str]) -> Output {
        self.command()
            .envs(env.iter().copied())
            .args(args)
            .args(["exec", "fixture request", "--json"])
            .output()
            .expect("isolated CLI")
    }
}
#[test]
fn project_trust_new_endpoint_cannot_receive_inherited_or_explicit_keys() {
    for origin in ["environment", "global", "cli", "project", "none"] {
        let endpoint = Endpoint::new();
        let global = if origin == "global" {
            "api_key='SYNTHETIC_GLOBAL_KEY'"
        } else {
            ""
        };
        let mut project = format!("base_url='{}'", endpoint.url);
        if origin == "project" {
            project += "\napi_key='SYNTHETIC_PROJECT_KEY'";
        }
        let fixture = Fixture::new(global, &project);
        let env = if origin == "environment" {
            vec![("OPENAI_API_KEY", "SYNTHETIC_ENV_KEY")]
        } else {
            vec![]
        };
        let args = if origin == "cli" {
            vec!["--api-key", "SYNTHETIC_CLI_KEY"]
        } else {
            vec![]
        };
        let output = fixture.run(&env, &args);
        assert!(!output.status.success(), "{origin}: {output:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("untrusted project configuration"),
            "{origin}: {error}"
        );
        assert!(error.contains("DOGE_CODE_CONFIG"), "{error}");
        assert!(!error.contains("SYNTHETIC_"));
        assert_eq!(
            endpoint.requests.load(Ordering::SeqCst),
            0,
            "{origin} must never send a request"
        );
    }
}
#[test]
fn project_trust_explicit_endpoints_and_identical_global_endpoint_work() {
    for origin in ["cli", "environment", "global-identical"] {
        let trusted = Endpoint::new();
        let untrusted = Endpoint::new();
        let project_url = if origin == "global-identical" {
            &trusted.url
        } else {
            &untrusted.url
        };
        let fixture = Fixture::new(
            &format!("base_url='{}'\napi_key='SYNTHETIC_GLOBAL_KEY'", trusted.url),
            &format!("base_url='{project_url}'"),
        );
        let args = if origin == "cli" {
            vec!["--base-url", trusted.url.as_str()]
        } else {
            vec![]
        };
        let env = if origin == "environment" {
            vec![("OPENAI_BASE_URL", trusted.url.as_str())]
        } else {
            vec![]
        };
        let output = fixture.run(&env, &args);
        assert!(output.status.success(), "{origin}: {output:?}");
        assert!(trusted.requests.load(Ordering::SeqCst) > 0);
        assert_eq!(untrusted.requests.load(Ordering::SeqCst), 0);
    }
}
#[test]
fn project_trust_global_restrictions_cannot_be_relaxed_at_startup() {
    for (global, project, expected) in [
        (
            "[execution]\nmode='deny'",
            "[execution]\nmode='unrestricted'",
            "execution.mode",
        ),
        (
            "[execution]\nallow_shell=false",
            "[execution]\nallow_shell=true",
            "execution.allow_shell",
        ),
        (
            "allowed_paths=[]",
            "allowed_paths=['/tmp']",
            "allowed_paths",
        ),
        (
            "",
            "[[mcp_servers]]\nname='project'\nenabled=true\naddress='http://127.0.0.1:1/mcp'",
            "mcp_servers",
        ),
        (
            "[mcp_server]\nenabled=false",
            "[mcp_server]\nenabled=true",
            "mcp_server",
        ),
    ] {
        let fixture = Fixture::new(global, project);
        let output = fixture
            .command()
            .arg("diagnostics")
            .output()
            .expect("diagnostics");
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("untrusted project configuration") && error.contains(expected),
            "{error}"
        );
    }
}

#[test]
fn project_trust_inference_origins_and_pinned_providers_are_preserved() {
    for (global, project, env, args, expected) in [
        (
            "base_url='http://127.0.0.1:1/v1'\napi_key='SYNTHETIC'",
            "",
            vec![],
            vec![],
            "provider=Default, endpoint=Global, API key=Global",
        ),
        (
            "",
            "",
            vec![
                ("OPENAI_BASE_URL", "http://127.0.0.1:1/v1"),
                ("OPENAI_API_KEY", "SYNTHETIC"),
            ],
            vec![],
            "provider=Default, endpoint=Environment, API key=Environment",
        ),
        (
            "",
            "",
            vec![],
            vec![
                "--provider",
                "openai-compatible",
                "--base-url",
                "http://127.0.0.1:1/v1",
                "--api-key",
                "SYNTHETIC",
            ],
            "provider=Cli, endpoint=Cli, API key=Cli",
        ),
        (
            "",
            "base_url='https://api.openai.com/v1'\napi_key='SYNTHETIC'",
            vec![],
            vec![],
            "provider=Default, endpoint=Project, API key=Project",
        ),
        (
            "provider='opencode-go'\nmodel='gpt-6-luna'",
            "base_url='http://127.0.0.1:1/v1'",
            vec![("OPENCODE_API_KEY", "SYNTHETIC")],
            vec![],
            "provider=Global, endpoint=Default, API key=Environment",
        ),
        (
            "provider='opencode-zen'\nmodel='gpt-6-luna'",
            "",
            vec![("OPENCODE_API_KEY", "SYNTHETIC")],
            vec![],
            "provider=Global, endpoint=Default, API key=Environment",
        ),
        (
            "provider='openai'\nmodel='gpt-6-luna'",
            "base_url='http://127.0.0.1:1/v1'",
            vec![("OPENAI_API_KEY", "SYNTHETIC")],
            vec![],
            "provider=Global, endpoint=Default, API key=none",
        ),
    ] {
        // Fixture supplies an explicit model; remove it for provider cases to
        // avoid duplicate TOML keys and retain the real route's model.
        let fixture = Fixture::new("", project);
        std::fs::write(
            &fixture.global,
            format!(
                "mcp_servers=[]\n{global}\n{}",
                if global.contains("model=") {
                    ""
                } else {
                    "model='fixture'"
                }
            ),
        )
        .expect("synthetic config");
        let output = fixture
            .command()
            .envs(env)
            .args(args)
            .arg("diagnostics")
            .output()
            .expect("diagnostics");
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains(expected), "{text}");
        assert!(!text.contains("SYNTHETIC"));
    }
}

#[test]
fn project_trust_repository_dotenv_cannot_select_endpoint_or_global_config() {
    let endpoint = Endpoint::new();
    let fixture = Fixture::new("", "");
    std::fs::write(
        fixture.root.path().join(".env"),
        format!("OPENAI_BASE_URL={}\n", endpoint.url),
    )
    .expect("project dotenv");
    // A dotenv-only redirect also cannot override an existing global endpoint.
    // Keep both destinations local so a regression never reaches a real API.
    let trusted = Endpoint::new();
    std::fs::write(
        &fixture.global,
        format!(
            "model='fixture'\nbase_url='{}'\n[llm]\nmax_retries=0\nrequest_timeout_ms=2000\n",
            trusted.url
        ),
    )
    .expect("trusted local endpoint");
    let output = fixture.run(&[("OPENAI_API_KEY", "SYNTHETIC_ENV_KEY")], &[]);
    assert!(output.status.success(), "{output:?}");
    assert!(trusted.requests.load(Ordering::SeqCst) > 0);
    assert_eq!(endpoint.requests.load(Ordering::SeqCst), 0);
    // A .doge endpoint reaches the guard even when dotenv tries to masquerade
    // as a higher-priority explicitly configured endpoint.
    std::fs::write(
        fixture.root.path().join(".doge/config.toml"),
        format!("base_url='{}'", endpoint.url),
    )
    .expect("project endpoint");
    let output = fixture.run(&[("OPENAI_API_KEY", "SYNTHETIC_ENV_KEY")], &[]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("untrusted project configuration"));
    assert_eq!(endpoint.requests.load(Ordering::SeqCst), 0);

    // Also cover the usual implicit XDG global path. Caller env has no
    // DOGE_CODE_CONFIG, so a project dotenv must not replace global policy.
    let user_dir = fixture.root.path().join("doge-code");
    std::fs::create_dir(&user_dir).expect("isolated XDG config");
    std::fs::write(user_dir.join("config.toml"), "[execution]\nmode='deny'")
        .expect("trusted global deny");
    std::fs::write(
        &fixture.global,
        "model='fixture'\n[execution]\nmode='unrestricted'",
    )
    .expect("untrusted replacement config");
    std::fs::write(
        fixture.root.path().join(".env"),
        format!("DOGE_CODE_CONFIG={}\n", fixture.global.display()),
    )
    .expect("project dotenv override");
    std::fs::write(
        fixture.root.path().join(".doge/config.toml"),
        "[execution]\nmode='unrestricted'",
    )
    .expect("project broaden");
    let output = fixture
        .command()
        .env_remove("DOGE_CODE_CONFIG")
        .arg("diagnostics")
        .output()
        .expect("diagnostics");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("execution.mode (global deny)"));
}
