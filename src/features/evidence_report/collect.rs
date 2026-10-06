use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::config::AppConfig;
use crate::provenance::{ProvenanceEvent as Event, ProvenanceEventEnvelope, ProvenanceStore};
use crate::provenance::{obligations as ob, query, requirements as req};
use crate::session::{SessionData, SessionStore};
use crate::tools::plan::{PlanItem, plan_read_from_base_path};

use super::model::*;
use super::workspace::{GitSnapshot, bounded_read, safe_path, valid_relative};
use super::{MAX_ITEMS, ReportError, Result};

pub(super) struct Inputs {
    pub session: SessionData,
    pub events: Vec<ProvenanceEventEnvelope>,
    pub plan: Vec<PlanItem>,
    pub plan_available: bool,
    pub warnings: Vec<ReportWarning>,
}

/// Includes byte identity and inventory changes, even for malformed events.
pub(super) fn input_identity(
    root: &Path,
    store: &SessionStore,
    id: &str,
) -> Result<BTreeMap<String, String>> {
    read_inputs(root, store, id, None)
}

/// Legacy loaders operate on a private, bounded copy rather than reopening
/// mutable project paths after their safety checks.
pub(super) fn frozen_inputs(
    root: &Path,
    store: &SessionStore,
    id: &str,
) -> Result<(tempfile::TempDir, BTreeMap<String, String>)> {
    let frozen = tempfile::tempdir()?;
    let identity = read_inputs(root, store, id, Some(frozen.path()))?;
    Ok((frozen, identity))
}

fn read_inputs(
    root: &Path,
    store: &SessionStore,
    id: &str,
    destination: Option<&Path>,
) -> Result<BTreeMap<String, String>> {
    let mut files = Vec::new();
    let dir = store.session_dir(id);
    inventory(root, &dir, &mut files, 0)?;
    for relative in [
        format!(".doge/plans/{id}.json"),
        format!(".doge/todos/{id}.json"),
    ] {
        let path = root.join(&relative);
        if path.exists() {
            files.push(path);
        }
    }
    files.sort();
    files.dedup();
    let mut total = 0;
    let mut identity = BTreeMap::new();
    for path in files {
        let name = path
            .strip_prefix(root)
            .map_err(|_| ReportError::UnsafeInput)?
            .to_str()
            .ok_or(ReportError::UnsafeInput)?
            .replace('\\', "/");
        if !safe_path(root, &name) {
            return Err(ReportError::UnsafeInput);
        }
        let bytes = bounded_read(root, &name, &mut total)?;
        if let Some(destination) = destination {
            let target = destination.join(&name);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(target, &bytes)?;
        }
        identity.insert(name, blake3::hash(&bytes).to_hex().to_string());
    }
    Ok(identity)
}

fn inventory(
    root: &Path,
    dir: &Path,
    files: &mut Vec<std::path::PathBuf>,
    depth: usize,
) -> Result<()> {
    if depth > 8 {
        return Err(ReportError::Limit("input directory depth"));
    }
    let relative = dir
        .strip_prefix(root)
        .map_err(|_| ReportError::UnsafeInput)?
        .to_str()
        .ok_or(ReportError::UnsafeInput)?
        .replace('\\', "/");
    if !safe_path(root, &relative) {
        return Err(ReportError::UnsafeInput);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(ReportError::UnsafeInput);
        }
        if kind.is_dir() {
            inventory(root, &entry.path(), files, depth + 1)?;
        } else if kind.is_file() {
            files.push(entry.path());
        } else {
            return Err(ReportError::UnsafeInput);
        }
        if files.len() > MAX_ITEMS {
            return Err(ReportError::Limit("input files"));
        }
    }
    Ok(())
}

