use super::*;
use std::{fs, process::Command};
use tempfile::{TempDir, tempdir};
fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git fixture");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn project() -> TempDir {
    let root = tempdir().expect("project");
    git(root.path(), &["init", "-q"]);
    fs::write(root.path().join("source.rs"), "fn main() {}\n").expect("source");
    git(root.path(), &["add", "source.rs"]);
    root
}
#[tokio::test]
async fn endpoint_and_current_changes_are_independent() {
    let root = project();
    let start = begin(root.path(), BTreeSet::new(), None).await;
    assert_eq!(start.start.status, CaptureStatus::Complete);
    let record = finish(root.path(), start, None).await;
    assert_eq!(record.run_state, RunState::StableEndpoints);
    let current = capture(root.path(), BTreeSet::new(), None).await;
    assert_eq!(
        compare_current(Some(&record), Some(&current)).state,
        CurrentState::MatchesStart
    );
    fs::write(
        root.path().join("source.rs"),
        "fn main() { println!(\"changed\"); }\n",
    )
    .expect("mutate");
    let current = capture(root.path(), BTreeSet::new(), None).await;
    assert_eq!(
        compare_current(Some(&record), Some(&current)).state,
        CurrentState::DiffersFromStart
    );
    assert_eq!(record.run_state, RunState::StableEndpoints);
    let new = finish(
        root.path(),
        begin(root.path(), BTreeSet::new(), None).await,
        None,
    )
    .await;
    assert_eq!(
        compare_current(Some(&new), Some(&current)).state,
        CurrentState::MatchesStart
    );
}
#[tokio::test]
async fn additions_deletions_binary_and_runtime_exclusions() {
    let root = project();
    fs::write(root.path().join(".gitignore"), "ignored\n.doge/\n").expect("ignore");
    fs::write(root.path().join("ignored"), "private").expect("ignored");
    fs::create_dir(root.path().join(".doge")).expect("runtime");
    fs::write(root.path().join(".doge/state"), "state").expect("state");
    let record = begin(root.path(), BTreeSet::new(), None).await;
    assert!(
        !record
            .start
            .files
            .iter()
            .any(|f| f.path == "ignored" || f.path.starts_with(".doge"))
    );
    fs::remove_file(root.path().join("source.rs")).expect("delete");
    fs::write(root.path().join("data.bin"), [0, 255, 3]).expect("binary");
    let record = finish(root.path(), record, None).await;
    assert_eq!(record.run_state, RunState::ChangedBetweenEndpoints);
    assert!(record.differences.deleted.contains(&"source.rs".into()));
    assert!(record.differences.added.contains(&"data.bin".into()));
    assert!(
        record
            .end
            .expect("end")
            .files
            .iter()
            .any(|f| f.path == "data.bin" && f.known())
    );
}
#[tokio::test]
async fn missing_git_is_partial_and_never_matching() {
    let root = project();
    fs::remove_dir_all(root.path().join(".git")).expect("remove fixture git");
    let record = finish(
        root.path(),
        begin(root.path(), BTreeSet::from(["source.rs".into()]), None).await,
        None,
    )
    .await;
    assert_ne!(record.run_state, RunState::StableEndpoints);
    assert_eq!(
        compare_current(None, record.end.as_ref()).state,
        CurrentState::NotRecorded
    );
}
#[tokio::test]
async fn file_and_path_limits_do_not_return_matching_subsets() {
    let root = project();
    let file = fs::File::create(root.path().join("huge")).expect("file");
    file.set_len(MAX_FILE_BYTES as u64 + 1)
        .expect("sparse size");
    let snapshot = capture(root.path(), BTreeSet::new(), None).await;
    assert_eq!(snapshot.status, CaptureStatus::LimitExceeded);
    assert!(snapshot.files.is_empty());
    assert!(snapshot.manifest_digest.is_none());
    let snapshot = capture(
        root.path(),
        (0..=MAX_PATHS).map(|i| format!("p{i}")).collect(),
        None,
    )
    .await;
    assert_eq!(snapshot.diagnostics, vec![Diagnostic::PathLimit]);
}
#[cfg(unix)]
#[tokio::test]
async fn symlink_and_fifo_are_not_followed_or_read() {
    use std::{ffi::CString, os::unix::fs::symlink};
    let root = project();
    let outside = tempdir().expect("outside");
    fs::write(outside.path().join("secret"), "SECRET").expect("outside secret");
    symlink(outside.path(), root.path().join("link")).expect("symlink");
    let name = CString::new(root.path().join("pipe").to_str().expect("path")).expect("cstring");
    // SAFETY: valid NUL terminated test fixture path.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let snapshot = capture(
        root.path(),
        BTreeSet::from(["link/secret".into(), "pipe".into()]),
        None,
    )
    .await;
    assert_eq!(snapshot.status, CaptureStatus::Partial);
    assert!(
        snapshot
            .files
            .iter()
            .filter(|f| f.path.starts_with("link") || f.path == "pipe")
            .all(|f| f.content_hash.is_none())
    );
}
#[cfg(unix)]
#[tokio::test]
async fn executable_bit_change_is_a_content_state_change() {
    use std::os::unix::fs::PermissionsExt;
    let root = project();
    let record = begin(root.path(), BTreeSet::new(), None).await;
    fs::set_permissions(
        root.path().join("source.rs"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("chmod");
    let record = finish(root.path(), record, None).await;
    assert_eq!(record.differences.changed, vec!["source.rs"]);
}
#[test]
fn empty_or_unknown_does_not_establish_success() {
    let snapshot = Snapshot::unavailable(Diagnostic::GitUnavailable);
    assert!(!comparable(&snapshot, &snapshot));
    assert_eq!(differences(&snapshot, &snapshot).changed.len(), 0);
    assert!(!capture::valid_path("../outside"));
    assert!(!capture::valid_path("/absolute"));
}

#[tokio::test]
async fn unresolved_extra_reference_never_claims_current_match() {
    let root = project();
    let record = finish(
        root.path(),
        begin(root.path(), BTreeSet::new(), None).await,
        None,
    )
    .await;
    let current = capture(
        root.path(),
        BTreeSet::from(["other-run-missing.rs".into()]),
        None,
    )
    .await;
    assert_eq!(current.status, CaptureStatus::Complete);
    let comparison = compare_current(Some(&record), Some(&current));
    assert_eq!(comparison.differences.unknown, vec!["other-run-missing.rs"]);
    assert_eq!(comparison.state, CurrentState::Indeterminate);
}

#[tokio::test]
async fn incomplete_or_duplicate_entries_never_claim_current_match() {
    let root = project();
    let mut record = begin(root.path(), BTreeSet::new(), None).await;
    let current = record.start.clone();
    record.start.files[0].byte_len = None;
    assert_eq!(
        compare_current(Some(&record), Some(&current)).state,
        CurrentState::Indeterminate
    );
    record.start = current.clone();
    record.start.files.push(record.start.files[0].clone());
    assert_eq!(
        compare_current(Some(&record), Some(&current)).state,
        CurrentState::Indeterminate
    );
}

#[tokio::test]
async fn running_process_change_is_observed_with_a_synchronization_point() {
    use crate::execution::{ManagedProcessSpec, ManagedRunOptions, run_managed_process};
    let root = project();
    let signals = tempdir().expect("signals outside project");
    let ready = signals.path().join("ready");
    let release = signals.path().join("release");
    let record = begin(root.path(), BTreeSet::new(), None).await;
    let script = "import pathlib,sys,time; pathlib.Path(sys.argv[1]).touch(); p=pathlib.Path(sys.argv[2]);\nwhile not p.exists(): time.sleep(0.01)\n";
    let process = tokio::spawn(run_managed_process(
        ManagedProcessSpec::new(
            "python3",
            vec![
                "-c".into(),
                script.into(),
                ready.to_string_lossy().into(),
                release.to_string_lossy().into(),
            ],
            root.path().to_path_buf(),
        ),
        ManagedRunOptions::new(Some(std::time::Duration::from_secs(5))),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !ready.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("process reached sync point");
    fs::write(
        root.path().join("source.rs"),
        "changed while process waited",
    )
    .expect("change");
    fs::write(release, "release").expect("release");
    assert!(
        process
            .await
            .expect("join")
            .expect("managed process")
            .success()
    );
    let record = finish(root.path(), record, None).await;
    assert_eq!(record.run_state, RunState::ChangedBetweenEndpoints);
    assert_eq!(record.differences.changed, vec!["source.rs"]);
}
#[tokio::test]
async fn subdirectory_untracking_and_ignored_reference_scope() {
    let root = project();
    fs::create_dir(root.path().join("child")).expect("child");
    fs::write(root.path().join("child/input"), "input").expect("child input");
    fs::write(root.path().join(".gitignore"), "child/ignored\n").expect("ignore");
    fs::write(root.path().join("child/ignored"), "ignored reference").expect("ignored");
    git(root.path(), &["add", "."]);
    let record = begin(
        &root.path().join("child"),
        BTreeSet::from(["ignored".into()]),
        None,
    )
    .await;
    assert_eq!(
        record.start.project_relative_to_git_root.as_deref(),
        Some("child")
    );
    assert!(
        record
            .start
            .files
            .iter()
            .all(|f| f.path == "input" || f.path == "ignored")
    );
    git(root.path(), &["rm", "--cached", "--", "child/input"]);
    let record = finish(&root.path().join("child"), record, None).await;
    assert_eq!(record.run_state, RunState::StableEndpoints);
}
#[tokio::test]
async fn cancelled_capture_and_invalid_path_never_confirm_match() {
    let root = project();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let snapshot = capture(root.path(), BTreeSet::new(), Some(cancel)).await;
    assert_ne!(snapshot.status, CaptureStatus::Complete);
    let snapshot = capture(root.path(), BTreeSet::from(["../outside".into()]), None).await;
    assert_eq!(snapshot.status, CaptureStatus::Partial);
    assert!(snapshot.diagnostics.contains(&Diagnostic::InvalidPath));
}
#[tokio::test]
async fn new_manifest_has_no_file_contents_and_is_timestamp_independent() {
    let root = project();
    fs::write(root.path().join("source.rs"), "PRIVATE-CONTENT\r\n").expect("content");
    let first = capture(root.path(), BTreeSet::new(), None).await;
    let second = capture(root.path(), BTreeSet::new(), None).await;
    assert_eq!(first.manifest_digest, second.manifest_digest);
    assert!(
        !serde_json::to_string(&first)
            .expect("snapshot JSON")
            .contains("PRIVATE-CONTENT")
    );
}
#[cfg(unix)]
#[test]
fn directory_replacement_does_not_escape_the_open_root() {
    use std::{io::Read, os::unix::fs::symlink};
    let root = tempdir().expect("root");
    let outside = tempdir().expect("outside");
    fs::create_dir(root.path().join("parent")).expect("parent");
    fs::write(root.path().join("parent/file"), "INSIDE").expect("inside");
    fs::write(outside.path().join("file"), "OUTSIDE-SECRET").expect("outside");
    let mut file = capture::open_relative_with_hook(root.path(), "parent/file", &mut |index| {
        if index == 0 {
            fs::rename(root.path().join("parent"), root.path().join("old-parent")).expect("rename");
            symlink(outside.path(), root.path().join("parent")).expect("swap symlink");
        }
    })
    .expect("anchored read");
    let mut contents = String::new();
    file.read_to_string(&mut contents).expect("read");
    assert_eq!(contents, "INSIDE");
}
#[tokio::test]
async fn cumulative_bytes_and_serialized_path_limits_are_explicit() {
    let root = project();
    for i in 0..9 {
        fs::File::create(root.path().join(format!("large-{i}")))
            .expect("sparse file")
            .set_len(MAX_FILE_BYTES as u64)
            .expect("size");
    }
    let snapshot = capture(root.path(), BTreeSet::new(), None).await;
    assert_eq!(snapshot.status, CaptureStatus::LimitExceeded);
    assert_eq!(snapshot.diagnostics, vec![Diagnostic::InputLimit]);
    for i in 0..9 {
        fs::remove_file(root.path().join(format!("large-{i}"))).expect("remove test file");
    }
    let prefix = "a/".repeat(1250);
    let snapshot = capture(
        root.path(),
        (0..2000).map(|i| format!("{prefix}{i}")).collect(),
        None,
    )
    .await;
    assert_eq!(snapshot.status, CaptureStatus::LimitExceeded);
    assert_eq!(snapshot.diagnostics, vec![Diagnostic::JsonLimit]);
}
#[cfg(unix)]
#[tokio::test]
async fn submodules_and_non_utf8_paths_cannot_establish_complete_match() {
    use std::os::unix::ffi::OsStringExt;
    let root = project();
    git(root.path(), &["hash-object", "-w", "source.rs"]);
    // Use a valid object as the gitlink target; the observer does not recurse.
    let oid = Command::new("git")
        .args(["hash-object", "source.rs"])
        .current_dir(root.path())
        .output()
        .expect("hash");
    let oid = String::from_utf8(oid.stdout).expect("oid");
    git(
        root.path(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},module", oid.trim()),
        ],
    );
    let snapshot = capture(root.path(), BTreeSet::new(), None).await;
    assert_eq!(snapshot.status, CaptureStatus::Partial);
    assert!(
        snapshot
            .files
            .iter()
            .any(|f| f.path == "module" && f.kind == FileKind::Submodule)
    );
    let name = std::ffi::OsString::from_vec(vec![b'f', 0xff]);
    fs::write(root.path().join(name), "input").expect("non UTF8 file");
    let snapshot = capture(root.path(), BTreeSet::new(), None).await;
    assert_ne!(snapshot.status, CaptureStatus::Complete);
    assert!(
        snapshot
            .diagnostics
            .contains(&Diagnostic::UnsupportedEncoding)
    );
}
