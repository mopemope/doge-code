use super::*;
fn plan() -> plan::Plan {
    serde_json::from_value(serde_json::json!({"version":1,"instruction":"fix value", "editable_paths":["value.txt"],"verifiers":[{"program":"python3","args":["check.py"],"timeout_ms":1000}],"max_restarts":1,"per_run":{"iterations":4,"tool_calls":4,"tokens":30000,"elapsed_ms":10000},"total_requests":8,"total_tokens":60000,"total_elapsed_ms":30000})).expect("plan")
}
#[test]
fn repair_plan_rejects_unbounded_shell_and_secret_environment() {
    let mut p = plan();
    p.validate().expect("valid");
    p.max_restarts = 2;
    assert!(p.validate().is_err());
    p.max_restarts = 1;
    p.verifiers[0].timeout_ms = 0;
    assert!(p.validate().is_err());
    p.verifiers[0].timeout_ms = 1000;
    p.verifiers[0].program = "sh".into();
    assert!(p.validate().is_err());
    p.verifiers[0].program = "python3".into();
    p.verifier_env
        .insert("OPENAI_API_KEY".into(), "fixture".into());
    assert!(p.validate().is_err());
}
#[test]
fn repair_gate_refuses_complete_batch_before_declared_write() {
    let root = tempfile::tempdir().expect("root");
    fs::write(root.path().join("value.txt"), "old").expect("source");
    let guard = guard::Guard {
        budget: None,
        root: root.path().into(),
        baseline: workspace::manifest(root.path()).expect("manifest"),
        plan: plan(),
        suffixes: BTreeMap::new(),
    };
    let write = crate::llm::types::ToolCall {
        id: Some("write".into()),
        r#type: "function".into(),
        function: crate::llm::types::ToolCallFunction {
            name: "fs_write".into(),
            arguments: serde_json::json!({"path":root.path().join("value.txt"),"content":"new"})
                .to_string(),
        },
    };
    let mut denied = write.clone();
    denied.function.name = "execute_process".into();
    assert!(guard.preflight(&[write, denied]).is_err());
    assert_eq!(
        fs::read_to_string(root.path().join("value.txt")).expect("source"),
        "old"
    );
}