pub(super) fn load(root: &Path, store: &SessionStore, id: &str) -> Result<Inputs> {
    let session = store.load(id)?;
    if session.meta.id != id {
        return Err(ReportError::UnsafeInput);
    }
    let loaded = ProvenanceStore::new(store.session_dir(id)).load_all()?;
    let mut warnings = Vec::new();
    // Loader warnings can contain serde's untrusted text and absolute paths.
    // Preserve each diagnostic as a typed gap without exporting its raw input.
    for (index, _) in loaded.warnings.iter().enumerate() {
        warn(
            &mut warnings,
            WarningCode::ProvenanceLoad,
            format!(
                "Provenance loader diagnostic {}; an event was unreadable, malformed, unsupported, or duplicated",
                index + 1
            ),
        );
    }
    let mut events = Vec::new();
    for event in loaded.events {
        if event.session_id != id {
            warn(
                &mut warnings,
                WarningCode::InvalidEvent,
                "Excluded event with mismatched session ID",
            );
        } else if matches!(&event.event, Event::ChangeCommitted(c) if !valid_relative(&c.file)) {
            warn(
                &mut warnings,
                WarningCode::InvalidPath,
                "Excluded event with non-project-relative file path",
            );
        } else {
            events.push(event);
        }
    }
    if events.len() > MAX_ITEMS
        || events.iter().any(|event| match &event.event {
            Event::RequirementChanged(value) => value.changes.len() > MAX_ITEMS,
            Event::PlanChanged(value) => value.changes.len() > MAX_ITEMS,
            Event::VerificationObserved(value) => {
                value.observed_change_ids.len() > MAX_ITEMS
                    || value.matched_obligations.len() > MAX_ITEMS
            }
            _ => false,
        })
    {
        return Err(ReportError::Limit("events or event items"));
    }
    if events.is_empty() {
        warn(
            &mut warnings,
            WarningCode::NoProvenance,
            "No recorded provenance; absence of evidence is not a passing result",
        );
    }
    if session.provenance_incomplete {
        warn(
            &mut warnings,
            WarningCode::ProvenanceIncomplete,
            "Session recorded failed provenance writes",
        );
    }
    let config = AppConfig {
        project_root: root.to_path_buf(),
        ..AppConfig::default()
    };
    let (plan, plan_available) = match plan_read_from_base_path(id, root, &config) {
        Ok(plan) if plan.session_id.as_deref().is_none_or(|value| value == id) => {
            (plan.items, true)
        }
        _ => {
            warn(
                &mut warnings,
                WarningCode::PlanUnavailable,
                "Plan could not be read or belongs to another session; obligation states are unavailable",
            );
            (vec![], false)
        }
    };
    if plan.len() > MAX_ITEMS
        || plan
            .iter()
            .map(|p| p.verification_obligations.len())
            .sum::<usize>()
            > MAX_ITEMS
    {
        return Err(ReportError::Limit("plan items"));
    }
    Ok(Inputs {
        session,
        events,
        plan,
        plan_available,
        warnings,
    })
}

pub(super) fn paths(inputs: &mut Inputs, git: &GitSnapshot) -> BTreeSet<String> {
    let mut paths: BTreeSet<_> = inputs
        .events
        .iter()
        .filter_map(|e| {
            if let Event::ChangeCommitted(c) = &e.event {
                Some(c.file.clone())
            } else {
                None
            }
        })
        .collect();
    for name in &inputs.session.changed_files {
        if valid_relative(name) {
            paths.insert(name.clone());
        } else {
            warn(
                &mut inputs.warnings,
                WarningCode::InvalidPath,
                "Skipped invalid session changed-file path",
            );
        }
    }
    paths.extend(git.files.iter().map(|f| f.path.clone()));
    paths.extend(git.unsupported.iter().cloned());
    paths
}

pub(super) struct ReportRoots<'a> {
    pub project: &'a Path,
    pub query: &'a Path,
}

