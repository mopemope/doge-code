//! Regression coverage for checker startup failures versus executed failures.
use super::*;

fn tools(root: &std::path::Path) -> crate::tools::FsTools {
    crate::tools::FsTools::new(
        std::sync::Arc::new(tokio::sync::RwLock::new(None)),
        std::sync::Arc::new(crate::config::AppConfig {
            project_root: root.to_path_buf(),
            no_repomap: true,
            ..Default::default()
        }),
    )
}
fn python_project(root: &std::path::Path) {
    std::fs::write(
        root.join("test_value.py"),
        "def test_value(): assert 4 == 4\n",
    )
    .expect("source");
    std::fs::create_dir_all(root.join(".venv/bin")).expect("environment");
    std::fs::write(root.join(".venv/pyvenv.cfg"), "home = local-fixture\n")
        .expect("environment marker");
}
async fn run(
    root: &std::path::Path,
    timeout: u64,
) -> (String, crate::tui::followup::DeferredFollowupStore) {
    let (tx, rx) = std::sync::mpsc::channel();
    let deferred = crate::tui::followup::DeferredFollowupStore::default();
    let outcome = run_test_job(
        crate::jobs::JobId(1),
        deferred.clone(),
        root.into(),
        tools(root),
        tx,
        timeout,
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(outcome, JobRunOutcome::Completed));
    (rx.try_iter().collect::<Vec<_>>().join("\n"), deferred)
}
#[tokio::test]
async fn checker_start_failure_does_not_request_code_repair_or_claim_tests_passed() {
    let root = tempfile::tempdir().expect("root");
    python_project(root.path());
    let original = std::fs::read(root.path().join("test_value.py")).expect("source");
    let (messages, deferred) = run(root.path(), 5000).await;
    assert_eq!(deferred.len(), 0, "{messages}");
    assert!(messages.contains("Failed to run tests"), "{messages}");
    assert!(messages.contains("Retry /test"), "{messages}");
    assert!(
        messages.contains("test execution not confirmed"),
        "{messages}"
    );
    assert!(
        !messages.contains("::shell_output:could not start:"),
        "{messages}"
    );
    assert!(
        messages.contains("test success was not confirmed"),
        "{messages}"
    );
    assert!(
        !messages.contains("Sending test failures to LLM"),
        "{messages}"
    );
    assert!(
        !messages.contains("All reported tests passed"),
        "{messages}"
    );
    assert_eq!(
        std::fs::read(root.path().join("test_value.py")).expect("source"),
        original
    );
}
#[tokio::test]
async fn checker_start_failure_is_excluded_from_mixed_compile_failure_repair() {
    let root = tempfile::tempdir().expect("root");
    python_project(root.path());
    std::fs::create_dir(root.path().join("src")).expect("source directory");
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname='checker_start_mixed'\nversion='0.0.0'\nedition='2021'\n[workspace]\n",
    )
    .expect("manifest");
    let source = "compile_error!(\"REAL_COMPILE_FAILURE\");\n";
    std::fs::write(root.path().join("src/lib.rs"), source).expect("source");
    let (messages, deferred) = run(root.path(), 30000).await;
    let followup = deferred
        .take(crate::jobs::JobId(1))
        .expect("executed compile failure");
    assert!(
        followup.prompt.contains("REAL_COMPILE_FAILURE"),
        "{}",
        followup.prompt
    );
    assert!(messages.contains("Failed to run tests"), "{messages}");
    assert!(
        !followup.prompt.contains("Failed to run tests"),
        "{}",
        followup.prompt
    );
    assert!(
        !followup.prompt.contains(".venv/bin/python"),
        "{}",
        followup.prompt
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("src/lib.rs")).expect("source"),
        source
    );
}
#[cfg(unix)]
#[tokio::test]
async fn checker_started_timeout_still_requests_analysis() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().expect("root");
    python_project(root.path());
    let interpreter = root.path().join(".venv/bin/python");
    std::fs::write(
        &interpreter,
        "#!/bin/sh\nprintf 'STARTED_TIMEOUT_MARKER\\n'\nwhile :; do :; done\n",
    )
    .expect("local checker stand-in");
    std::fs::set_permissions(&interpreter, std::fs::Permissions::from_mode(0o755)).expect("mode");
    let (_, deferred) = run(root.path(), 100).await;
    let followup = deferred
        .take(crate::jobs::JobId(1))
        .expect("started timeout");
    assert!(
        followup.prompt.contains("STARTED_TIMEOUT_MARKER"),
        "{}",
        followup.prompt
    );
    assert!(
        followup.prompt.contains("Exit code: timeout"),
        "{}",
        followup.prompt
    );
}
