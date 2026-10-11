use super::*;
#[test]
fn verify_requires_explicit_single_selection() {
    assert_eq!(parse("").expect("list"), Action::List);
    assert_eq!(
        parse("refresh a.rs b.rs").expect("paths"),
        Action::Refresh(Some(vec!["a.rs".into(), "b.rs".into()]))
    );
    assert!(parse("run 2").is_err());
    let id = "q0123456789abcdef0123456789abcdef-2-0123456789abcdef";
    assert_eq!(
        parse(&format!("run {id}")).expect("select"),
        Action::Run(id.into())
    );
    for input in ["run", "run 0", "run -1", "run all", "run 1 2", "whatever"] {
        assert!(parse(input).is_err(), "{input}");
    }
}
fn tools(root: &std::path::Path) -> FsTools {
    FsTools::new(
        Arc::new(tokio::sync::RwLock::new(None)),
        Arc::new(crate::config::AppConfig {
            project_root: root.into(),
            no_repomap: true,
            ..Default::default()
        }),
    )
}
#[tokio::test]
async fn precancelled_verify_does_not_run_or_publish() {
    let root = tempfile::tempdir().expect("root");
    let (tx, rx) = std::sync::mpsc::channel();
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(
        run_action(
            tools(root.path()),
            Default::default(),
            Action::Run("stale".into()),
            1000,
            tx,
            token,
            None,
        )
        .await
        .expect("job"),
        JobRunOutcome::Cancelled
    );
    assert_eq!(rx.try_iter().count(), 0);
}
#[tokio::test]
async fn listing_never_executes_and_selection_runs_only_one_command() {
    let root = tempfile::tempdir().expect("root");
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(root.path())
            .status()
            .expect("git")
            .success()
    );
    std::fs::write(root.path().join("file.rs"), "fn a() {}\n").expect("file");
    let snapshot = verification_snapshot::capture(root.path(), Default::default(), None).await;
    let mut q = VerificationQueue::default();
    let make = |arg: &str| crate::features::change_impact::VerificationRecommendation {
        kind: "test".into(),
        program: "printf".into(),
        args: vec![arg.into()],
        cwd: None,
        reason: "synthetic regression".into(),
        confidence: "unknown".into(),
        coverage_scope: "file".into(),
    };
    q.refresh(
        None,
        snapshot,
        vec![make("selected"), make("unselected")],
        None,
    );
    let queue = Arc::new(Mutex::new(q));
    let (tx, rx) = std::sync::mpsc::channel();
    run_action(
        tools(root.path()),
        queue.clone(),
        Action::List,
        1000,
        tx.clone(),
        CancellationToken::new(),
        None,
    )
    .await
    .expect("list");
    assert!(
        queue
            .lock()
            .expect("queue")
            .entries
            .iter()
            .all(|e| e.outcome == Outcome::Unexecuted)
    );
    run_action(
        tools(root.path()),
        queue.clone(),
        Action::Run(selected_id(&queue)),
        1000,
        tx,
        CancellationToken::new(),
        None,
    )
    .await
    .expect("run");
    let q = queue.lock().expect("queue");
    assert_eq!(q.entries[0].outcome, Outcome::Unknown);
    assert_eq!(q.entries[0].exit_code, Some(0));
    assert_eq!(
        q.entries[0].workspace.as_ref().expect("snapshot").run_state,
        verification_snapshot::RunState::StableEndpoints
    );
    assert_eq!(q.entries[1].outcome, Outcome::Unexecuted);
    assert!(
        !rx.try_iter()
            .any(|m| m.starts_with("::test_failures_analysis:"))
    );
}

