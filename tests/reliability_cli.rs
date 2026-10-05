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
        self.command_prompt(server, "fixture request")
    }

    fn command_prompt(&self, server: &Server, prompt: &str) -> Command {
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
                prompt,
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
        let server = Server::new(move |index, request| {
            if index > 1 {
                assert_tool_result_blocks(request["messages"].as_array().unwrap());
                return response("cancelled batch retained", vec![], "stop");
            }
            response(
                "",
                vec![
                    call(
                        "before-process",
                        "fs_read",
                        json!({"path":root.join("fixture.toml")}),
                    ),
                    call(
                        "process",
                        "execute_process",
                        json!({"program":"/bin/sh","args":["-c",script],"cwd":root}),
                    ),
                    call(
                        "unstarted-write",
                        "fs_write",
                        json!({"path":root.join("must-not-run"),"content":"unexpected"}),
                    ),
                ],
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
        let checkpoint = std::fs::read_dir(project.root.join(".doge/sessions"))
            .unwrap()
            .map(|e| e.unwrap().path().join("session.json"))
            .find(|p| p.is_file())
            .unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(checkpoint).unwrap()).unwrap();
        let messages = saved["conversation"].as_array().unwrap();
        assert_tool_result_blocks(messages);
        assert_eq!(
            messages
                .iter()
                .filter(|m| m["role"] == "tool"
                    && m["content"]
                        .as_str()
                        .is_some_and(|c| c.contains("outcome unknown")))
                .count(),
            2
        );
        assert!(
            messages
                .iter()
                .any(|m| m["tool_call_id"] == "before-process"
                    && !m["content"].as_str().unwrap().contains("outcome unknown"))
        );
        assert!(!project.root.join("must-not-run").exists());
        let resumed = project
            .command(&server)
            .arg("--resume=latest")
            .output()
            .unwrap();
        assert!(
            resumed.status.success(),
            "{}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        assert!(!project.root.join("must-not-run").exists());
    }
}

fn response_with_usage(content: &str, calls: Vec<Value>, reason: &str) -> (u16, Value) {
    let (status, mut value) = response(content, calls, reason);
    value["usage"] = json!({"prompt_tokens":100,"completion_tokens":50,"total_tokens":150,"prompt_tokens_details":{"cached_tokens":0}});
    (status, value)
}

#[test]
fn reported_usage_is_per_run_and_once_per_saved_session_across_resume() {
    let project = Project::new(false);
    std::fs::write(project.root.join("notes.txt"), "fixture").expect("file");
    let server = Server::new(|n, _| {
        if n == 1 {
            response_with_usage(
                "",
                vec![call("read", "fs_read", json!({"path":"notes.txt"}))],
                "tool_calls",
            )
        } else {
            response_with_usage("done", vec![], "stop")
        }
    });
    let first = project.command(&server).output().expect("first");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_json = output_json(&first);
    assert_eq!(first_json["tokens_used"], 200);
    assert_eq!(first_json["usage"]["total_tokens"], 300);
    assert_eq!(first_json["usage"]["attempts"], 2);
    assert_eq!(first_json["usage"]["cached_tokens"], 0);
    assert_eq!(first_json["usage"]["all_tracked_attempts_reported"], true);
    let resumed = project
        .command(&server)
        .arg("--resume=latest")
        .output()
        .expect("resume");
    assert!(resumed.status.success());
    assert_eq!(output_json(&resumed)["usage"]["total_tokens"], 150);
    let files: Vec<_> = std::fs::read_dir(project.root.join(".doge/sessions"))
        .expect("sessions")
        .map(|entry| entry.expect("entry").path().join("session.json"))
        .filter(|path| path.is_file())
        .collect();
    assert_eq!(files.len(), 1);
    let saved: Value =
        serde_json::from_slice(&std::fs::read(&files[0]).expect("session")).expect("JSON");
    assert_eq!(saved["token_count"], 450);
    assert_eq!(saved["requests"], 3);
    assert_eq!(saved["usage"]["total_tokens"], 450);
    assert_eq!(saved["usage"]["attempts"], 3);
    assert_eq!(saved["usage"]["usage_records"], 3);
}

#[test]
fn failed_attempt_has_unknown_usage_and_invalid_completion_keeps_reported_usage() {
    for reported in [false, true] {
        let project = Project::new(false);
        let server = Server::new(move |_, _| {
            if reported {
                response_with_usage("truncated", vec![], "length")
            } else {
                failure()
            }
        });
        let output = project.command(&server).output().expect("CLI");
        assert!(!output.status.success());
        let result = output_json(&output);
        assert_eq!(result["usage"]["attempts"], 1);
        assert_eq!(result["usage"]["usage_records"], u64::from(reported));
        assert_eq!(
            result["usage"]["unknown_usage_attempts"],
            u64::from(!reported)
        );
        assert_eq!(
            result["usage"]["total_tokens"],
            if reported { 150 } else { 0 }
        );
        let file = std::fs::read_dir(project.root.join(".doge/sessions"))
            .expect("sessions")
            .map(|entry| entry.expect("entry").path().join("session.json"))
            .find(|path| path.is_file())
            .expect("one checkpoint");
        let saved: Value =
            serde_json::from_slice(&std::fs::read(file).expect("session")).expect("JSON");
        assert_eq!(saved["usage"]["attempts"], 1);
        assert_eq!(saved["usage"]["usage_records"], u64::from(reported));
    }
}

#[test]
fn retry_tracks_unreported_failed_attempt_separately_from_success() {
    let project = Project::new(false);
    let config = std::fs::read_to_string(&project.config)
        .expect("config")
        .replace("max_retries=0", "max_retries=1");
    std::fs::write(&project.config, config).expect("retry config");
    let server = Server::new(|n, _| {
        if n == 1 {
            failure()
        } else {
            response_with_usage("done", vec![], "stop")
        }
    });
    let output = project.command(&server).output().expect("CLI");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = output_json(&output);
    assert_eq!(result["usage"]["attempts"], 2);
    assert_eq!(result["usage"]["usage_records"], 1);
    assert_eq!(result["usage"]["unknown_usage_attempts"], 1);
    assert_eq!(result["usage"]["all_tracked_attempts_reported"], false);
    assert_eq!(result["usage"]["total_tokens"], 150);
}

#[cfg(unix)]
#[test]
fn failed_checkpoint_preserves_existing_session_bytes_in_real_cli() {
    use std::os::unix::process::CommandExt;
    let project = Project::new(false);
    let server = Server::new(|_, _| {
        response_with_usage(&format!("done {}", "x".repeat(20_000)), vec![], "stop")
    });
    let first = project.command(&server).output().expect("first");
    assert!(first.status.success());
    let file = std::fs::read_dir(project.root.join(".doge/sessions"))
        .expect("sessions")
        .map(|entry| entry.expect("entry").path().join("session.json"))
        .find(|path| path.is_file())
        .expect("one checkpoint");
    let before = std::fs::read(&file).expect("before");
    let saved: Value = serde_json::from_slice(&before).expect("saved");
    // The old payload greatly exceeds the limit; timestamp/provenance size
    // fluctuations cannot allow an earlier checkpoint to succeed.
    let limit = (before.len() / 2) as libc::rlim_t;
    let mut command = project.command_prompt(
        &server,
        &format!("larger checkpoint {}", "x".repeat(20_000)),
    );
    command.arg(format!(
        "--resume={}",
        saved["meta"]["id"].as_str().expect("id")
    ));
    unsafe {
        command.pre_exec(move || {
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            let limits = libc::rlimit {
                rlim_cur: limit,
                rlim_max: limit,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limits) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = command.output().expect("limited child");
    assert!(!output.status.success());
    assert!(
        std::fs::read(&file).expect("after") == before,
        "old checkpoint bytes retained"
    );
    assert!(output_json(&output)["error"].as_str().is_some());
    assert!(
        !project.root.join(".doge/sessions/.recovery").exists(),
        "ordinary filesystem failures must not trigger automatic recovery"
    );
    assert_eq!(
        server.requests.lock().expect("requests").len(),
        1,
        "failed checkpoint did not send a new inference request"
    );
}

#[cfg(unix)]
#[test]
fn lease_independent_cli_processes_exclude_stale_writers_and_release_on_exit() {
    use std::process::Stdio;
    let project = Project::new(false);
    let release = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(AtomicBool::new(false));
    let gate = release.clone();
    let arrived = ready.clone();
    let server = Server::new(move |_, request| {
        let holding = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["content"] == "LEASE_HOLD" || m["content"] == "LEASE_KILL");
        let last_user = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|m| m["role"] == "user")
            .unwrap();
        if holding && (last_user["content"] == "LEASE_HOLD" || last_user["content"] == "LEASE_KILL")
        {
            arrived.store(true, Ordering::SeqCst);
            let until = Instant::now() + Duration::from_secs(8);
            while !gate.load(Ordering::SeqCst) && Instant::now() < until {
                thread::sleep(Duration::from_millis(5));
            }
        }
        response("done", vec![], "stop")
    });
    let seed = project
        .command_prompt(&server, "LEASE_SEED")
        .output()
        .unwrap();
    assert!(seed.status.success());
    for (prompt, force) in [("LEASE_HOLD", false), ("LEASE_KILL", true)] {
        release.store(false, Ordering::SeqCst);
        ready.store(false, Ordering::SeqCst);
        let mut first = CleanupChild(Some(
            project
                .command_prompt(&server, prompt)
                .arg("--resume=latest")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        ));
        let until = Instant::now() + Duration::from_secs(4);
        while !ready.load(Ordering::SeqCst) && Instant::now() < until {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            ready.load(Ordering::SeqCst),
            "first process reached provider with lease"
        );
        let before = server.requests.lock().unwrap().len();
        let second = project
            .command_prompt(&server, "MUST_NOT_RUN")
            .arg("--resume=latest")
            .output()
            .unwrap();
        assert!(!second.status.success());
        assert!(String::from_utf8_lossy(&second.stderr).contains("already in use"));
        assert_eq!(
            server.requests.lock().unwrap().len(),
            before,
            "busy process cannot call provider"
        );
        assert!(first.try_wait().unwrap().is_none());
        if force {
            first.kill().unwrap();
            assert!(!first.wait_with_output().unwrap().status.success());
            release.store(true, Ordering::SeqCst);
        } else {
            release.store(true, Ordering::SeqCst);
            assert!(first.wait_with_output().unwrap().status.success());
        }
        let retry = project
            .command_prompt(&server, "AFTER_OWNER_EXIT")
            .arg("--resume=latest")
            .output()
            .unwrap();
        assert!(
            retry.status.success(),
            "{}",
            String::from_utf8_lossy(&retry.stderr)
        );
        let requests = server.requests.lock().unwrap();
        let messages = requests.last().unwrap()["messages"].as_array().unwrap();
        assert!(messages.iter().any(|m| m["content"] == "LEASE_SEED"));
        assert!(messages.iter().any(|m| m["content"] == prompt));
    }
}

#[test]
fn capacity_exit_exports_complete_oversized_response_and_retains_checkpoint() {
    let project = Project::new(false);
    let response_body = format!("{}CAPACITY_END", "x".repeat(17 * 1024 * 1024));
    let expected = response_body.clone();
    let before_response = Arc::new(Mutex::new(None));
    let captured_checkpoint = before_response.clone();
    let fixture_store = project.root.join(".doge/sessions");
    let server = Server::new(move |n, _| {
        if n == 2 {
            let path = std::fs::read_dir(&fixture_store)
                .expect("store")
                .map(|entry| entry.expect("entry").path().join("session.json"))
                .find(|path| path.is_file())
                .expect("checkpoint before response");
            *captured_checkpoint.lock().expect("checkpoint") =
                Some(std::fs::read(path).expect("bytes"));
        }
        response_with_usage(if n == 1 { "seed" } else { &response_body }, vec![], "stop")
    });
    let first = project.command(&server).output().expect("seed");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let store = project.root.join(".doge/sessions");
    let checkpoint = std::fs::read_dir(&store)
        .expect("sessions")
        .map(|entry| entry.expect("entry").path().join("session.json"))
        .find(|path| path.is_file())
        .expect("checkpoint");

    let resumed = project
        .command_prompt(&server, "oversized response")
        .arg("--resume=latest")
        .output()
        .expect("oversized run");
    assert!(!resumed.status.success());
    // The new prompt checkpoint is saved before inference. The failed final
    // save must preserve those exact bytes, including that latest prompt.
    assert_eq!(
        std::fs::read(&checkpoint).expect("retained"),
        before_response
            .lock()
            .expect("checkpoint")
            .as_ref()
            .expect("captured")
            .clone()
    );
    let recoveries: Vec<_> = std::fs::read_dir(store.join(".recovery"))
        .expect("recovery")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert_eq!(recoveries.len(), 1);
    let recovered: Value =
        serde_json::from_slice(&std::fs::read(&recoveries[0]).expect("recovery bytes"))
            .expect("envelope");
    assert_eq!(recovered["version"], 1);
    let session = &recovered["session"];
    assert_eq!(recovered["source_session_id"], session["meta"]["id"]);
    assert!(
        session["conversation"]
            .as_array()
            .expect("conversation")
            .iter()
            .any(|message| message["content"].as_str() == Some(expected.as_str())),
        "full final response including tail retained"
    );
    assert_eq!(session["usage"]["total_tokens"], 300);
    let stderr = String::from_utf8_lossy(&resumed.stderr);
    assert!(
        stderr.contains(recoveries[0].to_str().expect("absolute path")),
        "{stderr}"
    );
    assert!(stderr.contains("capacity"), "{stderr}");
    assert_eq!(server.requests.lock().expect("requests").len(), 2);
}

#[test]
fn read_pagination_real_tool_loop_follows_cursor_through_final_marker() {
    let project = Project::new(false);
    let path = project.root.join("paged.txt");
    let mut expected: Vec<_> = (0..499)
        .map(|i| format!("{i:04}{}", "x".repeat(96)))
        .collect();
    expected.push("FINAL_READ_PAGINATION_MARKER".into());
    std::fs::write(&path, expected.join("\n")).expect("fixture file");
    let observed = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured = observed.clone();
    let server = Server::new(move |n, request| {
        let cursor = if n == 1 {
            Some(1)
        } else {
            let last = request["messages"]
                .as_array()
                .expect("messages")
                .iter()
                .rev()
                .find(|message| message["role"] == "tool")
                .expect("actual returned tool message");
            let encoded = last["content"].as_str().expect("serialized tool result");
            assert!(encoded.chars().count() <= 40_000);
            let decoded: Value = serde_json::from_str(encoded).expect("valid result JSON");
            assert_eq!(decoded["ok"], true);
            let result = &decoded["result"];
            let content = result["content"].as_str().expect("content");
            captured
                .lock()
                .expect("captured lines")
                .extend(content.lines().map(str::to_owned));
            result["next_cursor"].as_u64()
        };
        match cursor {
            Some(cursor) => response(
                "",
                vec![call(
                    &format!("read-{n}"),
                    "fs_read",
                    json!({"path":path,"cursor":cursor}),
                )],
                "tool_calls",
            ),
            None => response("FINAL_READ_PAGINATION_MARKER observed", vec![], "stop"),
        }
    });
    let output = project
        .command(&server)
        .output()
        .expect("real CLI tool loop");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(*observed.lock().expect("all lines"), expected);
    assert!(
        output_json(&output)["response"]
            .as_str()
            .is_some_and(|text| text.contains("FINAL_READ_PAGINATION_MARKER"))
    );
}

#[cfg(unix)]
#[test]
fn cli_symlink_edit_then_undo_preserves_alias_and_target() {
    use std::os::unix::fs::symlink;
    let project = Project::new(false);
    let target = project.root.join("target.txt");
    let alias = project.root.join("alias.txt");
    std::fs::write(&target, "original\n").unwrap();
    symlink("target.txt", &alias).unwrap();
    let observed_target = target.clone();
    let observed_alias = alias.clone();
    let server = Server::new(move |index, _request| match index {
        1 => response(
            "",
            vec![call(
                "edit-alias",
                "edit",
                json!({"file_path":observed_alias,"target_block":"original","new_block":"changed"}),
            )],
            "tool_calls",
        ),
        2 => {
            assert_eq!(
                std::fs::read_to_string(&observed_target).unwrap(),
                "changed\n"
            );
            assert_eq!(
                std::fs::read_link(&observed_alias).unwrap(),
                PathBuf::from("target.txt")
            );
            response(
                "",
                vec![call("undo-alias", "undo", json!({}))],
                "tool_calls",
            )
        }
        3 => {
            assert_eq!(
                std::fs::read_to_string(&observed_target).unwrap(),
                "original\n"
            );
            assert_eq!(
                std::fs::read_link(&observed_alias).unwrap(),
                PathBuf::from("target.txt")
            );
            response("alias undo verified", vec![], "stop")
        }
        _ => panic!("unexpected request {index}"),
    });
    let output = project.command(&server).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output_json(&output)
            .to_string()
            .contains("alias undo verified")
    );
    assert_eq!(server.requests.lock().unwrap().len(), 3);
    assert_eq!(std::fs::read_to_string(target).unwrap(), "original\n");
    assert!(alias.is_symlink());
}

#[test]
fn recovery_hint_follows_all_sibling_results_and_survives_resume() {
    let project = Project::new(false);
    let missing = project.root.join("not found.txt");
    let existing = project.root.join("existing.txt");
    std::fs::write(&existing, "sibling evidence").unwrap();
    let server = Server::new(move |index, request| {
        if index == 1 {
            return response(
                "",
                vec![
                    call("missing", "fs_read", json!({"path":missing})),
                    call("existing", "fs_read", json!({"path":existing})),
                ],
                "tool_calls",
            );
        }
        let messages = request["messages"].as_array().unwrap();
        let start = messages
            .iter()
            .position(|m| m["tool_calls"][0]["id"] == "missing")
            .unwrap();
        assert_eq!(messages[start + 1]["tool_call_id"], "missing");
        assert_eq!(
            messages[start + 2]["tool_call_id"],
            "existing",
            "hint must follow the complete result block"
        );
        assert!(
            messages[start + 2]["content"]
                .as_str()
                .unwrap()
                .contains("sibling evidence")
        );
        assert_eq!(messages[start + 3]["role"], "user");
        assert!(
            messages[start + 3]["content"]
                .as_str()
                .unwrap()
                .contains("path")
        );
        response("recovery complete", vec![], "stop")
    });
    let first = project.command(&server).output().unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let resumed = project
        .command(&server)
        .arg("--resume=latest")
        .output()
        .unwrap();
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    assert_eq!(server.requests.lock().unwrap().len(), 3);
}

fn assert_tool_result_blocks(messages: &[Value]) {
    let mut index = 0;
    while index < messages.len() {
        let calls = messages[index]["tool_calls"].as_array();
        assert_ne!(messages[index]["role"], "tool", "orphan result");
        let Some(calls) = calls.filter(|c| !c.is_empty()) else {
            index += 1;
            continue;
        };
        let mut pending = std::collections::BTreeSet::new();
        for call in calls {
            assert!(pending.insert(call["id"].as_str().unwrap()));
        }
        index += 1;
        while index < messages.len() && messages[index]["role"] == "tool" {
            assert!(pending.remove(messages[index]["tool_call_id"].as_str().unwrap()));
            index += 1;
        }
        assert!(
            pending.is_empty(),
            "incomplete result block before non-tool message"
        );
    }
}

#[test]
fn loop_plan_block_and_stall_interventions_follow_sibling_results() {
    for mode in ["loop", "plan", "stall"] {
        let project = Project::new(false);
        let mut reads = Vec::new();
        for n in 0..16 {
            let path = project.root.join(format!("file-{n}.txt"));
            std::fs::write(&path, "evidence").unwrap();
            reads.push(path);
        }
        let server = Server::new(move |index, request| {
            if index == 1 {
                let calls=match mode {
                    "loop" => (0..4).map(|n|call(&format!("loop-{n}"),"fs_read",json!({"path":reads[0]}))).collect(),
                    "plan" => (0..4).map(|n|call(&format!("plan-{n}"),"plan_write",json!({"items":[{"id":"p1","content":"fixture plan","status":"pending"}]}))).chain(std::iter::once(call("plan-sibling","fs_read",json!({"path":reads[0]})))).collect(),
                    _ => reads.iter().enumerate().map(|(n,path)|call(&format!("stall-{n}"),"fs_read",json!({"path":path.with_extension("missing")}))).collect(),
                };
                return response("", calls, "tool_calls");
            }
            let messages = request["messages"].as_array().unwrap();
            assert_tool_result_blocks(messages);
            let start = messages
                .iter()
                .position(|m| m["tool_calls"].as_array().is_some_and(|c| !c.is_empty()))
                .unwrap();
            let count = messages[start]["tool_calls"].as_array().unwrap().len();
            assert!(messages[start + count + 1..].iter().any(|m| matches!(
                m["role"].as_str(),
                Some("system" | "user")
            )
                && m["content"].as_str().is_some_and(|c| c.contains("WARNING")
                    || c.to_lowercase().contains("repeated")
                    || c.contains("Stop repeating"))));
            if mode != "stall" {
                assert!(
                    messages[start + count]["content"]
                        .as_str()
                        .unwrap()
                        .contains("Execution skipped")
                );
            }
            response("intervention verified", vec![], "stop")
        });
        let output = project.command(&server).output().unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(server.requests.lock().unwrap().len(), 2);
    }
}

#[test]
fn malformed_legacy_session_fails_closed_without_rewriting_or_provider_request() {
    let project = Project::new(false);
    let path = project.root.join("evidence.txt");
    std::fs::write(&path, "evidence").unwrap();
    let server = Server::new(move |index, _| match index {
        1 => response(
            "",
            vec![
                call("first", "fs_read", json!({"path":path})),
                call("second", "fs_read", json!({"path":path})),
            ],
            "tool_calls",
        ),
        2 => response("seed", vec![], "stop"),
        _ => panic!("invalid history reached provider"),
    });
    assert!(project.command(&server).output().unwrap().status.success());
    let checkpoint = std::fs::read_dir(project.root.join(".doge/sessions"))
        .unwrap()
        .map(|e| e.unwrap().path().join("session.json"))
        .find(|p| p.is_file())
        .unwrap();
    let mut session: Value = serde_json::from_slice(&std::fs::read(&checkpoint).unwrap()).unwrap();
    let messages = session["conversation"].as_array_mut().unwrap();
    let first = messages
        .iter()
        .position(|m| m["tool_call_id"] == "first")
        .unwrap();
    messages.insert(
        first + 1,
        json!({"role":"user","content":"legacy intervention","tool_calls":[]}),
    );
    let bytes = serde_json::to_vec(&session).unwrap();
    std::fs::write(&checkpoint, &bytes).unwrap();
    let output = project
        .command(&server)
        .arg("--resume=latest")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid history"));
    assert_eq!(std::fs::read(checkpoint).unwrap(), bytes);
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

#[test]
fn shell_stdin_timeout_then_next_invocation_recovers_in_real_cli() {
    let project = Project::new(true);
    let config = std::fs::read_to_string(&project.config)
        .unwrap()
        .replace("allow_shell=false", "allow_shell=true");
    std::fs::write(&project.config, format!("command_timeout_ms=50\n{config}")).unwrap();
    let large = format!(
        "head -c 1048576 /dev/zero\n#{}\nsleep 30",
        "x".repeat(256 * 1024)
    );
    let server = Server::new(move |index, request| match index {
        1 => response(
            "",
            vec![call(
                "large-shell",
                "execute_shell",
                json!({"command":large}),
            )],
            "tool_calls",
        ),
        2 => {
            let messages = request["messages"].as_array().unwrap();
            assert_tool_result_blocks(messages);
            let result: Value = serde_json::from_str(
                messages
                    .iter()
                    .find(|m| m["tool_call_id"] == "large-shell")
                    .unwrap()["content"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(result["timed_out"], true);
            response(
                "",
                vec![call(
                    "fresh-shell",
                    "execute_shell",
                    json!({"command":"printf fresh"}),
                )],
                "tool_calls",
            )
        }
        3 => {
            let messages = request["messages"].as_array().unwrap();
            assert_tool_result_blocks(messages);
            let result: Value = serde_json::from_str(
                messages
                    .iter()
                    .find(|m| m["tool_call_id"] == "fresh-shell")
                    .unwrap()["content"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(result["stdout"], "fresh");
            assert_eq!(result["success"], true);
            response("shell recovery verified", vec![], "stop")
        }
        _ => panic!("unexpected request"),
    });
    let output = project.command(&server).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(server.requests.lock().unwrap().len(), 3);
}

#[cfg(unix)]
#[test]
fn search_scope_real_cli_refuses_escapes_and_ambient_rg_configuration() {
    let project = Project::new(false);
    let outside = tempfile::tempdir().unwrap();
    let outside = outside.path().canonicalize().unwrap();
    let secret = outside.join("secret.txt");
    std::fs::write(&secret, "EXTERNAL_SEARCH_SCOPE_MARKER").unwrap();
    std::fs::write(project.root.join("inside.txt"), "inside marker").unwrap();
    std::os::unix::fs::symlink(&outside, project.root.join("link")).unwrap();
    let rg_config = project.root.join("rg-config");
    std::fs::write(&rg_config, format!("--follow\n{}\n", secret.display())).unwrap();
    let relative = format!(
        "../{}/secret.txt",
        outside.file_name().unwrap().to_str().unwrap()
    );
    let absolute = project.root.join(&relative).display().to_string();
    let server = Server::new(move |index, request| {
        if index == 1 {
            return response(
                "",
                vec![
                    call(
                        "relative",
                        "search_text",
                        json!({"search_pattern":"SEARCH_SCOPE","file_glob":relative}),
                    ),
                    call(
                        "absolute",
                        "search_text",
                        json!({"search_pattern":"SEARCH_SCOPE","file_glob":absolute}),
                    ),
                    call(
                        "symlink",
                        "search_text",
                        json!({"search_pattern":"SEARCH_SCOPE","file_glob":"link/secret.txt"}),
                    ),
                    call(
                        "ambient",
                        "search_text",
                        json!({"search_pattern":"SEARCH_SCOPE","file_glob":"**/*.txt"}),
                    ),
                    call(
                        "inside",
                        "search_text",
                        json!({"search_pattern":"inside marker","file_glob":"inside.txt"}),
                    ),
                ],
                "tool_calls",
            );
        }
        let messages = request["messages"].as_array().unwrap();
        assert_tool_result_blocks(messages);
        for id in ["relative", "absolute", "symlink", "ambient", "inside"] {
            let value: Value = serde_json::from_str(
                messages.iter().find(|m| m["tool_call_id"] == id).unwrap()["content"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            assert!(!value.to_string().contains("EXTERNAL_SEARCH_SCOPE_MARKER"));
            match id {
                "ambient" => assert_eq!(value["meta"]["returned"], 0),
                "inside" => assert_eq!(value["meta"]["returned"], 1),
                _ => assert!(value["error"].is_string()),
            }
        }
        response("search scope verified", vec![], "stop")
    });
    let output = project
        .command(&server)
        .env("RIPGREP_CONFIG_PATH", rg_config)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

#[test]
fn search_process_real_cli_distinguishes_regex_failure_and_no_match() {
    let project = Project::new(false);
    std::fs::write(project.root.join("inside.txt"), "fixture marker\n").unwrap();
    let server = Server::new(move |index, request| {
        if index == 1 {
            return response(
                "",
                vec![
                    call(
                        "regex-error",
                        "search_text",
                        json!({"search_pattern":"[","file_glob":"*.txt"}),
                    ),
                    call(
                        "no-match",
                        "search_text",
                        json!({"search_pattern":"ABSENT_FIXTURE_PATTERN","file_glob":"*.txt"}),
                    ),
                ],
                "tool_calls",
            );
        }
        let messages = request["messages"].as_array().unwrap();
        assert_tool_result_blocks(messages);
        for id in ["regex-error", "no-match"] {
            let message = messages
                .iter()
                .find(|message| message["tool_call_id"] == id)
                .unwrap();
            let value: Value = serde_json::from_str(message["content"].as_str().unwrap()).unwrap();
            if id == "regex-error" {
                let error = value["error"].as_str().unwrap();
                assert!(error.contains("ripgrep failed") && error.contains("regex"));
                assert!(error.chars().count() <= 2000);
            } else {
                assert_eq!(value["ok"], true);
                assert_eq!(value["meta"]["returned"], 0);
                assert_eq!(value["meta"]["truncated"], false);
                assert!(value["meta"]["next_offset"].is_null());
            }
        }
        response("search failure semantics verified", vec![], "stop")
    });
    let output = project.command(&server).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}