#[test]
fn repair_search_accepts_candidate_absolute_glob_and_refuses_escapes() {
    let owner = tempfile::tempdir().expect("root");
    let root = owner.path().join(".doge/repairs/run/candidate");
    fs::create_dir_all(&root).expect("candidate");
    fs::write(root.as_path().join("value.txt"), "old").expect("source");
    let guard = guard::Guard {
        budget: None,
        root: root.as_path().into(),
        baseline: workspace::manifest(root.as_path()).expect("manifest"),
        plan: plan(),
        suffixes: BTreeMap::new(),
    };
    let search = |glob: String| crate::llm::types::ToolCall {
        id: Some("search".into()),
        r#type: "function".into(),
        function: crate::llm::types::ToolCallFunction {
            name: "search_text".into(),
            arguments: serde_json::json!({"search_pattern":"old","file_glob":glob}).to_string(),
        },
    };
    for glob in [
        "value.txt".into(),
        "*.txt".into(),
        root.as_path().join("value.txt").display().to_string(),
        root.as_path().join("*.txt").display().to_string(),
    ] {
        guard.preflight(&[search(glob)]).expect("candidate search");
    }
    for glob in [
        "../*.txt".into(),
        ".doge/*".into(),
        "target/*".into(),
        ".git/*".into(),
        root.as_path().join("../*.txt").display().to_string(),
        root.as_path().join(".doge/*").display().to_string(),
        format!("{}-sibling/*.txt", root.as_path().display()),
    ] {
        assert!(guard.preflight(&[search(glob)]).is_err());
    }
}
#[test]
fn repair_ledger_reserves_before_start_without_refunding_or_resetting() {
    use crate::llm::client_core::RequestAttemptPolicy;
    let root = tempfile::tempdir().expect("root");
    let mut p = plan();
    p.total_requests = 1;
    let store = Store::create(root.path(), &p).expect("store");
    store.reserve_round(&p).expect("round");
    store.before_attempt().expect("request");
    assert!(store.before_attempt().is_err());
    store.reserve_round(&p).expect("second");
    assert!(store.reserve_round(&p).is_err());
    let d: ledger::Ledger =
        serde_json::from_slice(&fs::read(store.root.join("ledger.json")).expect("disk"))
            .expect("json");
    assert_eq!(d.rounds_started, 2);
    assert_eq!(d.requests_reserved, 1);
    assert_eq!(d.tokens_reserved, 60000);
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("fixture");
    fs::write(root.path().join("value.txt"), "old").expect("source");
    fs::write(root.path().join("check.py"),"from pathlib import Path\nimport sys\nprint('declared check: '+Path('value.txt').read_text())\nsys.exit(0 if Path('value.txt').read_text()=='good' else 1)\n").expect("verifier");
    fs::write(root.path().join(".gitignore"), "ignored.key\n.doge/\n").expect("ignore");
    for argv in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
    ] {
        let result = std::process::Command::new("git")
            .args(argv)
            .current_dir(root.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("git");
        assert!(result.status.success(), "{:?}", result);
    }
    fs::write(root.path().join("user-note.txt"), "untracked user work").expect("untracked");
    fs::write(
        root.path().join("ignored.key"),
        "synthetic ignored secret marker",
    )
    .expect("ignored");
    root
}
fn candidate(origin: &Path) -> PathBuf {
    fs::read_dir(origin.join(".doge/repairs"))
        .expect("runs")
        .next()
        .expect("one run")
        .expect("run")
        .path()
        .join("candidate")
}
#[tokio::test]
async fn repair_workflow_one_restart_preserves_dirty_source_and_binds_review() {
    use httptest::{Expectation, matchers::*};
    use std::sync::Mutex;
    let root = fixture();
    fs::write(root.path().join("value.txt"), "user dirty old").expect("dirty");
    let index = fs::read(root.path().join(".git/index")).expect("index");
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    let requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let captured = requests.clone();
    let original = root.path().to_path_buf();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let response_count = count.clone();
    server.expect(Expectation::matching(all_of![request::method_path("POST","/v1/chat/completions"),request::body(json_decoded(move |v:&serde_json::Value|{captured.lock().expect("requests").push(v.clone());true}))]).times(4).respond_with(move || {
        let n=response_count.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
        let msg=if n==0||n==2 {serde_json::json!({"role":"assistant","content":null,"tool_calls":[{"id":format!("write-{n}"),"type":"function","function":{"name":"fs_write","arguments":serde_json::json!({"path":candidate(&original).join("value.txt"),"content":if n==0 {"bad"}else{"good"}}).to_string()}}]})} else {serde_json::json!({"role":"assistant","content":"candidate ready for controller verification"})};
        httptest::responders::json_encoded(serde_json::json!({"choices":[{"index":0,"finish_reason":if n.is_multiple_of(2) {"tool_calls"}else{"stop"},"message":msg}],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}))
    }));
    let path = root.path().join("plan.json");
    write_json(&path, &plan()).expect("plan");
    // The plan is caller input, not an editable source. It remains protected.
    let cfg = crate::config::AppConfig {
        project_root: root.path().into(),
        no_repomap: true,
        api_key: Some("fixture".into()),
        base_url: server.url_str("/v1"),
        model: "fixture".into(),
        ..Default::default()
    };
    let report = run(cfg.clone(), &path, CancellationToken::new())
        .await
        .expect("workflow");
    assert_eq!(report.status, "verified_candidate", "{:?}", report);
    let run = PathBuf::from(&report.run);
    assert!(!run.join("baseline/ignored.key").exists());
    assert_eq!(
        fs::read_to_string(root.path().join("value.txt")).expect("original"),
        "user dirty old"
    );
    assert_eq!(
        fs::read(root.path().join(".git/index")).expect("index"),
        index
    );
    assert_eq!(
        fs::read_to_string(root.path().join("user-note.txt")).expect("user"),
        "untracked user work"
    );
    assert_eq!(
        fs::read_to_string(run.join("round-1-candidate/value.txt")).expect("first"),
        "bad"
    );
    assert_eq!(
        fs::read_to_string(run.join("round-2-candidate/value.txt")).expect("last"),
        "good"
    );
    let first: serde_json::Value =
        serde_json::from_slice(&fs::read(run.join("round-1-agent.json")).expect("agent"))
            .expect("json");
    let second: serde_json::Value =
        serde_json::from_slice(&fs::read(run.join("round-2-agent.json")).expect("agent"))
            .expect("json");
    assert_eq!(first["session_id"], second["session_id"]);
    {
        let captured = requests.lock().expect("requests");
        assert!(
            captured[2]["messages"]
                .as_array()
                .expect("messages")
                .iter()
                .any(|m| m["role"] == "tool")
        );
        assert_eq!(captured[0]["tools"].as_array().expect("tools").len(), 6);
        for request in captured.iter() {
            let system = request["messages"][0]["content"].as_str().expect("system");
            assert!(system.contains("# Finite repair tool profile"));
            assert!(!system.contains("use `tool_search`"));
            assert!(system.contains("The controller runs the declared verifiers"));
        }
        assert!(captured.iter().all(|r| r["model"] == "fixture"));
    }
    let data = inspect(&run).expect("inspect");
    assert_eq!(data["ledger"]["requests_reserved"], 4);
    assert_eq!(data["ledger"]["acceptance"], "pending");
    let digest = data["verification"]["candidate_digest"]
        .as_str()
        .expect("digest")
        .to_owned();
    cli(
        cfg,
        &RepairCommand::Review {
            run: run.clone(),
            candidate_digest: digest,
            accept: true,
            note: "independent operator inspected the fixture patch and all declared checks".into(),
        },
    )
    .await
    .expect("review");
    let accepted: serde_json::Value =
        serde_json::from_slice(&fs::read(run.join("acceptance.json")).expect("acceptance"))
            .expect("json");
    assert_eq!(accepted["decision"], "accepted");
    fs::write(run.join("round-2-candidate/value.txt"), "tampered").expect("tamper");
    assert!(inspect(&run).is_err());
}

