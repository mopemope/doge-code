//! Read-only, session-scoped evidence exports. No LLM or provenance writes.
mod collect;
mod handoff;
mod model;
mod render;
mod workspace;

use crate::session::SessionStore;
pub use model::{EvidenceReport, ReportFormat};
use std::path::Path;
use workspace::GitReader;

const MAX_ITEMS: usize = 10_000;
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_INPUT_BYTES: usize = 128 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 32 * 1024 * 1024;

type Result<T> = std::result::Result<T, ReportError>;

#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("evidence report read failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("evidence session read failed: {0}")]
    Session(#[from] crate::session::error::SessionError),
    #[error("evidence serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("evidence input could not be loaded")]
    Input(#[source] anyhow::Error),
    #[error("evidence report limit exceeded: {0}")]
    Limit(&'static str),
    #[error("evidence storage contains an unsafe path or mismatched session")]
    UnsafeInput,
    #[error("evidence report Git comparison failed: {0}")]
    Git(String),
    #[error(
        "workspace or saved evidence changed during both export attempts; retry when writers are idle"
    )]
    ConcurrentModification,
}

impl From<anyhow::Error> for ReportError {
    fn from(value: anyhow::Error) -> Self {
        Self::Input(value)
    }
}

pub async fn export(
    root: &Path,
    id: &str,
    base: Option<&str>,
    include_content: bool,
    format: ReportFormat,
) -> Result<String> {
    let report = build_with(
        root,
        id,
        base,
        include_content,
        chrono::Utc::now().to_rfc3339(),
        &GitReader::default(),
        &mut |_| {},
    )
    .await?;
    render::render(&report, format)
}

async fn build_with(
    root: &Path,
    prefix: &str,
    base: Option<&str>,
    include_content: bool,
    generated_at: String,
    git: &GitReader,
    between_reads: &mut dyn FnMut(usize),
) -> Result<EvidenceReport> {
    let root = root.canonicalize()?;
    let store_root = root.join(".doge/sessions");
    if !workspace::safe_path(&root, ".doge/sessions") {
        return Err(ReportError::UnsafeInput);
    }
    let store = SessionStore::open_existing(store_root)?;
    let id = store.resolve_id_prefix(prefix)?;
    if !workspace::valid_relative(&id) || id.contains('/') {
        return Err(ReportError::UnsafeInput);
    }
    for attempt in 0..2 {
        let (frozen, before) = collect::frozen_inputs(&root, &store, &id)?;
        let frozen_root = frozen.path().canonicalize()?;
        let frozen_store = SessionStore::open_existing(frozen_root.join(".doge/sessions"))?;
        let mut inputs = collect::load(&frozen_root, &frozen_store, &id)?;
        let git_before = git.capture(&root, base).await?;
        let paths = collect::paths(&mut inputs, &git_before);
        let snapshot = workspace::snapshot(&root, &paths, &git_before.unsupported)?;
        let query_files = tempfile::tempdir()?;
        if !workspace::freeze_query_files(&root, query_files.path(), &snapshot)? {
            continue;
        }
        let records: Vec<_> = inputs
            .events
            .iter()
            .filter_map(|e| match &e.event {
                crate::provenance::ProvenanceEvent::VerificationObserved(v) => {
                    v.execution_workspace.as_ref()
                }
                _ => None,
            })
            .collect();
        let execution_paths: std::collections::BTreeSet<String> = records
            .iter()
            .flat_map(|r| {
                r.start
                    .files
                    .iter()
                    .chain(r.end.iter().flat_map(|s| s.files.iter()))
            })
            .map(|f| f.path.clone())
            .collect();
        let current_execution = if records.is_empty() {
            None
        } else {
            Some(
                crate::features::verification_snapshot::capture(
                    &root,
                    execution_paths.clone(),
                    None,
                )
                .await,
            )
        };
        let report = collect::build(
            collect::ReportRoots {
                project: &root,
                query: query_files.path(),
            },
            inputs,
            git_before.clone(),
            snapshot.clone(),
            current_execution.as_ref(),
            include_content,
            generated_at.clone(),
        );
        between_reads(attempt);
        let after = collect::input_identity(&root, &store, &id)?;
        let git_after = git.capture(&root, base).await?;
        let files_after = workspace::snapshot(&root, &paths, &git_after.unsupported)?;
        let execution_after = if current_execution.is_some() {
            Some(
                crate::features::verification_snapshot::capture(&root, execution_paths, None).await,
            )
        } else {
            None
        };
        let execution_unchanged = match (&current_execution, &execution_after) {
            (Some(a), Some(b)) => a.same_observation(b),
            (None, None) => true,
            _ => false,
        };
        if before == after
            && git_before == git_after
            && snapshot == files_after
            && execution_unchanged
        {
            return Ok(report);
        }
    }
    Err(ReportError::ConcurrentModification)
}

#[cfg(test)]
mod tests;