#[cfg(unix)]
fn fixture(
    script: &str,
) -> (
    tempfile::TempDir,
    FsTools,
    Arc<Mutex<VerificationQueue>>,
    Sender<String>,
    std::sync::mpsc::Receiver<String>,
) {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().expect("root");
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(root.path())
            .status()
            .expect("git")
            .success()
    );
    std::fs::write(root.path().join("file.rs"), "fn a() {}\n").expect("file");
    std::fs::write(root.path().join("go"), format!("#!/bin/sh\n{script}\n")).expect("program");
    std::fs::set_permissions(
        root.path().join("go"),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("mode");
    let cfg = crate::config::AppConfig {
        project_root: root.path().into(),
        no_repomap: true,
        ..Default::default()
    };
    let store =
        crate::session::SessionStore::new(root.path().join(".doge/sessions")).expect("store");
    let mut manager = crate::session::SessionManager::with_store(store);
    manager.create_session(None).expect("session");
    let tools = FsTools::new(Arc::new(tokio::sync::RwLock::new(None)), Arc::new(cfg))
        .with_session_manager(Arc::new(Mutex::new(manager)));
    let (tx, rx) = std::sync::mpsc::channel();
    (root, tools, Default::default(), tx, rx)
}
#[cfg(unix)]
async fn seed(tools: &FsTools, queue: &Arc<Mutex<VerificationQueue>>) {
    let root = &tools.config.project_root;
    let snapshot = verification_snapshot::capture(root, Default::default(), None).await;
    queue.lock().expect("queue").refresh(
        tools.get_current_session().map(|s| s.meta.id),
        snapshot,
        vec![crate::features::change_impact::VerificationRecommendation {
            kind: "test".into(),
            program: root.join("go").to_string_lossy().into(),
            args: vec!["test".into(), "-json".into()],
            cwd: None,
            reason: "synthetic".into(),
            confidence: "unknown".into(),
            coverage_scope: "file".into(),
        }],
        None,
    );
}
#[cfg(unix)]
#[tokio::test]
async fn mid_run_change_is_saved_with_frozen_snapshot_and_visible_as_stale() {
    let (_root, tools, queue, tx, rx) = fixture(
        r#"printf 'fn b() {}\n' > file.rs
printf '%s\n' '{"Action":"start","Package":"p"}' '{"Action":"run","Package":"p","Test":"TestT"}' '{"Action":"pass","Package":"p","Test":"TestT"}' '{"Action":"pass","Package":"p"}'"#,
    );
    seed(&tools, &queue).await;
    run_action(
        tools.clone(),
        queue.clone(),
        Action::Run(selected_id(&queue)),
        1000,
        tx,
        CancellationToken::new(),
        None,
    )
    .await
    .expect("run");
    assert_eq!(
        queue.lock().expect("queue").entries[0].outcome,
        Outcome::Passed
    );
    assert!(
        rx.try_iter()
            .any(|m| m.contains("changed during run; stale"))
    );
    let loaded = crate::tools::provenance::load_current_events(&tools)
        .expect("events")
        .expect("session");
    let observations: Vec<_> = loaded
        .events
        .iter()
        .filter_map(|env| match &env.event {
            crate::provenance::ProvenanceEvent::VerificationObserved(v) => Some(v),
            _ => None,
        })
        .collect();
    assert_eq!(observations.len(), 1);
    let event = observations[0];
    assert_eq!(
        event
            .execution_workspace
            .as_ref()
            .expect("workspace")
            .run_state,
        verification_snapshot::RunState::ChangedBetweenEndpoints
    );
    assert_eq!(
        event
            .structured_test_result
            .as_ref()
            .expect("counts")
            .test_count(),
        Some(1)
    );
    assert_eq!(event.command.args, ["test", "-json"]);
}
#[cfg(unix)]
#[tokio::test]
async fn timeout_and_failure_are_observed_but_cancelled_execution_is_not_success_evidence() {
    for (script, timeout, outcome, cancel) in [
        ("exit 4", 1000, Outcome::Failed, false),
        ("sleep 30", 20, Outcome::TimedOut, false),
        (
            "mkdir -p .doge; touch .doge/started; sleep 30",
            10000,
            Outcome::Cancelled,
            true,
        ),
    ] {
        let (_root, tools, queue, tx, _rx) = fixture(script);
        seed(&tools, &queue).await;
        let token = CancellationToken::new();
        if cancel {
            let token = token.clone();
            let marker = tools.config.project_root.join(".doge/started");
            tokio::spawn(async move {
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    while !marker.exists() {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                })
                .await
                .expect("process started");
                token.cancel();
            });
        }
        run_action(
            tools.clone(),
            queue.clone(),
            Action::Run(selected_id(&queue)),
            timeout,
            tx,
            token,
            None,
        )
        .await
        .expect("run");
        assert_eq!(queue.lock().expect("queue").entries[0].outcome, outcome);
        let loaded = crate::tools::provenance::load_current_events(&tools)
            .expect("events")
            .expect("session");
        let observations: Vec<_> = loaded
            .events
            .iter()
            .filter_map(|env| match &env.event {
                crate::provenance::ProvenanceEvent::VerificationObserved(v) => Some(v),
                _ => None,
            })
            .collect();
        assert_eq!(observations.len(), usize::from(!cancel));
        assert!(observations.iter().all(|v| !v.outcome.success));
    }
}
#[tokio::test]
async fn no_changes_is_an_empty_unexecuted_queue_even_without_git() {
    let root = tempfile::tempdir().expect("root");
    let queue = Default::default();
    let (tx, rx) = std::sync::mpsc::channel();
    run_action(
        tools(root.path()),
        queue,
        Action::List,
        1000,
        tx,
        CancellationToken::new(),
        None,
    )
    .await
    .expect("list");
    assert!(rx.try_iter().any(|m| m.contains("No candidates")));
}
#[cfg(unix)]
#[tokio::test]
async fn cancellation_before_commit_suppresses_publication_and_terminalizes_queue() {
    let (_root, tools, queue, _tx, _rx) = fixture("exit 0");
    seed(&tools, &queue).await;
    queue.lock().expect("queue").entries[0].outcome = Outcome::Running;
    let jobs = crate::jobs::JobManager::new();
    let worker_jobs = jobs.clone();
    let published = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_published = published.clone();
    let worker_queue = queue.clone();
    let (ready, entered) = tokio::sync::oneshot::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    let id = jobs
        .spawn(
            JobSpec::new(
                JobKind::Verify,
                JobScope::Foreground,
                WorkspaceAccess::ReadOnly,
                "commit cancellation regression",
            ),
            move |ctx| async move {
                let _guard = RunningGuard {
                    queue: worker_queue,
                    index: 0,
                };
                let _ = ready.send(());
                let _ = wait.await;
                commit_completion(
                    Some((worker_jobs, ctx.id)),
                    &ctx.cancellation_token(),
                    || {
                        worker_published.store(true, std::sync::atomic::Ordering::SeqCst);
                        JobRunOutcome::Completed
                    },
                )
            },
        )
        .expect("job");
    entered.await.expect("before commit");
    jobs.cancel(id);
    let _ = release.send(());
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if jobs
                .get_snapshot(id)
                .is_some_and(|j| j.status.is_terminal())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("terminal");
    assert_eq!(
        jobs.get_snapshot(id).expect("job").status,
        crate::jobs::JobStatus::Cancelled
    );
    assert!(!published.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        queue.lock().expect("queue").entries[0].outcome,
        Outcome::Cancelled
    );
    assert!(
        !crate::tools::provenance::load_current_events(&tools)
            .expect("events")
            .expect("session")
            .events
            .iter()
            .any(|e| matches!(
                e.event,
                crate::provenance::ProvenanceEvent::VerificationObserved(_)
            ))
    );
}
#[cfg(unix)]
#[tokio::test]
async fn selected_cwd_and_literal_argv_are_preserved_in_execution_and_evidence() {
    let (root, tools, queue, tx, _rx) = fixture("printf '%s\\n' \"$PWD\" \"$1\" \"$2\"");
    std::fs::create_dir(root.path().join("sub")).expect("cwd");
    seed(&tools, &queue).await;
    let literal = "literal ; touch injection-marker $(printf expanded)";
    {
        let mut q = queue.lock().expect("queue");
        q.entries[0].recommendation.cwd = Some("sub".into());
        q.entries[0].recommendation.args = vec!["--not-tests".into(), literal.into()];
    }
    run_action(
        tools.clone(),
        queue.clone(),
        Action::Run(selected_id(&queue)),
        1000,
        tx,
        CancellationToken::new(),
        None,
    )
    .await
    .expect("run");
    let loaded = crate::tools::provenance::load_current_events(&tools)
        .expect("events")
        .expect("session");
    let event = loaded
        .events
        .iter()
        .find_map(|e| match &e.event {
            crate::provenance::ProvenanceEvent::VerificationObserved(v) => Some(v),
            _ => None,
        })
        .expect("observation");
    assert_eq!(event.command.cwd.as_deref(), Some("sub"));
    assert_eq!(event.command.args, ["--not-tests", literal]);
    assert!(event.outcome.success);
    assert!(!root.path().join("sub/injection-marker").exists());
    assert!(event.stdout_excerpt.contains(literal));
}
#[tokio::test]
async fn empty_git_project_has_no_candidates_without_claiming_success() {
    let root = tempfile::tempdir().expect("root");
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(root.path())
            .status()
            .expect("git")
            .success()
    );
    let (tx, rx) = std::sync::mpsc::channel();
    run_action(
        tools(root.path()),
        Default::default(),
        Action::List,
        1000,
        tx,
        CancellationToken::new(),
        None,
    )
    .await
    .expect("list");
    assert!(rx.try_iter().any(|m| m.contains("No candidates")));
}
#[tokio::test]
async fn dispatch_and_busy_job_preserve_ownership_and_log_before_completion() {
    use crate::tui::commands::core::CommandHandler;
    let root = tempfile::tempdir().expect("root");
    let cfg = crate::config::AppConfig {
        project_root: root.path().into(),
        no_repomap: true,
        ..Default::default()
    };
    let repomap = Arc::new(tokio::sync::RwLock::new(None));
    let tools = FsTools::new(repomap.clone(), Arc::new(cfg.clone()));
    let store =
        crate::session::SessionStore::new(root.path().join(".doge/sessions")).expect("store");
    let manager = Arc::new(Mutex::new(crate::session::SessionManager::with_store(
        store,
    )));
    let mut executor = TuiExecutor::construct_with_session_manager(cfg, repomap, tools, manager)
        .expect("executor");
    let mut ui = TuiApp::new_for_test("verify", None, "default");
    executor.handle("/verify run all", &mut ui);
    assert!(executor.jobs.foreground_id().is_none());
    assert_eq!(executor.deferred_followups.len(), 0);
    let (tx, rx) = std::sync::mpsc::channel();
    executor.set_ui_tx(Some(tx));
    let id = executor
        .jobs
        .spawn(
            JobSpec::new(
                JobKind::Test,
                JobScope::Foreground,
                WorkspaceAccess::ReadOnly,
                "blocker",
            ),
            |ctx| async move {
                ctx.cancellation.cancelled().await;
                JobRunOutcome::Cancelled
            },
        )
        .expect("blocker");
    handle_verify(&mut executor, &mut ui, "");
    assert_eq!(executor.jobs.foreground_id(), Some(id));
    assert!(rx.try_iter().next().is_none());
    executor.jobs.cancel(id);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while executor.jobs.foreground_id().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("terminal");
    let _ = rx.try_iter().count();
    handle_verify(&mut executor, &mut ui, "");
    let id = executor.jobs.foreground_id().expect("verify");
    assert_eq!(
        executor.jobs.get_snapshot(id).expect("job").kind,
        JobKind::Verify
    );
    assert!(executor.start_new_session(&mut ui, None).is_err());
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while executor.jobs.foreground_id().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("terminal");
    let messages: Vec<_> = rx.try_iter().collect();
    let result_index = messages
        .iter()
        .position(|m| m.contains("No candidates"))
        .expect("queue");
    let completion_index = messages
        .iter()
        .position(|m| m == &format!("::job_completed:{id}"))
        .expect("completion");
    assert!(result_index < completion_index);
    assert!(executor.handle_verify_completed(&id.to_string(), &mut ui));
    assert_eq!(ui.status, Status::Ready);
    assert_eq!(executor.deferred_followups.len(), 0);
}