#[test]
fn repair_gate_checks_actual_edit_ranges_including_marker_crossing() {
    let root = tempfile::tempdir().expect("root");
    let source = "same\nx\n#[cfg(test)]\nsame\n";
    fs::write(root.path().join("value.txt"), source).expect("source");
    let mut p = plan();
    p.protected_suffixes
        .insert("value.txt".into(), "#[cfg(test)]".into());
    let gate = guard::Guard {
        budget: None,
        root: root.path().into(),
        baseline: workspace::manifest(root.path()).expect("manifest"),
        suffixes: BTreeMap::from([(
            "value.txt".into(),
            workspace::suffix(source, "#[cfg(test)]")
                .expect("suffix")
                .into(),
        )]),
        plan: p,
    };
    let call = |target: &str, new: &str, end: usize| {
        crate::llm::types::ToolCall {
        id:Some("edit".into()),r#type:"function".into(),function:crate::llm::types::ToolCallFunction {
            name:"edit".into(),arguments:serde_json::json!({"file_path":root.path().join("value.txt"),"target_block":target,"new_block":new,"start_line":1,"end_line":end}).to_string(),
        },
    }
    };
    assert!(
        gate.preflight(&[call("x\n#[cfg(", "x\n//", usize::MAX)])
            .is_err()
    );
    assert!(gate.preflight(&[call("same", "updated", 1)]).is_ok());
    assert!(
        gate.preflight(&[call("same", "updated", usize::MAX)])
            .is_err()
    );
    assert!(gate.preflight(&[call("same", "#[cfg(test)]", 1)]).is_err());
    assert_eq!(
        fs::read_to_string(root.path().join("value.txt")).expect("unchanged"),
        source
    );
}
#[tokio::test]
#[cfg(unix)]
async fn repair_input_rejects_fifo_symlink_and_oversized_plan() {
    let root = tempfile::tempdir().expect("root");
    let cfg = crate::config::AppConfig {
        project_root: root.path().into(),
        ..Default::default()
    };
    let path = root.path().join("plan");
    let cpath = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).expect("path");
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        run(cfg.clone(), &path, CancellationToken::new()),
    )
    .await
    .expect("FIFO must not block");
    assert!(result.is_err());
    assert!(workspace::manifest(root.path()).is_err());
    fs::remove_file(&path).expect("remove fifo");
    fs::write(&path, vec![b' '; 65537]).expect("oversize");
    assert!(
        run(cfg.clone(), &path, CancellationToken::new())
            .await
            .is_err()
    );
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&path, &link).expect("link");
    assert!(run(cfg, &link, CancellationToken::new()).await.is_err());
    assert!(!root.path().join(".doge").exists());
}
#[test]
fn repair_save_failure_prevents_new_request_and_is_not_refunded() {
    use crate::llm::client_core::RequestAttemptPolicy;
    let root = tempfile::tempdir().expect("root");
    let p = plan();
    let store = Store::create(root.path(), &p).expect("store");
    store.reserve_round(&p).expect("round");
    let path = store.root.join("ledger.json");
    fs::remove_file(&path).expect("remove");
    fs::create_dir(&path).expect("fail atomic replace");
    assert!(store.before_attempt().is_err());
    let d = store.data.lock().expect("ledger");
    assert_eq!(d.requests_reserved, 1);
    drop(d);
    assert!(
        store.before_attempt().is_err(),
        "unknown previous attempt must not permit a new one"
    );
}
fn config_for(root: &Path, server: &httptest::Server) -> crate::config::AppConfig {
    crate::config::AppConfig {
        project_root: root.into(),
        no_repomap: true,
        api_key: Some("fixture".into()),
        base_url: server.url_str("/v1"),
        model: "fixture".into(),
        ..Default::default()
    }
}
fn completion(message: serde_json::Value, usage: bool) -> serde_json::Value {
    let mut body = serde_json::json!({"choices":[{"index":0,"finish_reason":if message["tool_calls"].is_array(){"tool_calls"}else{"stop"},"message":message}]});
    if usage {
        body["usage"] =
            serde_json::json!({"prompt_tokens":10,"completion_tokens":2,"total_tokens":12});
    }
    body
}
fn tool(name: &str, args: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"id":format!("call-{name}"),"type":"function","function":{"name":name,"arguments":args.to_string()}})
}
#[tokio::test]
async fn repair_workflow_fatal_auth_unknown_usage_and_scope_do_not_restart() {
    use httptest::{Expectation, matchers::*, responders::*};
    for case in ["auth", "unknown_usage", "scope"] {
        let root = fixture();
        let server = crate::test_support::HTTP_SERVER_POOL.get_server();
        let original = root.path().to_path_buf();
        server.expect(Expectation::matching(request::method_path("POST","/v1/chat/completions")).times(1).respond_with(move || {
            let body=if case=="scope" {completion(serde_json::json!({"role":"assistant","tool_calls":[
                tool("fs_write",serde_json::json!({"path":candidate(&original).join("value.txt"),"content":"good"})),
                tool("execute_process",serde_json::json!({"program":"python3","args":["check.py"]}))
            ]}),true)} else {completion(serde_json::json!({"role":"assistant","content":"ready"}),case!="unknown_usage")};
            status_code(if case=="auth"{401}else{200}).append_header("Content-Type","application/json").body(body.to_string())
        }));
        let path = root.path().join("plan.json");
        write_json(&path, &plan()).expect("plan");
        let report = run(
            config_for(root.path(), &server),
            &path,
            CancellationToken::new(),
        )
        .await
        .expect("stop report");
        assert_eq!(report.status, "stopped", "{case}: {:?}", report);
        let saved = inspect(Path::new(&report.run)).expect("retained evidence");
        assert_eq!(saved["ledger"]["rounds_started"], 1, "{case}");
        assert_eq!(saved["ledger"]["requests_reserved"], 1, "{case}");
        assert_eq!(
            fs::read_to_string(root.path().join("value.txt")).expect("original"),
            "old"
        );
        assert_eq!(
            fs::read_to_string(Path::new(&report.run).join("terminal-candidate/value.txt"))
                .expect("terminal"),
            "old",
            "batch prefix must not execute: {case}"
        );
        if case == "auth" {
            assert!(
                report.reason.as_deref().is_some_and(|s| s.contains("401")),
                "auth reason: {:?}",
                report
            );
        }
    }
}
#[tokio::test]
async fn repair_workflow_two_failed_rounds_stop_with_all_candidates() {
    use httptest::{Expectation, matchers::*};
    let root = fixture();
    let original = root.path().to_path_buf();
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let n = count.clone();
    server.expect(Expectation::matching(request::method_path("POST","/v1/chat/completions")).times(4).respond_with(move|| {
        let n=n.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
        let msg=if n.is_multiple_of(2) {serde_json::json!({"role":"assistant","tool_calls":[tool("fs_write",serde_json::json!({"path":candidate(&original).join("value.txt"),"content":format!("failed-{n}")}))]})}else{serde_json::json!({"role":"assistant","content":"ready"})};
        httptest::responders::json_encoded(completion(msg,true))
    }));
    let path = root.path().join("plan.json");
    write_json(&path, &plan()).expect("plan");
    let cfg = config_for(root.path(), &server);
    let report = run(cfg.clone(), &path, CancellationToken::new())
        .await
        .expect("workflow");
    assert_eq!(report.status, "stopped");
    assert!(
        report
            .reason
            .as_deref()
            .is_some_and(|s| s.contains("restart limit"))
    );
    let run = PathBuf::from(&report.run);
    let saved = inspect(&run).expect("inspect");
    assert_eq!(saved["ledger"]["rounds_started"], 2);
    assert_eq!(saved["ledger"]["requests_reserved"], 4);
    for (round, value) in [(1, "failed-0"), (2, "failed-2")] {
        assert_eq!(
            fs::read_to_string(run.join(format!("round-{round}-candidate/value.txt")))
                .expect("candidate"),
            value
        );
    }
    let command = RepairCommand::Review {
        run: run.clone(),
        candidate_digest: saved["verification"]["candidate_digest"]
            .as_str()
            .expect("digest")
            .into(),
        accept: true,
        note: "cannot accept failed candidate".into(),
    };
    assert!(cli(cfg, &command).await.is_err());
    assert!(!run.join("acceptance.json").exists());
}
#[tokio::test]
async fn repair_workflow_baseline_incomplete_or_already_passed_makes_no_requests() {
    for case in ["zero", "timeout", "spawn", "changed", "passed"] {
        let root = fixture();
        let server = crate::test_support::HTTP_SERVER_POOL.get_server();
        let mut p = plan();
        match case {
            "zero" => {
                fs::write(root.path().join("check.py"), "print('no tests')\n").expect("verifier");
                p.verifiers[0].test_format = Some("cargo".into());
                p.verifiers[0].minimum_tests = Some(1);
            }
            "timeout" => {
                fs::write(
                    root.path().join("check.py"),
                    "import time\ntime.sleep(10)\n",
                )
                .expect("verifier");
                p.verifiers[0].timeout_ms = 40;
            }
            "spawn" => p.verifiers[0].program = "dgc-no-such-frozen-verifier".into(),
            "changed" => {
                fs::write(
                    root.path().join("check.py"),
                    "from pathlib import Path\nPath('value.txt').write_text('good')\n",
                )
                .expect("verifier");
            }
            _ => fs::write(root.path().join("value.txt"), "good").expect("source"),
        }
        let path = root.path().join("plan.json");
        write_json(&path, &p).expect("plan");
        let report = run(
            config_for(root.path(), &server),
            &path,
            CancellationToken::new(),
        )
        .await
        .expect("report");
        assert_eq!(
            report.status,
            if case == "passed" {
                "already_verified"
            } else {
                "stopped"
            },
            "{case}: {:?}",
            report
        );
        let saved = inspect(Path::new(&report.run)).expect("inspect");
        assert_eq!(saved["ledger"]["requests_reserved"], 0);
        assert_eq!(saved["ledger"]["rounds_started"], 0);
    }
}
#[tokio::test]
async fn repair_workflow_provider_timeout_retains_terminal_without_restart() {
    use httptest::{Expectation, matchers::*, responders::*};
    let root = fixture();
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    server.expect(
        Expectation::matching(request::method_path("POST", "/v1/chat/completions"))
            .times(1)
            .respond_with(delay_and_then(
                Duration::from_secs(3),
                json_encoded(completion(
                    serde_json::json!({"role":"assistant","content":"late"}),
                    true,
                )),
            )),
    );
    let mut p = plan();
    // The turn deadline includes durable checkpoint/scope/attempt preparation
    // before transport. Give the request time to reach the fixture under full
    // suite load, while keeping its response later than the entire turn.
    p.per_run.elapsed_ms = 1000;
    let path = root.path().join("plan.json");
    write_json(&path, &p).expect("plan");
    let report = run(
        config_for(root.path(), &server),
        &path,
        CancellationToken::new(),
    )
    .await
    .expect("report");
    assert_eq!(report.status, "stopped");
    let data = inspect(Path::new(&report.run)).expect("inspect");
    assert_eq!(data["ledger"]["rounds_started"], 1);
    assert_eq!(data["ledger"]["requests_reserved"], 1);
    assert_eq!(data["ledger"]["usage_records"], 0);
    assert_eq!(data["ledger"]["tokens_reserved"], p.per_run.tokens);
    assert!(
        Path::new(&report.run)
            .join("terminal-candidate/value.txt")
            .exists()
    );
}

