//! Real CLI regressions. Only generated files and loopback HTTP; no provider keys.
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    stopped: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn new(script: impl Fn(usize, &Value) -> (u16, Value) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback fixture bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let url = format!("http://{}/v1", listener.local_addr().expect("address"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let captured = requests.clone();
        let stop = stopped.clone();
        let script = Arc::new(script);
        let worker = thread::spawn(move || {
            let mut handlers = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // BSD/macOS accept sockets inherit O_NONBLOCK. The
                        // fixture reads a whole framed request, including a
                        // body which may arrive after the headers.
                        stream
                            .set_nonblocking(false)
                            .expect("blocking fixture socket");
                        let script = script.clone();
                        let captured = captured.clone();
                        handlers.push(thread::spawn(move || {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .expect("timeout");
                        let Some(request) = read_request(&mut stream) else {
                            return;
                        };
                        let index = {
                            let mut requests = captured.lock().expect("requests");
                            requests.push(request.clone());
                            requests.len()
                        };
                        let (status, value) = script(index, &request);
                        let body = value.to_string();
                        let response = format!(
                            "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        // Cancellation may close the client while a fixture responds.
                        let _ = stream.write_all(response.as_bytes());
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            }
            for handler in handlers {
                handler.join().expect("HTTP handler");
            }
        });
        Self {
            url,
            requests,
            stopped,
            worker: Some(worker),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !thread::panicking() {
                result.expect("fixture worker");
            }
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Value> {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).ok()?;
        header.push(byte[0]);
        assert!(header.len() < 16_384, "bounded fixture header");
    }
    let header = String::from_utf8(header).expect("HTTP header UTF8");
    let size: usize = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().expect("length"))
        })
        .expect("Content-Length");
    assert!(size < 2_000_000, "bounded fixture body");
    let mut body = vec![0; size];
    stream.read_exact(&mut body).ok()?;
    Some(serde_json::from_slice(&body).expect("request JSON"))
}

fn response(content: &str, calls: Vec<Value>, reason: &str) -> (u16, Value) {
    (
        200,
        json!({"choices":[{"index":0,"finish_reason":reason,"message":{"role":"assistant","content":content,"tool_calls":calls}}]}),
    )
}

fn call(id: &str, tool: &str, arguments: Value) -> Value {
    json!({"id":id,"type":"function","function":{"name":tool,"arguments":arguments.to_string()}})
}

fn failure() -> (u16, Value) {
    (
        500,
        json!({"error":{"code":"fixture_failure","message":"intentional failure"}}),
    )
}

struct Project {
    _temp: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
}

impl Project {
    fn new(process: bool) -> Self {
        let temp = tempfile::tempdir().expect("project fixture");
        let root = temp.path().canonicalize().expect("canonical fixture");
        let config = root.join("fixture.toml");
        std::fs::write(&config, format!("[llm]\nmax_retries=0\nrequest_timeout_ms=5000\ntimeout_ms=5000\n[tool_routing]\nmode='eager'\n[execution]\nmode='{}'\nallow_shell=false\n", if process { "unrestricted" } else { "deny" })).expect("config");
        Self {
            _temp: temp,
            root,
            config,
        }
    }

    fn command(&self, server: &Server) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dgc"));
        command
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("NO_PROXY", "*")
            .env("DOGE_CODE_CONFIG", &self.config)
            .args([
                "--no-repomap",
                "--provider",
                "openai-compatible",
                "--model",
                "fixture",
                "--api-key",
                "fixture-only",
                "--base-url",
                &server.url,
                "exec",
                "fixture request",
                "--json",
            ]);
        command
    }
}

fn output_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "JSON {error}; stderr {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[cfg(unix)]
struct CleanupChild(Option<std::process::Child>);

#[cfg(unix)]
impl std::ops::Deref for CleanupChild {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("owned CLI")
    }
}

#[cfg(unix)]
impl std::ops::DerefMut for CleanupChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().expect("owned CLI")
    }
}

#[cfg(unix)]
impl CleanupChild {
    fn wait_with_output(mut self) -> std::io::Result<Output> {
        self.0.take().expect("owned CLI").wait_with_output()
    }
}

