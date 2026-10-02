use super::model::*;
use super::*;
use crate::config::AppConfig;
use crate::provenance::*;
use crate::session::{SessionData, SessionStore};
use crate::tools::plan::{PlanItem, PlanWriteMode, plan_write};
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::{TempDir, tempdir};

#[cfg(unix)]
#[test]
fn parent_replacement_after_safety_check_cannot_read_external_bytes() {
    use std::os::unix::fs::symlink;
    let root = tempdir().expect("project");
    let outside = tempdir().expect("outside");
    fs::create_dir(root.path().join("parent")).expect("parent");
    fs::write(root.path().join("parent/source"), "inside").expect("source");
    fs::write(outside.path().join("source"), "EXTERNAL-PRIVATE").expect("outside source");
    assert!(workspace::safe_path(root.path(), "parent/source"));
    fs::rename(root.path().join("parent"), root.path().join("original")).expect("replace parent");
    symlink(outside.path(), root.path().join("parent")).expect("external symlink");
    assert!(workspace::bounded_read(root.path(), "parent/source", &mut 0).is_err());
}

#[test]
fn query_copy_rejects_changed_bytes_and_preserves_accepted_bytes() {
    let root = tempdir().expect("project");
    let frozen = tempdir().expect("private query copy");
    fs::write(root.path().join("source.rs"), "fn before() {}\n").expect("source");
    let snapshot = workspace::snapshot(
        root.path(),
        &std::collections::BTreeSet::from(["source.rs".into()]),
        &Default::default(),
    )
    .expect("snapshot");
    fs::write(root.path().join("source.rs"), "fn after() {}\n").expect("change");
    assert!(
        !workspace::freeze_query_files(root.path(), frozen.path(), &snapshot)
            .expect("reject changed")
    );
    assert!(!frozen.path().join("source.rs").exists());
    fs::write(root.path().join("source.rs"), "fn before() {}\n").expect("restore");
    assert!(workspace::freeze_query_files(root.path(), frozen.path(), &snapshot).expect("freeze"));
    fs::write(root.path().join("source.rs"), "fn later() {}\n").expect("later change");
    assert_eq!(
        fs::read_to_string(frozen.path().join("source.rs")).expect("fixed bytes"),
        "fn before() {}\n"
    );
}

#[test]
fn frozen_saved_evidence_is_not_reopened_from_mutable_storage() {
    let f = fixture(true);
    let (frozen, _) =
        collect::frozen_inputs(f.dir.path(), &f.store, &f.session.meta.id).expect("freeze");
    fs::write(
        f.store.session_dir(&f.session.meta.id).join("session.json"),
        "invalid",
    )
    .expect("replace saved session");
    let store =
        SessionStore::open_existing(frozen.path().join(".doge/sessions")).expect("private store");
    let inputs = collect::load(frozen.path(), &store, &f.session.meta.id)
        .expect("frozen input remains valid");
    assert_eq!(inputs.session.meta.id, f.session.meta.id);
}

struct Fixture {
    dir: TempDir,
    store: SessionStore,
    session: SessionData,
    provenance: ProvenanceStore,
    directive: String,
    change: String,
    binding: String,
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .expect("run test git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 test output")
}

fn init_git(root: &Path) {
    git(root, &["init", "-q"]);
    fs::write(root.join(".gitignore"), ".doge/\nignored\n").expect("ignore file");
    fs::write(root.join("code.rs"), "fn target() { }\n").expect("base file");
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "baseline"]);
}