#[test]
fn repair_checkpoint_refuses_unsaved_current_session() {
    let root = tempfile::tempdir().expect("root");
    let store = crate::session::SessionStore::new(root.path().join("sessions")).expect("store");
    let mut manager = crate::session::SessionManager::with_store(store);
    manager.create_session(None).expect("session");
    let id = manager.current_session_id().expect("id");
    let manager = Arc::new(std::sync::Mutex::new(manager));
    let tools = crate::tools::FsTools::new(
        Arc::new(tokio::sync::RwLock::new(None)),
        Arc::new(crate::config::AppConfig {
            project_root: root.path().into(),
            ..Default::default()
        }),
    )
    .with_session_manager(manager.clone());
    tools
        .ensure_repair_checkpoint()
        .expect("initial durable state");
    fs::remove_file(root.path().join("sessions").join(id).join("session.json"))
        .expect("delete saved checkpoint");
    {
        let mut mgr = manager.lock().expect("manager");
        assert!(mgr.flush_current_session().is_err());
        assert!(mgr.has_unsaved_current_session());
        assert!(mgr.checkpoint_warning().is_none());
    }
    assert!(tools.ensure_repair_checkpoint().is_err());
    assert!(
        !tools
            .get_current_session()
            .expect("session")
            .provenance_incomplete
    );
}