#[cfg(unix)]
impl Drop for CleanupChild {
    fn drop(&mut self) {
        let Some(child) = &mut self.0 else {
            return;
        };
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        // Every asynchronous fixture CLI starts its own process group. Give
        // dgc cooperative cancellation so it also reaps its managed groups.
        unsafe {
            libc::killpg(child.id() as i32, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if child.try_wait().ok().flatten().is_none() {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}

#[test]
fn fixture_accepts_a_body_arriving_after_headers() {
    let server = Server::new(|_, _| response("done", vec![], "stop"));
    let address = server
        .url
        .strip_prefix("http://")
        .expect("fixture URL")
        .strip_suffix("/v1")
        .expect("fixture path");
    let mut stream = TcpStream::connect(address).expect("fixture connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    let body = "{\"fixture\":true}";
    write!(
        stream,
        "POST /v1/chat/completions HTTP/1.1\r\nHost: fixture\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .expect("headers");
    thread::sleep(Duration::from_millis(100));
    stream.write_all(body.as_bytes()).expect("delayed body");
    let mut result = String::new();
    stream.read_to_string(&mut result).expect("response");
    assert!(result.starts_with("HTTP/1.1 200"));
    assert_eq!(
        server.requests.lock().expect("requests").as_slice(),
        &[json!({"fixture":true})]
    );
}

#[test]
fn api_error_is_nonzero_and_success_is_zero() {
    let project = Project::new(false);
    let server = Server::new(|index, _| {
        if index == 1 {
            failure()
        } else {
            response("done", vec![], "stop")
        }
    });
    let failed = project.command(&server).output().expect("CLI");
    assert!(!failed.status.success());
    assert_eq!(output_json(&failed)["success"], false);
    let success = project.command(&server).output().expect("CLI");
    assert!(success.status.success());
    assert_eq!(output_json(&success)["success"], true);
}

#[test]
fn failed_api_key_turn_resumes_exact_tool_execution_history() {
    let project = Project::new(false);
    let written = project.root.join("written.txt");
    let write_path = written.clone();
    let server = Server::new(move |index, _| match index {
        1 => response(
            "",
            vec![call(
                "write-once",
                "fs_write",
                json!({"path":write_path,"content":"side effect"}),
            )],
            "tool_calls",
        ),
        2 => failure(),
        _ => response("recovered", vec![], "stop"),
    });
    let first_result = project.command(&server).output().expect("first CLI");
    assert!(!first_result.status.success());
    assert!(
        written.exists(),
        "stdout:{} stderr:{} requests:{:?}",
        String::from_utf8_lossy(&first_result.stdout),
        String::from_utf8_lossy(&first_result.stderr),
        server.requests.lock().expect("requests")
    );
    assert_eq!(
        std::fs::read_to_string(written).expect("side effect"),
        "side effect"
    );
    let resumed = project
        .command(&server)
        .arg("--resume=latest")
        .output()
        .expect("resume");
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let requests = server.requests.lock().expect("requests");
    let messages = requests[2]["messages"].as_array().expect("messages");
    assert!(
        messages
            .iter()
            .any(|m| m["tool_calls"][0]["id"] == "write-once")
    );
    assert!(messages.iter().any(|m| {
        m["tool_call_id"] == "write-once"
            && m["content"]
                .as_str()
                .is_some_and(|s| s.contains("written.txt"))
    }));
    assert_eq!(
        messages
            .iter()
            .filter(|m| m["role"] == "user" && m["content"] == "fixture request")
            .count(),
        2
    );
}

#[test]
fn incomplete_or_invalid_tool_batch_never_writes() {
    for (reason, duplicate, refusal, role) in [
        ("length", false, false, "assistant"),
        ("content_filter", false, false, "assistant"),
        ("tool_calls", true, false, "assistant"),
        ("tool_calls", false, true, "assistant"),
        ("tool_calls", false, false, "user"),
    ] {
        let project = Project::new(false);
        let target = project.root.join("must-not-exist.txt");
        let write_path = target.clone();
        let server = Server::new(move |_, _| {
            let first = call(
                "mutation",
                "fs_write",
                json!({"path":write_path,"content":"rejected"}),
            );
            let calls = if duplicate {
                vec![first.clone(), first]
            } else {
                vec![first]
            };
            let (status, mut body) = response("", calls, reason);
            body["choices"][0]["message"]["role"] = json!(role);
            if refusal {
                body["choices"][0]["message"]["refusal"] = json!("declined");
            }
            (status, body)
        });
        let result = project.command(&server).output().expect("CLI");
        assert!(
            !result.status.success(),
            "{reason} duplicate:{duplicate} refusal:{refusal}"
        );
        assert!(!target.exists());
        assert_eq!(
            server.requests.lock().expect("requests").len(),
            1,
            "no automatic retry"
        );
    }
}

#[test]
fn malformed_or_unadvertised_sibling_never_executes_a_valid_write_prefix() {
    for malformed in [
        json!({}),
        json!({"content":null}),
        json!({"content":42}),
        json!({"content":"valid","unknown_tool":true}),
    ] {
        let project = Project::new(false);
        let existing = project.root.join("keep.txt");
        let prefix = project.root.join("must-not-exist.txt");
        std::fs::write(&existing, "KEEP USER CONTENT\n").expect("original");
        let target = existing.clone();
        let first = prefix.clone();
        let server = Server::new(move |_, _| {
            let mut arguments = malformed.clone();
            arguments["path"] = json!(target);
            let name = if arguments.get("unknown_tool").is_some() {
                "unadvertised_tool"
            } else {
                "fs_write"
            };
            response(
                "",
                vec![
                    call(
                        "valid-prefix",
                        "fs_write",
                        json!({"path":first,"content":"must not be written"}),
                    ),
                    call("invalid-sibling", name, arguments),
                ],
                "tool_calls",
            )
        });
        let result = project.command(&server).output().expect("CLI");
        assert!(!result.status.success());
        assert_eq!(output_json(&result)["success"], false);
        assert!(
            !prefix.exists(),
            "preflight must reject before the valid prefix"
        );
        assert_eq!(
            std::fs::read_to_string(&existing).expect("original"),
            "KEEP USER CONTENT\n"
        );
        assert_eq!(server.requests.lock().expect("requests").len(), 1);
    }
}

#[test]
fn reactive_compaction_retains_project_authority_and_unseen_result() {
    let project = Project::new(false);
    let sentinel = "PROJECT_RULE_SENTINEL_MUST_SURVIVE";
    std::fs::write(project.root.join("AGENTS.md"), sentinel).expect("instructions");
    std::fs::write(project.root.join("read.txt"), "unseen-exact-result").expect("read fixture");
    std::fs::write(project.root.join("seen.txt"), "previously-seen-result").expect("seen fixture");
    let read_path = project.root.join("read.txt");
    let seen_path = project.root.join("seen.txt");
    let server = Server::new(move |index, request| {
        if !request
            .get("tools")
            .is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty()))
        {
            return response("compacted research", vec![], "stop");
        }
        match index {
            1 => response(
                "",
                vec![call("seen", "fs_read", json!({"path":seen_path}))],
                "tool_calls",
            ),
            2 => response(
                "",
                vec![call("read", "fs_read", json!({"path":read_path}))],
                "tool_calls",
            ),
            3 => (
                400,
                json!({"error":{"code":"context_length_exceeded","message":"fixture overflow"}}),
            ),
            _ => response("done", vec![], "stop"),
        }
    });
    let result = project.command(&server).output().expect("CLI");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let requests = server.requests.lock().expect("requests");
    assert_eq!(requests.len(), 5);
    let authority = requests[0]["messages"][0]["content"]
        .as_str()
        .expect("authority");
    assert!(authority.len() > 4000);
    assert!(authority.contains(sentinel));
    let retry = &requests[4];
    assert_eq!(retry["messages"][0]["content"], authority);
    assert!(
        retry["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .any(|m| m["tool_call_id"] == "read"
                && m["content"]
                    .as_str()
                    .is_some_and(|s| s.contains("unseen-exact-result")))
    );
}

#[cfg(unix)]
#[test]
fn cli_cancellation_during_compaction_does_not_wait_for_provider_timeout() {
    use std::os::unix::process::CommandExt;
    let project = Project::new(false);
    let root = project.root.clone();
    std::fs::write(root.join("seen.txt"), "seen").expect("seen");
    std::fs::write(root.join("unseen.txt"), "unseen evidence must survive").expect("unseen");
    let summarizing = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let ready = summarizing.clone();
    let released = release.clone();
    let server = Server::new(move |index, request| {
        if !request
            .get("tools")
            .is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty()))
        {
            ready.store(true, Ordering::Relaxed);
            while !released.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(5));
            }
            return response("summary", vec![], "stop");
        }
        match index {
            1 | 2 => response(
                "",
                vec![call(
                    if index == 1 { "seen" } else { "unseen" },
                    "fs_read",
                    json!({"path":root.join(if index==1 {"seen.txt"} else {"unseen.txt"})}),
                )],
                "tool_calls",
            ),
            _ => (
                400,
                json!({"error":{"code":"context_length_exceeded","message":"fixture overflow"}}),
            ),
        }
    });
    struct Release(Arc<AtomicBool>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let _release = Release(release.clone());
    let mut command = project.command(&server);
    command
        .process_group(0)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = CleanupChild(Some(command.spawn().expect("CLI")));
    let deadline = Instant::now() + Duration::from_secs(8);
    while !summarizing.load(Ordering::Relaxed)
        && child.try_wait().expect("CLI startup wait").is_none()
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(5));
    }
    if !summarizing.load(Ordering::Relaxed) {
        let _ = child.kill();
        let result = child.wait_with_output().expect("CLI startup output");
        panic!(
            "summary request did not start; status:{} stdout:{} stderr:{} requests:{:?}",
            result.status,
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr),
            server.requests.lock().expect("requests")
        );
    }
    assert_eq!(unsafe { libc::killpg(child.id() as i32, libc::SIGINT) }, 0);
    let started = Instant::now();
    while child.try_wait().expect("wait").is_none() && started.elapsed() < Duration::from_secs(2) {
        thread::sleep(Duration::from_millis(5));
    }
    let prompt_exit = child.try_wait().expect("wait").is_some();
    if !prompt_exit {
        let _ = child.kill();
    }
    let result = child.wait_with_output().expect("output");
    release.store(true, Ordering::Relaxed);
    println!(
        "compaction cancelled promptly: {prompt_exit}; signal-to-exit ms: {}",
        started.elapsed().as_millis()
    );
    assert!(
        prompt_exit,
        "cancel must not wait for the 5s provider timeout"
    );
    assert!(!result.status.success());
    assert_eq!(output_json(&result)["success"], false);
}