fn fixture(with_git: bool) -> Fixture {
    let dir = tempdir().expect("temp project");
    if with_git {
        init_git(dir.path());
    }
    let store = SessionStore::new(dir.path().join(".doge/sessions")).expect("store");
    let mut session = SessionData::new();
    session.changed_files = vec!["code.rs".into()];
    store.save(&session).expect("save session");
    let provenance = ProvenanceStore::new(store.session_dir(&session.meta.id));
    let directive = provenance
        .append(
            &session.meta.id,
            ProvenanceEvent::DirectiveObserved(DirectiveObservedEvent {
                origin: DirectiveOrigin::ExecRun,
                raw_input: "DIRECTIVE-PRIVATE".into(),
                raw_input_hash: directive_content_hash("DIRECTIVE-PRIVATE"),
                effective_instruction: "EFFECTIVE-PRIVATE".into(),
                effective_instruction_hash: directive_content_hash("EFFECTIVE-PRIVATE"),
            }),
        )
        .expect("directive")
        .event_id;
    provenance
        .append(
            &session.meta.id,
            ProvenanceEvent::RequirementChanged(RequirementChangedEvent {
                directive_id: directive.clone(),
                changes: vec![RequirementTransition {
                    requirement_id: "req-1".into(),
                    before: None,
                    after: Some(RequirementSnapshot {
                        id: "req-1".into(),
                        statement: "キャッシュ | <script> ` [link](url)\n追加".into(),
                        status: RequirementStatus::Active,
                    }),
                }],
            }),
        )
        .expect("requirement");
    let obligation = VerificationObligation {
        id: "vo-test".into(),
        description: "test target".into(),
        kind: VerificationKind::Test,
        command: Some(VerificationCommandMatcher {
            program: "cargo".into(),
            args_prefix: vec!["test".into()],
        }),
    };
    let binding = obligations::obligation_binding_hash("step-1", &["req-1".into()], &obligation);
    let cfg = AppConfig {
        project_root: dir.path().to_path_buf(),
        ..AppConfig::default()
    };
    plan_write(
        vec![PlanItem {
            id: "step-1".into(),
            parent_id: None,
            content: "Implement cache".into(),
            status: "completed".into(),
            requirement_ids: vec!["req-1".into()],
            verification_obligations: vec![
                obligation,
                VerificationObligation {
                    id: "vo-lint".into(),
                    description: "lint".into(),
                    kind: VerificationKind::Lint,
                    command: None,
                },
            ],
        }],
        PlanWriteMode::Replace,
        &session.meta.id,
        &cfg,
        None,
    )
    .expect("plan");
    let content = "fn target() { println!(\"new\"); }\n";
    fs::write(dir.path().join("code.rs"), content).expect("source");
    let change = provenance
        .append(
            &session.meta.id,
            ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                transaction_id: String::new(),
                directive_id: Some(directive.clone()),
                plan_item_id: Some("step-1".into()),
                requirement_ids: vec!["req-1".into()],
                change_kind: ChangeKind::TextEdit,
                file: "code.rs".into(),
                target: ChangeTarget::File,
                before: FileStateEvidence {
                    exists: true,
                    content_hash: Some(file_content_hash("fn target() { }\n")),
                    byte_len: Some(16),
                },
                after: FileStateEvidence {
                    exists: true,
                    content_hash: Some(file_content_hash(content)),
                    byte_len: Some(content.len() as u64),
                },
                predecessor_change_id: None,
                reverts_change_id: None,
                diff: "DIFF-PRIVATE ``` </details>".into(),
                diff_hash: "blake3:diff".into(),
                lines_added: 1,
                lines_removed: 1,
            }),
        )
        .expect("change")
        .event_id;
    let fixture = Fixture {
        dir,
        store,
        session,
        provenance,
        directive,
        change,
        binding,
    };
    verification(&fixture, true, false);
    fixture
}

fn verification(f: &Fixture, success: bool, timeout: bool) {
    f.provenance
        .append(
            &f.session.meta.id,
            ProvenanceEvent::VerificationObserved(VerificationObservedEvent {
                execution_workspace: None,
                directive_id: Some(f.directive.clone()),
                plan_item_id: Some("step-1".into()),
                requirement_ids: vec!["req-1".into()],
                verification_kind: VerificationKind::Test,
                source: VerificationSource::ExecuteProcess,
                command: CommandEvidence {
                    program: "cargo".into(),
                    args: vec!["test".into(), "target".into()],
                    cwd: Some("/outside/private-project".into()),
                },
                outcome: VerificationOutcome {
                    success,
                    status: if timeout {
                        "timed_out"
                    } else if success {
                        "completed"
                    } else {
                        "failed"
                    }
                    .into(),
                    exit_code: if timeout {
                        None
                    } else {
                        Some(if success { 0 } else { 1 })
                    },
                    timed_out: timeout,
                },
                observed_change_ids: vec![f.change.clone()],
                matched_obligations: vec![VerificationObligationRef {
                    id: "vo-test".into(),
                    binding_hash: f.binding.clone(),
                }],
                stdout_excerpt: "STDOUT-PRIVATE".into(),
                stderr_excerpt: "STDERR-PRIVATE".into(),
                output_digest: "blake3:output".into(),
                output_truncated: true,
                warnings: vec![],
            }),
        )
        .expect("verification");
}

async fn report(f: &Fixture, include: bool) -> EvidenceReport {
    build_with(
        f.dir.path(),
        &f.session.meta.id[..12],
        None,
        include,
        "2026-10-02T00:00:00Z".into(),
        &GitReader::default(),
        &mut |_| {},
    )
    .await
    .expect("report")
}