#[tokio::test]
#[cfg(unix)]
async fn repair_native_inventory_error_preserves_diagnostics() {
    let root = fixture();
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    fs::write(
        root.path().join("check.py"),
        "import os\nprint('native diagnostic retained')\nos.mkfifo('unsupported_fifo')\n",
    )
    .expect("verifier");
    let path = root.path().join("plan.json");
    write_json(&path, &plan()).expect("plan");
    let report = run(
        config_for(root.path(), &server),
        &path,
        CancellationToken::new(),
    )
    .await
    .expect("stop report");
    assert_eq!(report.status, "stopped");
    let checks: Verification = serde_json::from_slice(
        &read_record(&Path::new(&report.run).join("baseline-checks.json"), 65536)
            .expect("saved output"),
    )
    .expect("json");
    assert!(!checks.complete);
    assert!(!checks.passed);
    assert_eq!(checks.checks[0].status, "source_inventory_failed");
    assert!(
        checks.checks[0]
            .stdout
            .contains("native diagnostic retained")
    );
}
#[tokio::test]
async fn repair_inspection_checks_acceptance_flags_and_completed_agent_binding() {
    use httptest::{Expectation, matchers::*, responders::*};
    let root = fixture();
    let original = root.path().to_path_buf();
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    server.expect(Expectation::matching(request::method_path("POST","/v1/chat/completions")).times(2).respond_with(move || {
        let path=candidate(&original).join("value.txt");
        let msg=if fs::read_to_string(&path).expect("candidate")=="good" {serde_json::json!({"role":"assistant","content":"ready"})}
            else {serde_json::json!({"role":"assistant","tool_calls":[tool("fs_write",serde_json::json!({"path":path,"content":"good"}))]})};
        json_encoded(completion(msg,true))
    }));
    let path = root.path().join("plan.json");
    write_json(&path, &plan()).expect("plan");
    let report = run(
        config_for(root.path(), &server),
        &path,
        CancellationToken::new(),
    )
    .await
    .expect("run");
    assert_eq!(report.status, "verified_candidate");
    let run = PathBuf::from(&report.run);
    let lease = ledger::lock_owner(&run.join("owner.lock"), false).expect("lease");
    assert!(
        ledger::lock_owner(&run.join("owner.lock"), false).is_err(),
        "same run cannot be reviewed by overlapping owners"
    );
    drop(lease);
    let check_path = run.join("round-1-checks.json");
    let raw = read_record(&check_path, 65536).expect("checks");
    let mut checks: Verification = serde_json::from_slice(&raw).expect("checks");
    checks.checks[0].exit_code = Some(1);
    write_json(&check_path, &checks).expect("tamper");
    assert!(inspect(&run).is_err());
    fs::write(&check_path, &raw).expect("restore");
    let agent_path = run.join("round-1-agent.json");
    let raw = read_record(&agent_path, 65536).expect("agent");
    let mut agent: serde_json::Value = serde_json::from_slice(&raw).expect("agent");
    agent["status"] = serde_json::json!("partial");
    write_json(&agent_path, &agent).expect("tamper");
    assert!(inspect(&run).is_err());
    fs::write(&agent_path, raw).expect("restore");
    assert!(inspect(&run).is_ok());
}
#[tokio::test]
async fn repair_insufficient_cumulative_token_reservation_stops_before_restart() {
    use httptest::{Expectation, matchers::*, responders::*};
    let root = fixture();
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    server.expect(
        Expectation::matching(request::method_path("POST", "/v1/chat/completions"))
            .times(1)
            .respond_with(json_encoded(completion(
                serde_json::json!({"role":"assistant","content":"no change"}),
                true,
            ))),
    );
    let mut p = plan();
    p.total_tokens = p.per_run.tokens;
    let path = root.path().join("plan.json");
    write_json(&path, &p).expect("plan");
    let report = run(
        config_for(root.path(), &server),
        &path,
        CancellationToken::new(),
    )
    .await
    .expect("report");
    assert_eq!(report.status, "stopped");
    assert!(
        report
            .reason
            .as_deref()
            .is_some_and(|s| s.contains("reservation limit"))
    );
    let data = inspect(Path::new(&report.run)).expect("inspect");
    assert_eq!(data["ledger"]["rounds_started"], 1);
    assert_eq!(data["ledger"]["requests_reserved"], 1);
}
#[tokio::test]
async fn repair_partial_agent_stops_even_when_declared_verifier_fails() {
    use httptest::{Expectation, matchers::*};
    let root = fixture();
    let original = root.path().to_path_buf();
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    server.expect(Expectation::matching(request::method_path("POST","/v1/chat/completions")).times(1).respond_with(move || {
        httptest::responders::json_encoded(completion(serde_json::json!({"role":"assistant","tool_calls":[tool("fs_write",serde_json::json!({"path":candidate(&original).join("value.txt"),"content":"still failing"}))]}),true))
    }));
    let mut p = plan();
    p.per_run.tool_calls = 1;
    p.total_requests = 1;
    let path = root.path().join("plan.json");
    write_json(&path, &p).expect("plan");
    let report = run(
        config_for(root.path(), &server),
        &path,
        CancellationToken::new(),
    )
    .await
    .expect("report");
    assert_eq!(report.status, "stopped");
    let data = inspect(Path::new(&report.run)).expect("inspect");
    assert_eq!(data["ledger"]["rounds_started"], 1);
    assert_eq!(data["ledger"]["requests_reserved"], 1);
}