#[cfg(unix)]
#[test]
fn cli_signals_reap_managed_process_and_preserve_unknown_outcome() {
    use std::os::unix::process::CommandExt;
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let project = Project::new(true);
        let pid_path = project.root.join("child.pid");
        let root = project.root.clone();
        let pid_file = pid_path.clone();
        let script = format!(
            "sleep 30 & echo $$ > '{}.tmp'; /bin/mv '{}.tmp' '{}'; wait",
            pid_file.display(),
            pid_file.display(),
            pid_file.display()
        );
        let server = Server::new(move |_, _| {
            response(
                "",
                vec![call(
                    "process",
                    "execute_process",
                    json!({"program":"/bin/sh","args":["-c",script],"cwd":root}),
                )],
                "tool_calls",
            )
        });
        let mut command = project.command(&server);
        command
            .process_group(0)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = CleanupChild(Some(command.spawn().expect("CLI spawn")));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !pid_path.exists()
            && child.try_wait().expect("CLI startup wait").is_none()
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(10));
        }
        if !pid_path.exists() {
            let _ = child.kill();
            let result = child.wait_with_output().expect("CLI startup output");
            panic!(
                "managed child did not start; status:{} stdout:{} stderr:{} requests:{:?}",
                result.status,
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr),
                server.requests.lock().expect("requests")
            );
        }
        let pid: i32 = std::fs::read_to_string(&pid_path)
            .expect("pid file")
            .trim()
            .parse()
            .expect("pid");
        struct Cleanup(i32);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                unsafe {
                    libc::killpg(self.0, libc::SIGKILL);
                }
            }
        }
        let _cleanup = Cleanup(pid);
        let started = Instant::now();
        assert_eq!(unsafe { libc::killpg(child.id() as i32, signal) }, 0);
        while child.try_wait().expect("CLI wait").is_none()
            && started.elapsed() < Duration::from_secs(5)
        {
            thread::sleep(Duration::from_millis(10));
        }
        if child.try_wait().expect("CLI wait").is_none() {
            let _ = child.kill();
            panic!("CLI cancellation cleanup exceeded 5s");
        }
        let result = child.wait_with_output().expect("CLI output");
        assert!(!result.status.success());
        assert_eq!(output_json(&result)["success"], false);
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "managed child still exists"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        // Check persisted interruption evidence without starting another model/tool loop.
        let sessions = std::fs::read_dir(project.root.join(".doge/sessions")).expect("sessions");
        let mut retained = false;
        for entry in sessions {
            let entry = entry.expect("entry");
            let p = entry.path().join("session.json");
            if let Ok(text) = std::fs::read_to_string(p) {
                retained |= text.contains("outcome unknown");
            }
        }
        assert!(retained, "interrupted call must be durable");
    }
}