fn selected_id(queue: &Arc<Mutex<VerificationQueue>>) -> String {
    queue.lock().expect("queue").selection_id(0).expect("ID")
}

#[cfg(unix)]
#[tokio::test]
async fn stale_displayed_selection_is_rejected_before_spawn_after_refresh() {
    let (root, tools, queue, tx, _rx) = fixture("touch spawned; exit 0");
    seed(&tools, &queue).await;
    let queued_action = Action::Run(selected_id(&queue));
    {
        let mut queue = queue.lock().expect("queue");
        let mut replacement = queue.entries[0].recommendation.clone();
        replacement.args = vec!["replacement".into()];
        let planned = queue.planned.clone().expect("planned");
        let session = queue.session_id.clone();
        queue.refresh(session, planned, vec![replacement], None);
    }
    let error = run_action(
        tools.clone(),
        queue.clone(),
        queued_action,
        5000,
        tx.clone(),
        CancellationToken::new(),
        None,
    )
    .await
    .expect_err("old display must not authorize replacement");
    assert!(error.to_string().contains("Stale or unknown selection ID"));
    assert!(!root.path().join("spawned").exists());
    assert_eq!(
        queue.lock().expect("queue").entries[0].outcome,
        Outcome::Unexecuted
    );
    run_action(
        tools.clone(),
        queue.clone(),
        Action::Run(selected_id(&queue)),
        5000,
        tx,
        CancellationToken::new(),
        None,
    )
    .await
    .expect("fresh explicit selection");
    assert!(root.path().join("spawned").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn pytest_report_verify_saves_skipped_counts_without_pass_claim() {
    let (root, tools, queue, tx, _rx) = fixture("exit 0");
    let program = root.path().join("python3");
    std::fs::write(
        root.path().join("fixture.xml"),
        include_str!("../../../../../features/structured_test_results/fixtures/pytest/skipped.xml"),
    )
    .expect("report fixture");
    std::fs::write(
        &program,
        r#"#!/bin/sh
[ "$#" -eq 3 ] && [ "$1" = "-m" ] && [ "$2" = "pytest" ] && [ "$3" = "-v" ] || exit 1
report_path="${PYTEST_ADDOPTS#--junitxml=\'}"
report_path="${report_path%\'}"
cp fixture.xml "$report_path"
"#,
    )
    .expect("python fixture");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("mode");
    seed(&tools, &queue).await;
    {
        let mut queue = queue.lock().expect("queue");
        let recommendation = &mut queue.entries[0].recommendation;
        recommendation.program = program.to_string_lossy().into();
        recommendation.args = vec!["-m".into(), "pytest".into(), "-v".into()];
    }
    run_action(
        tools.clone(),
        queue.clone(),
        Action::Run(selected_id(&queue)),
        5000,
        tx,
        CancellationToken::new(),
        None,
    )
    .await
    .expect("run");
    let queue = queue.lock().expect("queue");
    assert_eq!(
        queue.entries[0].outcome,
        Outcome::Skipped,
        "exit_code={:?}, counts={:?}",
        queue.entries[0].exit_code,
        queue.entries[0].test_counts
    );
    assert_eq!(queue.entries[0].test_counts.expect("counts").skipped, 1);
    let events = crate::tools::provenance::load_current_events(&tools)
        .expect("load")
        .expect("session");
    let result = events
        .events
        .iter()
        .find_map(|e| match &e.event {
            crate::provenance::ProvenanceEvent::VerificationObserved(v) => {
                v.structured_test_result.as_deref()
            }
            _ => None,
        })
        .expect("stored report");
    assert_eq!(
        crate::features::structured_test_results::pytest::outcome(result),
        Some(crate::features::structured_test_results::pytest::Outcome::AllSkipped)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn history_restart_resume_rebuild_is_read_only_and_preserves_latest_and_stale_observations() {
    use std::os::unix::fs::PermissionsExt;
    let (root, tools, old_queue, tx, _rx) = fixture("exit 0");
    std::fs::write(
        root.path().join(".gitignore"),
        ".doge/\n.venv/\nfixture.xml\nspawned\n",
    )
    .expect("ignore");
    std::fs::write(
        root.path().join("test_example.py"),
        "def test_example(): pass\n",
    )
    .expect("source");
    std::fs::create_dir_all(root.path().join(".venv/bin")).expect("venv");
    std::fs::write(root.path().join(".venv/pyvenv.cfg"), "home = fixture\n").expect("venv config");
    std::fs::write(
        root.path().join("fixture.xml"),
        include_str!("../../../../../features/structured_test_results/fixtures/pytest/skipped.xml"),
    )
    .expect("report");
    let python = root.path().join(".venv/bin/python");
    std::fs::write(
        &python,
        r#"#!/bin/sh
report_path="${PYTEST_ADDOPTS#--junitxml=\'}"
report_path="${report_path%\'}"
printf x >> spawned
cp fixture.xml "$report_path"
"#,
    )
    .expect("runner");
    std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).expect("mode");
    tools
        .fs_write(
            root.path().join("test_example.py").to_str().expect("path"),
            "def test_example(): assert True\n",
        )
        .await
        .expect("tracked change");
    tools
        .get_session_manager_wrapper()
        .get_session_manager()
        .as_ref()
        .expect("manager")
        .lock()
        .expect("manager lock")
        .flush_current_session()
        .expect("checkpoint");
    run_action(
        tools.clone(),
        old_queue.clone(),
        Action::List,
        10000,
        tx.clone(),
        CancellationToken::new(),
        None,
    )
    .await
    .expect("list");
    let index = old_queue
        .lock()
        .expect("queue")
        .entries
        .iter()
        .position(|e| e.recommendation.program == python.to_string_lossy())
        .expect("Python candidate");
    let old_id = old_queue
        .lock()
        .expect("queue")
        .selection_id(index)
        .expect("ID");
    run_action(
        tools.clone(),
        old_queue.clone(),
        Action::Run(old_id.clone()),
        10000,
        tx.clone(),
        CancellationToken::new(),
        None,
    )
    .await
    .expect("run");
    assert_eq!(
        old_queue.lock().expect("queue").entries[index].outcome,
        Outcome::Skipped
    );
    let session = tools.get_current_session().expect("session").meta.id;
    drop(tools);
    drop(old_queue);
    let mut manager = crate::session::SessionManager::with_store(
        crate::session::SessionStore::new(root.path().join(".doge/sessions")).expect("store"),
    );
    manager
        .load_session(&session)
        .expect("resume after previous owner dropped");
    let tools =
        super::tests::tools(root.path()).with_session_manager(Arc::new(Mutex::new(manager)));
    let queue: Arc<Mutex<VerificationQueue>> = Default::default();
    run_action(
        tools.clone(),
        queue.clone(),
        Action::List,
        10000,
        tx.clone(),
        CancellationToken::new(),
        None,
    )
    .await
    .expect("restored list");
    let index = queue
        .lock()
        .expect("queue")
        .entries
        .iter()
        .position(|e| e.recommendation.program == python.to_string_lossy())
        .expect("Python candidate");
    {
        let q = queue.lock().expect("queue");
        let entry = &q.entries[index];
        let h = entry.history.as_ref().expect("historical result");
        assert_eq!(entry.outcome, Outcome::Unexecuted);
        assert_eq!(h.outcome, Outcome::Skipped);
        assert_eq!(h.counts.expect("counts").skipped, 1);
        assert_eq!(h.inputs, verification_queue::history::InputMatch::Matches);
        assert_ne!(q.selection_id(index).as_deref(), Some(old_id.as_str()));
    }
    assert_eq!(
        std::fs::read(root.path().join("spawned")).expect("marker"),
        b"x"
    );
    assert!(
        run_action(
            tools.clone(),
            queue.clone(),
            Action::Run(old_id),
            10000,
            tx.clone(),
            CancellationToken::new(),
            None
        )
        .await
        .is_err()
    );
    let loaded = crate::tools::provenance::load_current_events(&tools)
        .expect("load")
        .expect("session");
    let mut failed = loaded
        .events
        .into_iter()
        .rev()
        .find_map(|e| match e.event {
            crate::provenance::ProvenanceEvent::VerificationObserved(v) => Some(v),
            _ => None,
        })
        .expect("recorded");
    failed.outcome.success = false;
    failed.outcome.exit_code = Some(1);
    failed.structured_test_result = None;
    let context = tools.current_session_storage_context().expect("context");
    let store = crate::provenance::ProvenanceStore::new(context.session_dir);
    store
        .append(
            &session,
            crate::provenance::ProvenanceEvent::VerificationObserved(failed),
        )
        .expect("later failure");
    std::fs::write(
        root.path().join("test_example.py"),
        "def test_changed(): pass\n",
    )
    .expect("external edit");
    run_action(
        tools.clone(),
        queue.clone(),
        Action::Refresh(None),
        10000,
        tx.clone(),
        CancellationToken::new(),
        None,
    )
    .await
    .expect("refresh history");
    let snapshot = verification_snapshot::capture(root.path(), Default::default(), None).await;
    {
        let q = queue.lock().expect("queue");
        let h = q.entries[index].history.as_ref().expect("newest history");
        assert_eq!(h.outcome, Outcome::Failed);
        assert!(
            h.lines(&snapshot)
                .join("\n")
                .contains("stale for current files")
        );
    }
    let corrupt = store.events_dir().join("corrupt.json");
    std::fs::write(&corrupt, "broken later history").expect("corrupt");
    run_action(
        tools,
        queue.clone(),
        Action::Refresh(None),
        10000,
        tx,
        CancellationToken::new(),
        None,
    )
    .await
    .expect("history warning");
    let q = queue.lock().expect("queue");
    assert!(q.history_warning.is_some());
    assert!(q.entries.iter().all(|e| e.history.is_none()));
    assert_eq!(
        std::fs::read(root.path().join("spawned")).expect("no rerun"),
        b"x"
    );
}

#[test]
fn verify_path_quotes_keep_each_literal_path_together() {
    assert_eq!(
        parse(r#"refresh "src/my file.rs" 'tests/a b.rs' plain.rs"#).expect("paths"),
        Action::Refresh(Some(vec![
            "src/my file.rs".into(),
            "tests/a b.rs".into(),
            "plain.rs".into()
        ]))
    );
}
#[test]
fn verify_path_invalid_quotes_fail_before_planning() {
    for input in [
        "refresh \"unfinished",
        "refresh 'unfinished",
        "refresh \"\"",
        "refresh ''",
        "refresh \"a.rs\"suffix",
    ] {
        assert!(parse(input).is_err(), "{input}");
    }
}

#[tokio::test]
async fn verify_path_refresh_snapshots_exact_quoted_names_without_execution() {
    let root = tempfile::tempdir().expect("root");
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(root.path())
            .status()
            .expect("git")
            .success()
    );
    for path in ["my file.rs", " edge.rs ", "a'b.rs"] {
        std::fs::write(root.path().join(path), "fn a() {}\n").expect("source");
    }
    // A trimmed alias must not replace the literal file in impact analysis.
    std::fs::write(root.path().join("edge.rs"), "fn wrong() {}\n").expect("alias");
    let collected = crate::features::change_impact::changes::collect_explicit_changes(
        root.path(),
        &["my file.rs".into(), " edge.rs ".into(), "a'b.rs".into()],
        None,
    );
    assert_eq!(
        collected
            .files
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        vec![" edge.rs ", "a'b.rs", "my file.rs"]
    );
    assert!(
        collected
            .files
            .iter()
            .all(|f| f.change_kind != crate::features::change_impact::ChangeKind::Deleted)
    );
    let queue = Arc::new(Mutex::new(VerificationQueue::default()));
    let (tx, rx) = std::sync::mpsc::channel();
    run_action(
        tools(root.path()),
        queue.clone(),
        parse(r#"refresh "my file.rs" " edge.rs " "a'b.rs""#).expect("quoted refresh"),
        1000,
        tx,
        CancellationToken::new(),
        None,
    )
    .await
    .expect("refresh");
    let q = queue.lock().expect("queue");
    assert_eq!(
        q.paths.as_deref(),
        Some(["my file.rs".into(), " edge.rs ".into(), "a'b.rs".into()].as_slice())
    );
    assert_eq!(
        q.planned.as_ref().expect("snapshot").status,
        verification_snapshot::CaptureStatus::Complete
    );
    assert!(
        q.planned
            .as_ref()
            .expect("snapshot")
            .files
            .iter()
            .all(|f| f.exists == Some(true))
    );
    assert!(!q.entries.is_empty());
    assert!(q.entries.iter().all(|e| e.outcome == Outcome::Unexecuted));
    let output = rx.try_iter().collect::<Vec<_>>().join("\n");
    assert!(!output.contains("edge.rs could not"), "{output}");
    assert!(!root.path().join("marker").exists());
}