pub(super) fn build(
    roots: ReportRoots<'_>,
    mut inputs: Inputs,
    mut git: GitSnapshot,
    snapshot: Snapshot,
    current_execution_snapshot: Option<&crate::features::verification_snapshot::Snapshot>,
    include_content: bool,
    generated_at: String,
) -> EvidenceReport {
    let root = roots.project;
    let query_root = roots.query;
    let unavailable_files: BTreeSet<_> = snapshot
        .files
        .iter()
        .filter(|f| !matches!(f.kind, FileKind::Text | FileKind::Missing))
        .map(|f| f.path.clone())
        .collect();
    // Never let the existing queries read a symlink, binary, special file, or
    // inaccessible target. Keep historical records in the DTO, with unknown
    // current state; affected requirement/obligation evidence is unavailable.
    let query_events: Vec<_> = inputs.events.iter().filter(|e| {
        !matches!(&e.event, Event::ChangeCommitted(c) if unavailable_files.contains(&c.file) || !safe_path(query_root, &c.file))
    }).cloned().collect();
    for path in &unavailable_files {
        warn(
            &mut inputs.warnings,
            WarningCode::FileUnavailable,
            format!("Current file state unsupported or unavailable: {path}"),
        );
    }
    if !git.repository.comparison_available {
        warn(
            &mut inputs.warnings,
            WarningCode::WorkspaceUnavailable,
            "Git comparison is unavailable; workspace-wide attribution is incomplete",
        );
    }
    let resolved = query::resolve_active_states(query_root, &query_events);
    let current_req = req::current_requirements(&inputs.events);
    for _ in &current_req.warnings {
        warn(
            &mut inputs.warnings,
            WarningCode::RequirementHistory,
            "Requirement history contains an invalid transition",
        );
    }
    let links = req::plan_requirement_links(&inputs.plan, &inputs.events);
    let req_coverage =
        req::compute_requirement_coverage(&query_events, &current_req.items, &links, query_root);
    let ob_coverage = ob::compute_obligation_coverage(query_root, &query_events, &inputs.plan);
    let unavailable_plans: BTreeSet<_> = inputs
        .events
        .iter()
        .filter_map(|e| match &e.event {
            Event::ChangeCommitted(c) if unavailable_files.contains(&c.file) => {
                c.plan_item_id.clone()
            }
            _ => None,
        })
        .collect();
    let unavailable_reqs: BTreeSet<_> = inputs
        .events
        .iter()
        .filter_map(|e| match &e.event {
            Event::ChangeCommitted(c) if unavailable_files.contains(&c.file) => {
                Some(c.requirement_ids.clone())
            }
            _ => None,
        })
        .flatten()
        .collect();
    let obligations: Vec<_> = ob_coverage
        .into_iter()
        .map(|c| Obligation {
            id: c.obligation_id,
            plan_item_id: c.plan_item_id.clone(),
            kind: c.kind,
            binding_hash: c.binding_hash,
            state: if unavailable_plans.contains(&c.plan_item_id) {
                EvidenceState::Unavailable
            } else {
                obligation_state(c.state)
            },
            active_change_ids: c.active_change_ids,
        })
        .collect();
    let requirements =
        current_req
            .items
            .into_iter()
            .zip(req_coverage)
            .map(|(r, c)| {
                let mut states = BTreeMap::new();
                for obligation in &obligations {
                    if inputs.plan.iter().any(|p| {
                        p.id == obligation.plan_item_id && p.requirement_ids.contains(&r.id)
                    }) {
                        *states
                            .entry(obligation.state.label().to_string())
                            .or_insert(0) += 1;
                    }
                }
                let mut verification_ids = inputs
                    .events
                    .iter()
                    .filter_map(|e| match &e.event {
                        Event::VerificationObserved(v) if v.requirement_ids.contains(&r.id) => {
                            Some(e.event_id.clone())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                verification_ids.sort();
                Requirement {
                    id: r.id.clone(),
                    statement: r.statement,
                    status: r.status,
                    source_directive_ids: r.source_directive_ids,
                    linked_plan_item_ids: c.linked_plan_item_ids,
                    active_change_ids: c.active_change_ids,
                    observed_passing_change_ids: c.verified_active_change_ids,
                    unobserved_change_ids: c.unverified_active_change_ids,
                    diverged_change_ids: c.diverged_change_ids,
                    reverted_change_ids: c.reverted_change_ids,
                    verification_ids,
                    evidence_state: if unavailable_reqs.contains(&r.id) {
                        EvidenceState::Unavailable
                    } else {
                        requirement_state(c.evidence_state)
                    },
                    obligation_states: states,
                }
            })
            .collect::<Vec<_>>();
    let mut directives = Vec::new();
    let mut changes = Vec::new();
    let mut verifications = Vec::new();
    for event in &inputs.events {
        match &event.event {
            Event::DirectiveObserved(d) => directives.push(Directive {
                id: event.event_id.clone(),
                timestamp: event.timestamp.clone(),
                origin: d.origin,
                raw_input_hash: d.raw_input_hash.clone(),
                effective_instruction_hash: d.effective_instruction_hash.clone(),
                raw_input: include_content.then(|| d.raw_input.clone()),
                effective_instruction: include_content.then(|| d.effective_instruction.clone()),
            }),
            Event::ChangeCommitted(c) => {
                let state = resolved
                    .iter()
                    .find(|r| r.change_id == event.event_id)
                    .map(|r| change_state(r.state))
                    .unwrap_or(ChangeState::Unavailable);
                let file_match = snapshot
                    .files
                    .iter()
                    .find(|f| f.path == c.file)
                    .map(|f| current_match(f, &c.after))
                    .unwrap_or(FileMatch::Unavailable);
                let mut recorded = c.clone();
                if !include_content {
                    recorded.diff.clear();
                }
                changes.push(Change {
                    id: event.event_id.clone(),
                    timestamp: event.timestamp.clone(),
                    lifecycle_state: state,
                    current_file_match: file_match,
                    recorded,
                });
            }
            Event::VerificationObserved(v) => verifications.push(Verification {
                execution_context: v.execution_context.clone(),
                execution_workspace: v.execution_workspace.clone(),
                current_code_state: crate::features::verification_snapshot::compare_current(
                    v.execution_workspace.as_deref(),
                    current_execution_snapshot,
                ),
                id: event.event_id.clone(),
                timestamp: event.timestamp.clone(),
                directive_id: v.directive_id.clone(),
                plan_item_id: v.plan_item_id.clone(),
                requirement_ids: v.requirement_ids.clone(),
                source: v.source,
                kind: v.verification_kind,
                program: v.command.program.clone(),
                args: v.command.args.clone(),
                cwd: v
                    .command
                    .cwd
                    .as_deref()
                    .map(|value| relative_cwd(root, value)),
                outcome: v.outcome.clone(),
                observed_change_ids: v.observed_change_ids.clone(),
                matched_obligations: v.matched_obligations.clone(),
                output_digest: v.output_digest.clone(),
                output_truncated: v.output_truncated,
                test_count: None,
                execution_environment: None,
                warnings: v.warnings.clone(),
                stdout_excerpt: include_content.then(|| v.stdout_excerpt.clone()),
                stderr_excerpt: include_content.then(|| v.stderr_excerpt.clone()),
            }),
            _ => {}
        }
    }
    for file in &mut git.files {
        file.session_change_ids = changes
            .iter()
            .filter(|c| c.recorded.file == file.path)
            .map(|c| c.id.clone())
            .collect();
        file.session_change_ids.sort();
        if !file.session_change_ids.is_empty() {
            file.attribution = Attribution::SessionLinked;
        }
    }
    let mut summary = Summary::default();
    for r in &requirements {
        *summary
            .requirement_states
            .entry(r.evidence_state.label().into())
            .or_default() += 1;
    }
    for o in &obligations {
        *summary
            .obligation_states
            .entry(o.state.label().into())
            .or_default() += 1;
    }
    for c in &changes {
        *summary
            .change_states
            .entry(change_label(c.lifecycle_state).into())
            .or_default() += 1;
    }
    summary.verification_successes = verifications.iter().filter(|v| v.outcome.success).count();
    summary.verification_failures = verifications.len() - summary.verification_successes;
    summary.unattributed_files = git
        .files
        .iter()
        .filter(|f| f.attribution == Attribution::Unattributed)
        .count();
    summary.record_collection_complete = inputs.warnings.is_empty() && snapshot.complete;
    let review_handoff = super::handoff::build(&changes, &verifications);
    EvidenceReport {
        schema_version: 2, generator: format!("dgc/{}", env!("CARGO_PKG_VERSION")), generated_at,
        session: ReportSession { id: inputs.session.meta.id, updated_at: inputs.session.timestamp,
            provenance_incomplete: inputs.session.provenance_incomplete,
            provenance_record_failures: inputs.session.provenance_record_failures },
        scope: Scope { comparison: "single_session_against_current_project_workspace".into(),
            project_relative_to_git_root: git.repository.project_relative_to_git_root.clone(),
            excluded_workspace_paths: vec!["ignored files".into(), ".git/".into(), ".doge/ (except explicit session files)".into(),
                "outside project root".into(), "submodule contents".into()] },
        repository: git.repository, snapshot, directives, requirements, plan: inputs.plan,
        plan_available: inputs.plan_available, obligations, changes, verifications, workspace_comparison: git.files,
        review_handoff, summary, warnings: inputs.warnings,
        limitations: vec![
            "Observed passing is a command outcome, not a correctness proof or requirement satisfaction.".into(),
            "Requirement coverage does not imply all verification obligations passed.".into(),
            "Execution context covers recorded OS family/architecture and a bounded allowlisted primary-tool version only. Missing legacy context and unavailable versions remain unknown; test counts and dependency-wide snapshots are not recorded. Context equality never proves reproducibility or correctness. Version-five execution workspace endpoints cover selected project files only; legacy observations have no recorded endpoints.".into(),
            "Endpoint equality does not establish atomic snapshots or unchanged inputs throughout execution; changes restored between observations may be missed. Historical coverage remains independent of current code correspondence.".into(),
            "The manifest identifies selected files at export time; it is not a repository tree, execution snapshot, signature, or reproducibility guarantee.".into(),
            "Before/after comparison is optimistic and does not provide an atomic filesystem snapshot.".into(),
            "A session-linked file does not attribute every hunk or external edit to dgc.".into(),
            "No tests were executed for this export. Shell, workflow, remote MCP, and manual work may be unrecorded.".into(),
        ],
    }
}

fn relative_cwd(root: &Path, value: &str) -> String {
    let path = Path::new(value);
    if path == root || value == "." {
        return ".".into();
    }
    let relative = if path.is_absolute() {
        path.strip_prefix(root).ok()
    } else {
        Some(path)
    };
    relative
        .and_then(Path::to_str)
        .filter(|p| valid_relative(p))
        .map(str::to_string)
        .unwrap_or_else(|| "outside_project".into())
}

fn current_match(file: &FileEntry, expected: &crate::provenance::FileStateEvidence) -> FileMatch {
    if !matches!(file.kind, FileKind::Text | FileKind::Missing) {
        return FileMatch::Unavailable;
    }
    if !expected.exists {
        return if file.exists == Some(false) {
            FileMatch::Matched
        } else {
            FileMatch::Different
        };
    }
    if expected.content_hash.is_none() {
        return FileMatch::NotRecorded;
    }
    if file.exists == Some(true) && file.content_hash == expected.content_hash {
        FileMatch::Matched
    } else {
        FileMatch::Different
    }
}

fn requirement_state(state: req::RequirementEvidenceState) -> EvidenceState {
    match state {
        req::RequirementEvidenceState::NoLinkedWork => EvidenceState::NoLinkedWork,
        req::RequirementEvidenceState::PlannedNoActiveChange => {
            EvidenceState::PlannedNoActiveChange
        }
        req::RequirementEvidenceState::ActiveUnverified => EvidenceState::ActiveUnverified,
        req::RequirementEvidenceState::ObservedPassing => EvidenceState::ObservedPassing,
        req::RequirementEvidenceState::Diverged => EvidenceState::Diverged,
        req::RequirementEvidenceState::Reverted => EvidenceState::Reverted,
        req::RequirementEvidenceState::Mixed => EvidenceState::Mixed,
    }
}
fn obligation_state(state: ob::VerificationObligationEvidenceState) -> EvidenceState {
    match state {
        ob::VerificationObligationEvidenceState::NoLinkedChange => EvidenceState::NoLinkedChange,
        ob::VerificationObligationEvidenceState::Pending => EvidenceState::Pending,
        ob::VerificationObligationEvidenceState::ObservedPassing => EvidenceState::ObservedPassing,
        ob::VerificationObligationEvidenceState::ObservedFailing => EvidenceState::ObservedFailing,
        ob::VerificationObligationEvidenceState::Stale => EvidenceState::Stale,
        ob::VerificationObligationEvidenceState::Diverged => EvidenceState::Diverged,
        ob::VerificationObligationEvidenceState::Reverted => EvidenceState::Reverted,
        ob::VerificationObligationEvidenceState::Mixed => EvidenceState::Mixed,
    }
}
fn change_state(state: query::ActiveChangeState) -> ChangeState {
    match state {
        query::ActiveChangeState::Active => ChangeState::Active,
        query::ActiveChangeState::Superseded => ChangeState::Superseded,
        query::ActiveChangeState::Diverged => ChangeState::Diverged,
        query::ActiveChangeState::Missing => ChangeState::Missing,
        query::ActiveChangeState::Reverted => ChangeState::Reverted,
    }
}
pub(super) fn change_label(state: ChangeState) -> &'static str {
    match state {
        ChangeState::Active => "active",
        ChangeState::Superseded => "superseded",
        ChangeState::Diverged => "diverged",
        ChangeState::Missing => "missing",
        ChangeState::Reverted => "reverted",
        ChangeState::Unavailable => "unavailable",
    }
}