#[tokio::test]
async fn links_outcomes_pending_and_private_content() {
    let f = fixture(true);
    let r = report(&f, false).await;
    assert_eq!(r.directives[0].id, f.directive);
    assert_eq!(r.changes[0].id, f.change);
    assert_eq!(r.changes[0].current_file_match, FileMatch::Matched);
    assert_eq!(r.changes[0].lifecycle_state, ChangeState::Active);
    assert_eq!(
        r.requirements[0].evidence_state,
        EvidenceState::ObservedPassing
    );
    assert_eq!(r.requirements[0].obligation_states["pending"], 1);
    assert_eq!(
        r.verifications[0].observed_change_ids.as_slice(),
        std::slice::from_ref(&f.change)
    );
    assert_eq!(r.verifications[0].cwd.as_deref(), Some("outside_project"));
    let json = render::render(&r, ReportFormat::Json).expect("JSON");
    let value: serde_json::Value = serde_json::from_str(&json).expect("parse JSON");
    assert!(value["verifications"][0]["test_count"].is_null());
    assert!(value["verifications"][0]["execution_environment"].is_null());
    for marker in [
        "DIRECTIVE-PRIVATE",
        "EFFECTIVE-PRIVATE",
        "DIFF-PRIVATE",
        "STDOUT-PRIVATE",
        "STDERR-PRIVATE",
        "/outside/private-project",
    ] {
        assert!(!json.contains(marker));
    }
    let included = render::render(&report(&f, true).await, ReportFormat::Json).expect("included");
    for marker in [
        "DIRECTIVE-PRIVATE",
        "EFFECTIVE-PRIVATE",
        "DIFF-PRIVATE",
        "STDOUT-PRIVATE",
        "STDERR-PRIVATE",
    ] {
        assert!(included.contains(marker));
    }
}

#[tokio::test]
async fn stable_json_markdown_and_safe_rendering() {
    let f = fixture(true);
    let first = report(&f, true).await;
    let second = report(&f, true).await;
    assert_eq!(
        render::render(&first, ReportFormat::Json).expect("first"),
        render::render(&second, ReportFormat::Json).expect("second")
    );
    let md = render::render(&first, ReportFormat::Markdown).expect("Markdown");
    assert!(md.contains("&#124;") && md.contains("&#60;script&#62;"));
    assert!(md.contains("````json"));
    assert!(md.contains(&first.snapshot.manifest_digest));
    assert!(md.contains("vo-lint") && md.contains("pending"));
    assert!(md.contains("not a correctness proof"));
}

#[tokio::test]
async fn failure_and_timeout_do_not_become_passing() {
    let f = fixture(false);
    verification(&f, false, true);
    let r = report(&f, false).await;
    assert_eq!(
        r.obligations
            .iter()
            .find(|o| o.id == "vo-test")
            .expect("test obligation")
            .state,
        EvidenceState::ObservedFailing
    );
    assert_eq!(r.summary.verification_successes, 1);
    assert_eq!(r.summary.verification_failures, 1);
    assert!(r.verifications.last().expect("last run").outcome.timed_out);
}

#[tokio::test]
async fn changed_obligation_binding_makes_old_success_stale() {
    let f = fixture(false);
    let cfg = AppConfig {
        project_root: f.dir.path().to_path_buf(),
        ..AppConfig::default()
    };
    let mut plan = crate::tools::plan::plan_read(&f.session.meta.id, &cfg).expect("plan");
    plan.items[0].verification_obligations[0].description = "new definition".into();
    plan.items[0].requirement_ids.clear();
    plan_write(
        plan.items,
        PlanWriteMode::Replace,
        &f.session.meta.id,
        &cfg,
        None,
    )
    .expect("remap plan");
    let r = report(&f, false).await;
    assert_eq!(r.changes[0].recorded.requirement_ids, ["req-1"]);
    assert_eq!(r.verifications[0].requirement_ids, ["req-1"]);
    assert_eq!(
        r.verifications[0].matched_obligations[0].binding_hash,
        f.binding
    );
    assert_eq!(
        r.obligations
            .iter()
            .find(|o| o.id == "vo-test")
            .expect("obligation")
            .state,
        EvidenceState::Stale
    );
}

#[tokio::test]
async fn external_edit_and_missing_are_not_file_matches() {
    let f = fixture(true);
    fs::write(f.dir.path().join("code.rs"), "external edit").expect("external");
    fs::write(f.dir.path().join("external.txt"), "user").expect("external file");
    let r = report(&f, false).await;
    assert_eq!(r.changes[0].lifecycle_state, ChangeState::Diverged);
    assert_eq!(r.changes[0].current_file_match, FileMatch::Different);
    assert_eq!(
        r.workspace_comparison
            .iter()
            .find(|p| p.path == "external.txt")
            .expect("external")
            .attribution,
        Attribution::Unattributed
    );
    assert_eq!(
        r.workspace_comparison
            .iter()
            .find(|p| p.path == "code.rs")
            .expect("session")
            .attribution,
        Attribution::SessionLinked
    );
    fs::remove_file(f.dir.path().join("code.rs")).expect("remove");
    let r = report(&f, false).await;
    assert_eq!(r.changes[0].current_file_match, FileMatch::Different);
    assert_eq!(
        r.snapshot
            .files
            .iter()
            .find(|p| p.path == "code.rs")
            .expect("file")
            .kind,
        FileKind::Missing
    );
}