#[tokio::test]
async fn repair_usage_storage_failure_stops_before_model_write() {
    use httptest::{Expectation, matchers::*, responders::*};
    let root = fixture();
    let original = root.path().to_path_buf();
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    server.expect(Expectation::matching(request::method_path("POST","/v1/chat/completions")).times(1).respond_with(move || {
        let live=candidate(&original);let ledger=live.parent().expect("run").join("ledger.json");
        fs::remove_file(&ledger).expect("remove");fs::create_dir(&ledger).expect("simulate storage failure");
        json_encoded(completion(serde_json::json!({"role":"assistant","tool_calls":[tool("fs_write",serde_json::json!({"path":live.join("value.txt"),"content":"good"}))]}),true))
    }));
    let path = root.path().join("plan.json");
    write_json(&path, &plan()).expect("plan");
    let err = run(
        config_for(root.path(), &server),
        &path,
        CancellationToken::new(),
    )
    .await
    .expect_err("storage failure must propagate");
    assert!(err.to_string().contains("evidence retained"));
    let live = candidate(root.path());
    let stored = live.parent().expect("run");
    assert_eq!(
        fs::read_to_string(stored.join("terminal-candidate/value.txt")).expect("candidate"),
        "old"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("value.txt")).expect("original"),
        "old"
    );
    assert!(!stored.join("acceptance.json").exists());
}
#[tokio::test]
async fn repair_truncated_native_capture_is_saved_and_stops_without_model() {
    let root = fixture();
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    fs::write(root.path().join("check.py"), "print('x'*2000000)\n").expect("verifier");
    let path = root.path().join("plan.json");
    write_json(&path, &plan()).expect("plan");
    let report = run(
        config_for(root.path(), &server),
        &path,
        CancellationToken::new(),
    )
    .await
    .expect("report");
    assert_eq!(report.status, "stopped");
    let checks: Verification = serde_json::from_slice(
        &read_record(
            &Path::new(&report.run).join("baseline-checks.json"),
            16 * 1024 * 1024,
        )
        .expect("checks"),
    )
    .expect("json");
    assert!(!checks.complete);
    assert!(!checks.passed);
    assert!(!checks.checks[0].stdout.is_empty());
}

#[tokio::test]
async fn repair_expired_verification_records_unstarted_stop_without_spawning() {
    let root = fixture();
    let mut p = plan();
    p.total_elapsed_ms = 1;
    let store = Store::create(root.path(), &p).expect("store");
    let env = environment(&store.root, &p).expect("environment");
    let baseline = source(root.path(), &env, CancellationToken::new())
        .await
        .expect("source");
    let live = store.root.join("candidate");
    workspace::copy(root.path(), &live, &baseline).expect("copy");
    tokio::time::sleep(Duration::from_millis(3)).await;
    let checks = verify(&store, &p, &live, "expired", &env, CancellationToken::new())
        .await
        .expect("retained deadline result");
    assert!(!checks.complete);
    assert!(!checks.passed);
    assert_eq!(checks.checks[0].status, "unstarted_budget_stop");
    assert!(store.root.join("expired-checks.json").exists());
    assert!(store.finish("verified_candidate", None).is_err());
    store
        .finish("stopped", Some("expired".into()))
        .expect("stop record");
}

fn saved_evidence(passed: bool) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().expect("source");
    fs::write(root.path().join("value.txt"), "old").expect("source");
    fs::write(root.path().join("protected.txt"), "protected bytes").expect("protected");
    let p = plan();
    let store = Store::create(root.path(), &p).expect("store");
    let baseline = workspace::manifest(root.path()).expect("manifest");
    write_json(&store.root.join("baseline-manifest.json"), &baseline).expect("baseline");
    workspace::copy(root.path(), &store.root.join("baseline"), &baseline).expect("copy");
    workspace::copy(
        root.path(),
        &store.root.join("round-1-candidate"),
        &baseline,
    )
    .expect("copy");
    let digest = workspace::digest(&baseline).expect("digest");
    write_json(
        &store.root.join("round-1-checks.json"),
        &Verification {
            passed,
            complete: true,
            candidate_digest: digest,
            checks: vec![Check {
                exit_code: Some(if passed { 0 } else { 1 }),
                status: "completed".into(),
                stdout: "declared check".into(),
                stderr: String::new(),
                reported_tests: None,
            }],
        },
    )
    .expect("checks");
    store.reserve_round(&p).expect("round");
    {
        let mut data = store.data.lock().expect("ledger");
        data.requests_reserved = 1;
        data.usage_records = 1;
        data.provider_reported_tokens = 12;
    }
    write_json(&store.root.join("round-1-agent.json"),&serde_json::json!({"status":"completed","stop_reason":null,"session_id":"fixture-session","budget":{"charged_tokens":12}})).expect("agent");
    store
        .finish(
            if passed {
                "verified_candidate"
            } else {
                "stopped"
            },
            None,
        )
        .expect("finish");
    let path = store.root.clone();
    store.release_lease().expect("release");
    (root, path)
}
#[tokio::test]
async fn repair_corrupt_ledger_cannot_claim_free_round_or_acceptance() {
    let (_source, root) = saved_evidence(true);
    let original = read_record(&root.join("ledger.json"), 65536).expect("ledger");
    for field in [
        "tokens_reserved",
        "requests_reserved",
        "usage_records",
        "acceptance",
        "status",
    ] {
        let mut ledger: serde_json::Value = serde_json::from_slice(&original).expect("ledger");
        ledger[field] = match field {
            "acceptance" => serde_json::json!("accepted"),
            "status" => serde_json::json!("unsupported"),
            _ => serde_json::json!(0),
        };
        write_json(&root.join("ledger.json"), &ledger).expect("corrupt");
        assert!(inspect(&root).is_err(), "corrupt {field} must be rejected");
    }
    fs::write(root.join("ledger.json"), original).expect("restore");
    assert!(inspect(&root).is_ok());
}
#[tokio::test]
async fn repair_corrupt_rejection_cannot_accept_failed_candidate() {
    let (_source, root) = saved_evidence(false);
    let digest = inspect(&root).expect("inspect")["verification"]["candidate_digest"]
        .as_str()
        .expect("digest")
        .to_owned();
    cli(
        crate::config::AppConfig::default(),
        &RepairCommand::Review {
            run: root.clone(),
            candidate_digest: digest,
            accept: false,
            note: "independent rejection".into(),
        },
    )
    .await
    .expect("reject");
    let original = read_record(&root.join("acceptance.json"), 65536).expect("record");
    for (field, value) in [
        ("decision", "accepted"),
        ("decision", "unsupported"),
        ("original_application", "performed"),
        ("note", ""),
    ] {
        let mut record: serde_json::Value = serde_json::from_slice(&original).expect("record");
        record[field] = serde_json::json!(value);
        write_json(&root.join("acceptance.json"), &record).expect("corrupt");
        assert!(inspect(&root).is_err(), "{field}: {value}");
    }
    fs::write(root.join("acceptance.json"), original).expect("restore");
    assert_eq!(
        inspect(&root).expect("valid rejection")["acceptance"]["decision"],
        "rejected"
    );
}
#[test]
fn repair_protected_snapshot_corruption_is_detected_without_touching_source() {
    let (source, root) = saved_evidence(true);
    for relative in ["round-1-candidate/protected.txt", "baseline/protected.txt"] {
        let path = root.join(relative);
        let original = fs::read(&path).expect("original");
        fs::write(&path, "corruption").expect("corrupt");
        assert!(inspect(&root).is_err());
        fs::write(path, original).expect("restore");
    }
    assert_eq!(
        fs::read_to_string(source.path().join("protected.txt")).expect("source"),
        "protected bytes"
    );
    assert!(inspect(&root).is_ok());
}