#[tokio::test]
async fn optimistic_conflicts_retry_once_or_fail() {
    let f = fixture(false);
    let mut attempts = 0;
    let r = build_with(
        f.dir.path(),
        &f.session.meta.id,
        None,
        false,
        "fixed".into(),
        &GitReader::default(),
        &mut |attempt| {
            attempts += 1;
            if attempt == 0 {
                fs::write(f.dir.path().join("code.rs"), "new external").expect("race");
            }
        },
    )
    .await
    .expect("retry");
    assert_eq!(attempts, 2);
    assert_eq!(r.changes[0].lifecycle_state, ChangeState::Diverged);
    let error = build_with(
        f.dir.path(),
        &f.session.meta.id,
        None,
        false,
        "fixed".into(),
        &GitReader::default(),
        &mut |attempt| {
            fs::write(f.dir.path().join("code.rs"), format!("race {attempt}")).expect("race");
        },
    )
    .await
    .expect_err("persistent race");
    assert!(matches!(error, ReportError::ConcurrentModification));
}

#[tokio::test]
async fn event_inventory_and_plan_races_are_detected() {
    let f = fixture(false);
    let plan = f
        .dir
        .path()
        .join(format!(".doge/plans/{}.json", f.session.meta.id));
    let error = build_with(
        f.dir.path(),
        &f.session.meta.id,
        None,
        false,
        "fixed".into(),
        &GitReader::default(),
        &mut |attempt| {
            fs::write(
                f.provenance
                    .events_dir()
                    .join(format!("race-{attempt}.json")),
                "malformed",
            )
            .expect("event race");
            let data = fs::read_to_string(&plan).expect("read plan");
            fs::write(&plan, format!("{data} ")).expect("plan race");
        },
    )
    .await
    .expect_err("event races");
    assert!(matches!(error, ReportError::ConcurrentModification));
}

#[tokio::test]
async fn missing_plan_empty_provenance_and_incomplete_are_visible() {
    let dir = tempdir().expect("project");
    let store = SessionStore::new(dir.path().join(".doge/sessions")).expect("store");
    let mut session = SessionData::new();
    session.provenance_incomplete = true;
    session.provenance_record_failures = 2;
    store.save(&session).expect("save");
    let output = export(
        dir.path(),
        &session.meta.id,
        None,
        false,
        ReportFormat::Json,
    )
    .await
    .expect("empty report");
    let value: serde_json::Value = serde_json::from_str(&output).expect("JSON");
    assert_eq!(value["summary"]["verification_successes"], 0);
    assert_eq!(value["session"]["provenance_record_failures"], 2);
    assert_eq!(value["summary"]["record_collection_complete"], false);
    assert_eq!(value["plan_available"], true);
    assert!(output.contains("no_provenance"));
}

#[tokio::test]
async fn malformed_storage_and_mismatched_event_warn_without_migration() {
    let f = fixture(false);
    fs::write(f.provenance.events_dir().join("corrupt.json"), "not json").expect("corrupt");
    f.provenance
        .append(
            "another-session",
            ProvenanceEvent::PlanChanged(PlanChangedEvent {
                directive_id: None,
                changes: vec![],
            }),
        )
        .expect("wrong session");
    fs::write(
        f.dir
            .path()
            .join(format!(".doge/plans/{}.json", f.session.meta.id)),
        "broken",
    )
    .expect("broken plan");
    let before =
        collect::input_identity(f.dir.path(), &f.store, &f.session.meta.id).expect("identity");
    let r = report(&f, false).await;
    assert!(!r.plan_available && r.obligations.is_empty());
    assert!(
        r.warnings
            .iter()
            .any(|w| w.code == WarningCode::PlanUnavailable)
    );
    assert!(
        r.warnings
            .iter()
            .any(|w| w.code == WarningCode::ProvenanceLoad)
    );
    assert!(
        r.warnings
            .iter()
            .any(|w| w.code == WarningCode::InvalidEvent)
    );
    assert_eq!(
        before,
        collect::input_identity(f.dir.path(), &f.store, &f.session.meta.id).expect("after")
    );
}

#[tokio::test]
async fn git_base_stage_untracked_exclusions_and_index_preserved() {
    let f = fixture(true);
    let base = git(f.dir.path(), &["rev-parse", "HEAD"]).trim().to_string();
    git(f.dir.path(), &["add", "code.rs"]);
    // HEAD == working tree, but the index still differs: keep dirty flags.
    fs::write(f.dir.path().join("code.rs"), "fn target() { }\n").expect("restore working file");
    fs::write(f.dir.path().join("external.txt"), "untracked").expect("file");
    fs::write(f.dir.path().join("ignored"), "ignored").expect("ignored");
    let index_before = fs::read(f.dir.path().join(".git/index")).expect("index");
    let r = build_with(
        f.dir.path(),
        &f.session.meta.id,
        Some(&base),
        false,
        "fixed".into(),
        &GitReader::default(),
        &mut |_| {},
    )
    .await
    .expect("base report");
    let code = r
        .workspace_comparison
        .iter()
        .find(|p| p.path == "code.rs")
        .expect("code");
    assert!(code.staged && code.unstaged && !code.differs_from_base);
    assert!(
        r.workspace_comparison
            .iter()
            .any(|p| p.path == "external.txt" && p.untracked)
    );
    assert!(
        !r.workspace_comparison
            .iter()
            .any(|p| p.path.starts_with(".doge/") || p.path == "ignored")
    );
    assert_eq!(r.repository.base_oid.as_deref(), Some(base.as_str()));
    assert_eq!(
        index_before,
        fs::read(f.dir.path().join(".git/index")).expect("index after")
    );
    assert!(
        export(
            f.dir.path(),
            &f.session.meta.id,
            Some("--invalid"),
            false,
            ReportFormat::Json
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn committed_session_file_stays_in_manifest_and_base_comparison() {
    let f = fixture(true);
    let base = git(f.dir.path(), &["rev-parse", "HEAD"]).trim().to_string();
    git(f.dir.path(), &["add", "code.rs"]);
    git(f.dir.path(), &["commit", "-qm", "change"]);
    let r = report(&f, false).await;
    assert!(r.workspace_comparison.is_empty());
    assert!(r.snapshot.files.iter().any(|p| p.path == "code.rs"));
    let r = build_with(
        f.dir.path(),
        &f.session.meta.id,
        Some(&base),
        false,
        "fixed".into(),
        &GitReader::default(),
        &mut |_| {},
    )
    .await
    .expect("base");
    assert!(
        r.workspace_comparison
            .iter()
            .any(|p| p.path == "code.rs" && p.differs_from_base)
    );
}

#[tokio::test]
async fn binary_and_directory_states_are_unavailable() {
    let f = fixture(false);
    fs::write(f.dir.path().join("code.rs"), [0u8, 255]).expect("binary");
    let r = report(&f, false).await;
    assert!(!r.snapshot.complete);
    assert_eq!(r.changes[0].lifecycle_state, ChangeState::Unavailable);
    assert_eq!(r.changes[0].current_file_match, FileMatch::Unavailable);
    assert_eq!(r.requirements[0].evidence_state, EvidenceState::Unavailable);
    assert!(
        r.obligations
            .iter()
            .all(|o| o.state == EvidenceState::Unavailable)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_targets_are_not_followed() {
    use std::os::unix::fs::symlink;
    let f = fixture(false);
    let outside = tempdir().expect("outside");
    fs::write(outside.path().join("private"), "OUTSIDE-SECRET").expect("outside");
    fs::remove_file(f.dir.path().join("code.rs")).expect("remove");
    symlink(outside.path().join("private"), f.dir.path().join("code.rs")).expect("symlink");
    let r = report(&f, true).await;
    assert_eq!(r.snapshot.files[0].kind, FileKind::Symlink);
    assert!(r.snapshot.files[0].content_hash.is_none());
    assert!(
        !render::render(&r, ReportFormat::Json)
            .expect("JSON")
            .contains("OUTSIDE-SECRET")
    );
}

#[tokio::test]
async fn read_limits_and_nonexistent_store_do_not_create_state() {
    let dir = tempdir().expect("project");
    assert!(
        export(dir.path(), "unknown", None, false, ReportFormat::Json)
            .await
            .is_err()
    );
    assert!(!dir.path().join(".doge").exists());
    let f = fixture(false);
    let file = fs::File::create(f.dir.path().join("code.rs")).expect("large file");
    file.set_len((MAX_FILE_BYTES + 1) as u64).expect("size");
    assert!(matches!(
        export(
            f.dir.path(),
            &f.session.meta.id,
            None,
            false,
            ReportFormat::Json
        )
        .await,
        Err(ReportError::Limit(_))
    ));
}

#[tokio::test]
async fn later_mutation_and_undo_preserve_history_and_staleness() {
    let f = fixture(false);
    let original = f
        .provenance
        .load_all()
        .expect("load")
        .events
        .into_iter()
        .find(|e| e.event_id == f.change)
        .expect("change");
    let ProvenanceEvent::ChangeCommitted(first) = original.event else {
        panic!("change event");
    };
    let mut next = first.clone();
    let newer_content = "fn target() { println!(\"second\"); }\n";
    next.before = first.after.clone();
    next.after.content_hash = Some(file_content_hash(newer_content));
    next.after.byte_len = Some(newer_content.len() as u64);
    next.predecessor_change_id = Some(f.change.clone());
    fs::write(f.dir.path().join("code.rs"), newer_content).expect("new content");
    let second = f
        .provenance
        .append(
            &f.session.meta.id,
            ProvenanceEvent::ChangeCommitted(next.clone()),
        )
        .expect("second");
    let r = report(&f, false).await;
    assert_eq!(
        r.obligations
            .iter()
            .find(|o| o.id == "vo-test")
            .expect("obligation")
            .state,
        EvidenceState::Stale
    );
    assert_eq!(
        r.verifications[0].observed_change_ids.as_slice(),
        std::slice::from_ref(&f.change)
    );
    // Restore the first mutation through a recorded undo, keeping every record.
    let content = "fn target() { println!(\"new\"); }\n";
    let mut undo = next;
    undo.before = undo.after.clone();
    undo.after = first.after;
    undo.change_kind = ChangeKind::Undo;
    undo.reverts_change_id = Some(second.event_id.clone());
    undo.predecessor_change_id = Some(second.event_id.clone());
    fs::write(f.dir.path().join("code.rs"), content).expect("undo content");
    f.provenance
        .append(&f.session.meta.id, ProvenanceEvent::ChangeCommitted(undo))
        .expect("undo");
    let r = report(&f, false).await;
    assert_eq!(r.changes.len(), 3);
    assert_eq!(
        r.changes
            .iter()
            .find(|c| c.id == second.event_id)
            .expect("second record")
            .lifecycle_state,
        ChangeState::Reverted
    );
}

#[tokio::test]
async fn legacy_versions_unknown_duplicate_and_invalid_paths() {
    let f = fixture(false);
    for version in 1..=3 {
        let path = f
            .store
            .session_dir(&f.session.meta.id)
            .join(format!("provenance/v{version}/events"));
        fs::create_dir_all(&path).expect("legacy dir");
        let value = serde_json::json!({"schema_version":version,"event_id":uuid::Uuid::now_v7().to_string(),
            "session_id":f.session.meta.id,"timestamp":"2025-01-01T00:00:00Z","event":{"type":"plan_changed","changes":[]}});
        fs::write(
            path.join("legacy.json"),
            serde_json::to_vec(&value).expect("JSON"),
        )
        .expect("legacy event");
    }
    let loaded = f.provenance.load_all().expect("events");
    assert_eq!(loaded.events.len(), 7);
    let directive = loaded
        .events
        .iter()
        .find(|e| e.event_id == f.directive)
        .expect("directive");
    let wire = types::to_v3_wire(directive);
    let duplicate = f
        .store
        .session_dir(&f.session.meta.id)
        .join("provenance/v3/events/duplicate.json");
    fs::write(&duplicate, serde_json::to_vec(&wire).expect("v3")).expect("duplicate");
    fs::write(f.provenance.events_dir().join("future.json"), r#"{"schema_version":99,"event_id":"future","session_id":"s","timestamp":"x","event":{"type":"plan_changed","changes":[]}}"#).expect("future");
    let mut unsafe_change = loaded
        .events
        .iter()
        .find(|e| e.event_id == f.change)
        .expect("change")
        .event
        .clone();
    if let ProvenanceEvent::ChangeCommitted(c) = &mut unsafe_change {
        c.file = "../outside".into();
    }
    f.provenance
        .append(&f.session.meta.id, unsafe_change)
        .expect("unsafe event fixture");
    let before =
        collect::input_identity(f.dir.path(), &f.store, &f.session.meta.id).expect("before");
    let r = report(&f, false).await;
    assert_eq!(r.directives.len(), 1);
    assert_eq!(r.changes.len(), 1);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.code == WarningCode::InvalidPath)
    );
    assert!(
        r.warnings
            .iter()
            .any(|w| w.code == WarningCode::ProvenanceLoad)
    );
    assert_eq!(
        before,
        collect::input_identity(f.dir.path(), &f.store, &f.session.meta.id).expect("after")
    );
}

#[tokio::test]
async fn legacy_plan_and_superseded_changes_are_retained() {
    let f = fixture(false);
    fs::create_dir_all(f.dir.path().join(".doge/todos")).expect("legacy plans");
    fs::rename(
        f.dir
            .path()
            .join(format!(".doge/plans/{}.json", f.session.meta.id)),
        f.dir
            .path()
            .join(format!(".doge/todos/{}.json", f.session.meta.id)),
    )
    .expect("move legacy");
    let events = f.provenance.load_all().expect("load").events;
    let mut disconnected = events
        .iter()
        .find(|e| e.event_id == f.change)
        .expect("change")
        .event
        .clone();
    if let ProvenanceEvent::ChangeCommitted(c) = &mut disconnected {
        c.transaction_id.clear();
        c.predecessor_change_id = None;
    }
    f.provenance
        .append(&f.session.meta.id, disconnected)
        .expect("disconnected change");
    let r = report(&f, false).await;
    assert!(r.plan_available && !r.plan.is_empty());
    assert_eq!(r.changes.len(), 2);
    assert!(
        r.changes
            .iter()
            .any(|c| c.lifecycle_state == ChangeState::Superseded)
    );
}

#[tokio::test]
async fn project_subdirectory_and_unborn_repository_are_explicit() {
    let f = fixture(true);
    let sub = f.dir.path().join("sub");
    fs::create_dir(&sub).expect("subdir");
    let store = SessionStore::new(sub.join(".doge/sessions")).expect("store");
    let session = SessionData::new();
    store.save(&session).expect("session");
    fs::write(sub.join("new file.txt"), "sub").expect("sub file");
    fs::write(f.dir.path().join("sibling.txt"), "sibling").expect("sibling");
    let output = export(&sub, &session.meta.id, None, false, ReportFormat::Json)
        .await
        .expect("sub report");
    let value: serde_json::Value = serde_json::from_str(&output).expect("JSON");
    assert_eq!(value["scope"]["project_relative_to_git_root"], "sub");
    assert_eq!(value["workspace_comparison"][0]["path"], "new file.txt");
    assert!(!output.contains("sibling.txt"));
    let empty = fixture(false);
    git(empty.dir.path(), &["init", "-q"]);
    let r = report(&empty, false).await;
    assert_eq!(r.repository.state, RepositoryState::Unborn);
    assert!(!r.repository.comparison_available);
    assert!(r.workspace_comparison.iter().any(|p| p.path == "code.rs"));
}

#[cfg(unix)]
#[tokio::test]
async fn git_helpers_are_disabled_and_non_utf8_is_not_aliased() {
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;
    let f = fixture(true);
    let helper = f.dir.path().join("helper");
    let marker = f.dir.path().join("marker");
    fs::write(
        &helper,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .expect("helper");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).expect("permissions");
    git(
        f.dir.path(),
        &[
            "config",
            "diff.external",
            helper.to_str().expect("helper path"),
        ],
    );
    git(
        f.dir.path(),
        &[
            "config",
            "core.fsmonitor",
            helper.to_str().expect("helper path"),
        ],
    );
    fs::write(
        f.dir.path().join(".gitattributes"),
        "code.rs diff=private\n",
    )
    .expect("attributes");
    git(
        f.dir.path(),
        &[
            "config",
            "diff.private.textconv",
            helper.to_str().expect("helper path"),
        ],
    );
    let r = report(&f, false).await;
    assert!(r.repository.comparison_available);
    assert!(!marker.exists());
    let name = std::ffi::OsString::from_vec(vec![b'b', 255]);
    fs::write(f.dir.path().join(name), "bad path").expect("non-UTF-8");
    let r = report(&f, false).await;
    assert_eq!(r.repository.state, RepositoryState::Unavailable);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.code == WarningCode::WorkspaceUnavailable)
    );
    assert!(
        export(
            f.dir.path(),
            &f.session.meta.id,
            Some("HEAD"),
            false,
            ReportFormat::Json
        )
        .await
        .is_err()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn git_timeout_truncation_and_unavailable_executable() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture(false);
    let reader = GitReader {
        program: "/nonexistent/git".into(),
        timeout: std::time::Duration::from_millis(100),
    };
    assert!(
        !reader
            .capture(f.dir.path(), None)
            .await
            .expect("unavailable")
            .repository
            .comparison_available
    );
    assert!(reader.capture(f.dir.path(), Some("HEAD")).await.is_err());
    let helper = f.dir.path().join("fake-git");
    fs::write(&helper, "#!/bin/sh\nsleep 5\n").expect("fake");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).expect("permissions");
    let reader = GitReader {
        program: helper.to_str().expect("path").into(),
        timeout: std::time::Duration::from_millis(20),
    };
    assert!(reader.capture(f.dir.path(), Some("HEAD")).await.is_err());
    fs::write(&helper, "#!/bin/sh\nhead -c 100000 /dev/zero\n").expect("large capture");
    let reader = GitReader {
        timeout: std::time::Duration::from_secs(1),
        ..reader
    };
    assert!(reader.capture(f.dir.path(), Some("HEAD")).await.is_err());
}

#[tokio::test]
async fn output_and_path_count_limits_fail_before_rendering() {
    let f = fixture(false);
    let mut r = report(&f, false).await;
    r.limitations.push("x".repeat(MAX_OUTPUT_BYTES));
    assert!(matches!(
        render::render(&r, ReportFormat::Json),
        Err(ReportError::Limit("output bytes"))
    ));
    let paths = (0..=MAX_ITEMS).map(|n| format!("file-{n}")).collect();
    assert!(matches!(
        workspace::snapshot(f.dir.path(), &paths, &Default::default()),
        Err(ReportError::Limit("manifest paths"))
    ));
}

#[tokio::test]
async fn unchanged_semantic_symbol_does_not_claim_whole_file_match() {
    let f = fixture(false);
    let source = fs::read_to_string(f.dir.path().join("code.rs")).expect("source");
    let prepared = crate::features::semantic_edit::prepare_from_source(
        f.dir.path(),
        &f.dir.path().join("code.rs"),
        1,
        &source,
    )
    .expect("symbol");
    let events = f.provenance.load_all().expect("events").events;
    let mut semantic = events
        .iter()
        .find(|e| e.event_id == f.change)
        .expect("change")
        .event
        .clone();
    if let ProvenanceEvent::ChangeCommitted(c) = &mut semantic {
        c.transaction_id.clear();
        c.target = ChangeTarget::SemanticSymbol {
            symbol_id: prepared.symbol_id.to_string(),
            before_fingerprint: prepared.expected_fingerprint.to_string(),
            after_fingerprint: prepared.expected_fingerprint.to_string(),
        };
        c.change_kind = ChangeKind::SemanticEdit;
    }
    let semantic_id = f
        .provenance
        .append(&f.session.meta.id, semantic)
        .expect("semantic record")
        .event_id;
    fs::write(
        f.dir.path().join("code.rs"),
        format!("{source}\nfn unrelated() {{}}\n"),
    )
    .expect("unrelated edit");
    let r = report(&f, false).await;
    let change = r
        .changes
        .iter()
        .find(|c| c.id == semantic_id)
        .expect("symbol change");
    assert_eq!(change.lifecycle_state, ChangeState::Active);
    assert_eq!(change.current_file_match, FileMatch::Different);
}

#[cfg(unix)]
#[tokio::test]
async fn directories_and_submodules_are_incomplete() {
    let f = fixture(true);
    fs::remove_file(f.dir.path().join("code.rs")).expect("remove");
    fs::create_dir(f.dir.path().join("code.rs")).expect("directory");
    let r = report(&f, false).await;
    assert_eq!(
        r.snapshot
            .files
            .iter()
            .find(|p| p.path == "code.rs")
            .expect("directory")
            .kind,
        FileKind::Directory
    );
    assert_eq!(r.changes[0].lifecycle_state, ChangeState::Unavailable);
    let oid = git(f.dir.path(), &["rev-parse", "HEAD"]).trim().to_string();
    git(
        f.dir.path(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{oid},submodule"),
        ],
    );
    fs::create_dir(f.dir.path().join("submodule")).expect("submodule");
    let r = report(&f, false).await;
    assert!(
        r.snapshot
            .files
            .iter()
            .any(|p| p.path == "submodule" && p.kind == FileKind::Unsupported)
    );
    assert!(!r.snapshot.complete);
}

#[test]
fn read_only_session_store_refuses_mutation_and_oversized_metadata() {
    let f = fixture(false);
    let store =
        SessionStore::open_existing(f.dir.path().join(".doge/sessions")).expect("read-only");
    assert!(store.save(&f.session).is_err());
    assert!(store.delete(&f.session.meta.id).is_err());
    assert!(store.create().is_err());
    let metadata = f.store.session_dir(&f.session.meta.id).join("session.json");
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&metadata)
        .expect("metadata");
    file.set_len((MAX_FILE_BYTES + 1) as u64)
        .expect("large metadata");
    assert!(store.load(&f.session.meta.id).is_err());
    assert!(store.resolve_id_prefix(&f.session.meta.id).is_err());
}

#[tokio::test]
async fn execution_snapshot_detects_later_test_change_without_rewriting_coverage() {
    let f = fixture(true);
    let root = f.dir.path();
    fs::write(root.join("test.rs"), "initial test").expect("test input");
    let workspace = crate::features::verification_snapshot::finish(
        root,
        crate::features::verification_snapshot::begin(
            root,
            std::collections::BTreeSet::new(),
            None,
        )
        .await,
        None,
    )
    .await;
    let event = build_verification_event(VerificationRecordInput {
        kind: VerificationKind::Test,
        source: VerificationSource::ExecuteProcess,
        program: "cargo",
        args: &["test".into()],
        cwd_relative: None,
        success: true,
        status: "completed",
        exit_code: Some(0),
        timed_out: false,
        stdout: "",
        stderr: "",
        capture_truncated: false,
        context: VerificationContext {
            execution_workspace: Some(workspace),
            observed_change_ids: vec![f.change.clone()],
            plan_item_id: Some("step-1".into()),
            requirement_ids: vec!["req-1".into()],
            matched_obligations: vec![VerificationObligationRef {
                id: "vo-test".into(),
                binding_hash: f.binding.clone(),
            }],
            ..Default::default()
        },
        extra_warnings: vec![],
    });
    f.provenance
        .append(
            &f.session.meta.id,
            ProvenanceEvent::VerificationObserved(event),
        )
        .expect("verification");
    let before = report(&f, false).await;
    assert_eq!(
        before.verifications[1].current_code_state.state,
        crate::features::verification_snapshot::CurrentState::MatchesStart
    );
    fs::write(root.join("test.rs"), "test changed after success").expect("change only test");
    let after = report(&f, false).await;
    assert!(after.verifications[1].outcome.success);
    assert_eq!(
        after.verifications[1].current_code_state.state,
        crate::features::verification_snapshot::CurrentState::DiffersFromStart
    );
    assert_eq!(
        after.requirements[0].evidence_state,
        before.requirements[0].evidence_state
    );
    let markdown = render::render(&after, ReportFormat::Markdown).expect("markdown");
    assert!(markdown.contains("DiffersFromStart"));
    assert!(markdown.contains("test.rs"));
    let json = render::render(&after, ReportFormat::Json).expect("json");
    assert!(json.contains("differs_from_start"));
    assert!(!json.contains("test changed after success"));
}