#[test]
fn repair_already_verified_requires_zero_rounds_and_bound_passing_baseline() {
    let (_source, root) = saved_evidence(false);
    let ledger_path = root.join("ledger.json");
    let original = read_record(&ledger_path, 65536).expect("ledger");
    let mut ledger: serde_json::Value = serde_json::from_slice(&original).expect("ledger");
    ledger["status"] = serde_json::json!("already_verified");
    write_json(&ledger_path, &ledger).expect("status");
    assert!(
        inspect(&root).is_err(),
        "agent work cannot be described as a passing baseline"
    );
    ledger["rounds_started"] = serde_json::json!(0);
    ledger["requests_reserved"] = serde_json::json!(0);
    ledger["usage_records"] = serde_json::json!(0);
    ledger["provider_reported_tokens"] = serde_json::json!(0);
    ledger["tokens_reserved"] = serde_json::json!(0);
    write_json(&ledger_path, &ledger).expect("zero rounds");
    assert!(
        inspect(&root).is_err(),
        "missing baseline checks cannot verify baseline"
    );
    fs::copy(
        root.join("round-1-checks.json"),
        root.join("baseline-checks.json"),
    )
    .expect("failed baseline");
    assert!(
        inspect(&root).is_err(),
        "failed baseline cannot be already verified"
    );
    let mut checks: Verification = serde_json::from_slice(
        &read_record(&root.join("baseline-checks.json"), 65536).expect("checks"),
    )
    .expect("checks");
    checks.passed = true;
    checks.checks[0].exit_code = Some(0);
    write_json(&root.join("baseline-checks.json"), &checks).expect("passing baseline");
    assert!(inspect(&root).is_ok());
    fs::write(root.join("baseline/protected.txt"), "corruption").expect("corrupt baseline");
    assert!(inspect(&root).is_err());
}

#[tokio::test]
async fn repair_retry_final_response_over_budget_stops_before_verification() {
    use httptest::{Expectation, matchers::*, responders::*};
    let root = fixture();
    let original = root.path().to_path_buf();
    let server = crate::test_support::HTTP_SERVER_POOL.get_server();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let n = count.clone();
    server.expect(Expectation::matching(request::method_path("POST","/v1/chat/completions")).times(3).respond_with(move || {
        let n=n.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
        let msg=if n==1 {serde_json::json!({"role":"assistant","tool_calls":[tool("fs_write",serde_json::json!({"path":candidate(&original).join("value.txt"),"content":"good"}))]})}else{serde_json::json!({"role":"assistant","content":"candidate ready"})};
        let mut body=completion(msg,true);
        if n==2 {body["usage"]=serde_json::json!({"prompt_tokens":30000,"completion_tokens":1,"total_tokens":30001});}
        json_encoded(body)
    }));
    let path = root.path().join("plan.json");
    write_json(&path, &plan()).expect("plan");
    let report = run(
        config_for(root.path(), &server),
        &path,
        CancellationToken::new(),
    )
    .await
    .expect("report");
    assert_eq!(
        report.status, "stopped",
        "final response may complete but repair budget must stop: {:?}",
        report
    );
    let run = PathBuf::from(&report.run);
    assert!(
        report
            .reason
            .as_deref()
            .is_some_and(|s| s.contains("turn budget"))
    );
    assert!(
        !run.join("round-2-checks.json").exists(),
        "no verifier after budget stop"
    );
    assert_eq!(
        fs::read_to_string(run.join("round-2-candidate/value.txt")).expect("saved"),
        "good"
    );
    let data = inspect(&run).expect("inspect stopped");
    assert_eq!(data["ledger"]["rounds_started"], 2);
    assert_eq!(data["ledger"]["requests_reserved"], 3);
    assert_eq!(data["ledger"]["tokens_reserved"], 60000);
    assert_eq!(
        fs::read_to_string(root.path().join("value.txt")).expect("original"),
        "old"
    );
}

#[test]
fn repair_saved_turn_budget_must_be_known_and_within_allowance() {
    let (_source, root) = saved_evidence(true);
    let path = root.join("round-1-agent.json");
    let raw = read_record(&path, 65536).expect("agent");
    let mut record: serde_json::Value = serde_json::from_slice(&raw).expect("agent");
    record["budget"]["charged_tokens"] = serde_json::json!(30000);
    write_json(&path, &record).expect("at limit");
    assert!(inspect(&root).is_ok());
    record["budget"]["charged_tokens"] = serde_json::json!(30001);
    write_json(&path, &record).expect("over limit");
    assert!(inspect(&root).is_err());
    record["budget"]
        .as_object_mut()
        .expect("budget")
        .remove("charged_tokens");
    write_json(&path, &record).expect("unknown");
    assert!(inspect(&root).is_err());
}
