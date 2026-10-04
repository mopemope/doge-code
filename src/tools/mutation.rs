//! Unified mutation transactions for workspace text changes.
//!
//! Every major text mutation (`fs_write`, `edit`, `apply_patch`, semantic
//! edit, `undo`) flows through the same lifecycle:
//!
//! ```text
//! exact before snapshot
//!   -> candidate generation (tool-specific)
//!   -> pre-write re-read + race check
//!   -> sibling temp file + persist
//!   -> read-back verify
//!   -> exact after snapshot
//!   -> MutationReceipt
//!   -> shared commit bookkeeping (undo / session / provenance)
//! ```
//!
//! The ground truth for provenance is the observed `before -> after`
//! transaction, never the LLM's tool call. No-op and failed mutations
//! produce no receipt, no undo entry, and no provenance event.
//!
//! The pre-write re-read narrows the race window but does not claim a
//! filesystem-level CAS: external processes can still race a rename on a
//! general filesystem. Doge-internal write concurrency is prevented by
//! job ownership on top of this check. Never describe this as race-free.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::provenance::{ChangeKind, file_content_hash};

/// In-memory exact file snapshot (never persisted verbatim to provenance).
#[derive(Debug, Clone)]
pub struct MutationSnapshot {
    pub exists: bool,
    pub content: Option<String>,
    pub content_hash: Option<String>,
    pub byte_len: Option<u64>,
}

impl MutationSnapshot {
    pub fn missing() -> Self {
        Self {
            exists: false,
            content: None,
            content_hash: None,
            byte_len: None,
        }
    }

    pub fn state_matches(&self, other: &Self) -> bool {
        if self.exists != other.exists {
            return false;
        }
        if !self.exists {
            return true;
        }
        self.content_hash == other.content_hash
    }

    pub fn content_or_empty(&self) -> &str {
        self.content.as_deref().unwrap_or("")
    }
}

/// In-memory target receipt (kept separate from the durable provenance type
/// so execution and storage do not over-couple).
#[derive(Debug, Clone)]
pub enum MutationTargetReceipt {
    File,
    SemanticSymbol {
        symbol_id: String,
        before_fingerprint: String,
        after_fingerprint: String,
    },
}

/// Observed `before -> after` transaction for one file.
#[derive(Debug, Clone)]
pub struct MutationReceipt {
    pub kind: ChangeKind,
    pub path: PathBuf,
    pub before: MutationSnapshot,
    pub after: MutationSnapshot,
    pub target: MutationTargetReceipt,
    pub diff: String,
    pub lines_added: usize,
    pub lines_removed: usize,
}

/// Tool result paired with its optional mutation receipt.
///
/// `receipt = None` means no-op (success without a mutation). Failures never
/// produce a receipt.
#[derive(Debug, Clone)]
pub struct MutationExecution<T> {
    pub result: T,
    pub receipt: Option<MutationReceipt>,
}

/// Typed commit errors. Never distinguished by string matching.
#[derive(Debug, Error)]
pub enum MutationCommitError {
    #[error("file changed during commit; refusing to overwrite")]
    ConcurrentModification,
    #[error("failed to write file: {0}")]
    WriteFailed(String),
    #[error("read-back verification failed: {0}")]
    VerifyFailed(String),
}

/// Read an exact text snapshot of `path`.
///
/// - Missing file: missing snapshot (`exists = false`).
/// - Directory: error.
/// - Binary / non-UTF8: error (text tools reject these like before).
pub fn read_text_snapshot(path: &Path) -> Result<MutationSnapshot, MutationCommitError> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(MutationSnapshot::missing()),
        Err(e) => Err(MutationCommitError::WriteFailed(format!(
            "failed to stat {}: {e}",
            path.display()
        ))),
        Ok(meta) => {
            if meta.is_dir() {
                return Err(MutationCommitError::WriteFailed(format!(
                    "path is a directory: {}",
                    path.display()
                )));
            }
            let bytes = std::fs::read(path).map_err(|e| {
                MutationCommitError::WriteFailed(format!("failed to read {}: {e}", path.display()))
            })?;
            if bytes.contains(&0) {
                return Err(MutationCommitError::WriteFailed(
                    "binary content is not allowed".to_string(),
                ));
            }
            let content = String::from_utf8(bytes).map_err(|_| {
                MutationCommitError::WriteFailed(format!(
                    "file is not valid UTF-8: {}",
                    path.display()
                ))
            })?;
            let byte_len = content.len() as u64;
            let content_hash = file_content_hash(&content);
            Ok(MutationSnapshot {
                exists: true,
                content: Some(content),
                content_hash: Some(content_hash),
                byte_len: Some(byte_len),
            })
        }
    }
}

/// Async variant for async callers (same semantics as [`read_text_snapshot`]).
pub async fn read_text_snapshot_async(
    path: &Path,
) -> Result<MutationSnapshot, MutationCommitError> {
    match tokio::fs::symlink_metadata(path).await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(MutationSnapshot::missing()),
        Err(e) => Err(MutationCommitError::WriteFailed(format!(
            "failed to stat {}: {e}",
            path.display()
        ))),
        Ok(meta) => {
            if meta.is_dir() {
                return Err(MutationCommitError::WriteFailed(format!(
                    "path is a directory: {}",
                    path.display()
                )));
            }
            let bytes = tokio::fs::read(path).await.map_err(|e| {
                MutationCommitError::WriteFailed(format!("failed to read {}: {e}", path.display()))
            })?;
            if bytes.contains(&0) {
                return Err(MutationCommitError::WriteFailed(
                    "binary content is not allowed".to_string(),
                ));
            }
            let content = String::from_utf8(bytes).map_err(|_| {
                MutationCommitError::WriteFailed(format!(
                    "file is not valid UTF-8: {}",
                    path.display()
                ))
            })?;
            let byte_len = content.len() as u64;
            let content_hash = file_content_hash(&content);
            Ok(MutationSnapshot {
                exists: true,
                content: Some(content),
                content_hash: Some(content_hash),
                byte_len: Some(byte_len),
            })
        }
    }
}

/// Canonicalize a path for scope comparison.
///
/// Tools scope-check canonicalized paths (resolving `..` and symlinks), but
/// receipts carry the raw user-supplied path. Comparing the raw path
/// lexically would misclassify e.g. `/proj/sub/../a.txt` as outside the
/// project. Canonicalize the file when it exists, otherwise the parent
/// directory joined with the file name (covers deleted files on the undo
/// path). Never fails; falls back to the raw path.
pub fn canonicalize_for_scope(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    if let Some(parent) = path.parent()
        && let Ok(canonical_parent) = parent.canonicalize()
    {
        if let Some(name) = path.file_name() {
            return canonical_parent.join(name);
        }
        return canonical_parent;
    }
    path.to_path_buf()
}

/// True when `after_content` differs from `before` (exact bytes).
///
/// Identical content means no-op: no receipt, no undo, no provenance.
pub fn mutation_changed(before: &MutationSnapshot, after_content: &str) -> bool {
    if !before.exists {
        return true;
    }
    match &before.content {
        Some(content) => content.as_str() != after_content,
        None => true,
    }
}

/// Full diff plus `(lines_added, lines_removed)` computed once from it.
///
/// The full diff is for durable provenance; tool outputs must budget it
/// separately. Never store a truncated diff as commit evidence.
pub fn mutation_diff_and_stats(
    before_content: &str,
    after_content: &str,
) -> (String, usize, usize) {
    let diff = diffy::create_patch(before_content, after_content).to_string();
    let (added, removed) = count_diff_lines(&diff);
    (diff, added, removed)
}

/// Count `+`/`-` lines in a unified diff, excluding `+++`/`---` headers.
pub fn count_diff_lines(diff_text: &str) -> (usize, usize) {
    let mut added = 0usize;
    let mut removed = 0usize;
    for line in diff_text.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

/// Build a receipt from observed before/after states (no I/O).
pub fn build_receipt(
    kind: ChangeKind,
    path: PathBuf,
    before: MutationSnapshot,
    after: MutationSnapshot,
    target: MutationTargetReceipt,
) -> MutationReceipt {
    let (diff, lines_added, lines_removed) =
        mutation_diff_and_stats(before.content_or_empty(), after.content_or_empty());
    MutationReceipt {
        kind,
        path,
        before,
        after,
        target,
        diff,
        lines_added,
        lines_removed,
    }
}

/// Commit `candidate` for `path` after verifying `before` is still current.
///
/// Algorithm: pre-write re-read -> compare `exists` + content with `before`
/// -> sibling temp + write + fsync -> persist (no-clobber for new files) ->
/// read-back verify -> after snapshot. Any race yields
/// [`MutationCommitError::ConcurrentModification`] without touching disk
/// beyond the temp file.
pub async fn commit_text_candidate(
    path: &Path,
    before: &MutationSnapshot,
    candidate: &str,
) -> Result<MutationSnapshot, MutationCommitError> {
    if candidate.as_bytes().contains(&0) {
        return Err(MutationCommitError::WriteFailed(
            "binary content is not allowed".to_string(),
        ));
    }

    tracing::info!(
        file = %path.display(),
        before_hash = before.content_hash.as_deref().unwrap_or("missing"),
        "mutation.prepare"
    );

    // Pre-write re-read: refuse when the file moved under us.
    let current = read_text_snapshot_async(path).await?;
    if !current.state_matches(before) {
        tracing::warn!(file = %path.display(), "mutation.concurrent_modification");
        return Err(MutationCommitError::ConcurrentModification);
    }

    // New-file race: `before` said missing but something appeared (covered
    // by state_matches above, kept explicit for clarity).
    let is_new_file = !before.exists;

    let parent = path.parent().ok_or_else(|| {
        MutationCommitError::WriteFailed(format!("no parent directory: {}", path.display()))
    })?;
    tokio::fs::create_dir_all(parent).await.map_err(|e| {
        MutationCommitError::WriteFailed(format!(
            "failed to create parent for {}: {e}",
            path.display()
        ))
    })?;

    // Preserve source permissions on replace so temp+rename does not widen
    // or narrow modes beyond what was there.
    let original_permissions = if !is_new_file {
        tokio::fs::metadata(path)
            .await
            .ok()
            .map(|m| m.permissions())
    } else {
        None
    };

    // Sibling temp file in the destination directory.
    let mut temp = tempfile::Builder::new()
        .prefix(".tmp_mutation_")
        .tempfile_in(parent)
        .map_err(|e| MutationCommitError::WriteFailed(format!("temp file: {e}")))?;
    {
        use std::io::Write as _;
        // Apply preserved permissions before persisting.
        if let Some(perms) = original_permissions.as_ref() {
            let _ = temp.as_file().set_permissions(perms.clone());
        }
        temp.write_all(candidate.as_bytes())
            .map_err(|e| MutationCommitError::WriteFailed(format!("temp write: {e}")))?;
        temp.as_file_mut()
            .sync_all()
            .map_err(|e| MutationCommitError::WriteFailed(format!("temp fsync: {e}")))?;
    }

    if is_new_file {
        temp.persist_noclobber(path).map_err(|e| {
            // Another writer won the creation race.
            let msg = e.to_string();
            if msg.contains("already exists") || path.exists() {
                MutationCommitError::ConcurrentModification
            } else {
                MutationCommitError::WriteFailed(format!("persist: {e}"))
            }
        })?;
    } else {
        temp.persist(path)
            .map_err(|e| MutationCommitError::WriteFailed(format!("persist: {e}")))?;
    }

    // Read-back verify: the file must now contain exactly the candidate.
    let after_bytes = tokio::fs::read(path).await.map_err(|e| {
        MutationCommitError::VerifyFailed(format!("read-back failed for {}: {e}", path.display()))
    })?;
    if after_bytes != candidate.as_bytes() {
        return Err(MutationCommitError::VerifyFailed(format!(
            "content mismatch after commit for {}",
            path.display()
        )));
    }
    let after_content = String::from_utf8(after_bytes).map_err(|_| {
        MutationCommitError::VerifyFailed(format!("non-UTF8 after commit: {}", path.display()))
    })?;
    let after = MutationSnapshot {
        exists: true,
        byte_len: Some(after_content.len() as u64),
        content_hash: Some(file_content_hash(&after_content)),
        content: Some(after_content),
    };

    tracing::info!(
        file = %path.display(),
        before_hash = before.content_hash.as_deref().unwrap_or("missing"),
        after_hash = after.content_hash.as_deref().unwrap_or("missing"),
        "mutation.commit"
    );
    Ok(after)
}

/// Synchronous variant for sync callers (same race-check + verify semantics).
pub fn commit_text_candidate_blocking(
    path: &Path,
    before: &MutationSnapshot,
    candidate: &str,
) -> Result<MutationSnapshot, MutationCommitError> {
    if candidate.as_bytes().contains(&0) {
        return Err(MutationCommitError::WriteFailed(
            "binary content is not allowed".to_string(),
        ));
    }
    let current = read_text_snapshot(path)?;
    if !current.state_matches(before) {
        return Err(MutationCommitError::ConcurrentModification);
    }
    let is_new_file = !before.exists;
    let parent = path.parent().ok_or_else(|| {
        MutationCommitError::WriteFailed(format!("no parent directory: {}", path.display()))
    })?;
    std::fs::create_dir_all(parent).map_err(|e| {
        MutationCommitError::WriteFailed(format!(
            "failed to create parent for {}: {e}",
            path.display()
        ))
    })?;
    let original_permissions = if !is_new_file {
        std::fs::metadata(path).ok().map(|m| m.permissions())
    } else {
        None
    };
    let mut temp = tempfile::Builder::new()
        .prefix(".tmp_mutation_")
        .tempfile_in(parent)
        .map_err(|e| MutationCommitError::WriteFailed(format!("temp file: {e}")))?;
    {
        use std::io::Write as _;
        if let Some(perms) = original_permissions.as_ref() {
            let _ = temp.as_file().set_permissions(perms.clone());
        }
        temp.write_all(candidate.as_bytes())
            .map_err(|e| MutationCommitError::WriteFailed(format!("temp write: {e}")))?;
        temp.as_file_mut()
            .sync_all()
            .map_err(|e| MutationCommitError::WriteFailed(format!("temp fsync: {e}")))?;
    }
    if is_new_file {
        temp.persist_noclobber(path).map_err(|e| {
            let msg = e.to_string();
            if msg.contains("already exists") || path.exists() {
                MutationCommitError::ConcurrentModification
            } else {
                MutationCommitError::WriteFailed(format!("persist: {e}"))
            }
        })?;
    } else {
        temp.persist(path)
            .map_err(|e| MutationCommitError::WriteFailed(format!("persist: {e}")))?;
    }
    let after_bytes = std::fs::read(path).map_err(|e| {
        MutationCommitError::VerifyFailed(format!("read-back failed for {}: {e}", path.display()))
    })?;
    if after_bytes != candidate.as_bytes() {
        return Err(MutationCommitError::VerifyFailed(format!(
            "content mismatch after commit for {}",
            path.display()
        )));
    }
    let after_content = String::from_utf8(after_bytes).map_err(|_| {
        MutationCommitError::VerifyFailed(format!("non-UTF8 after commit: {}", path.display()))
    })?;
    Ok(MutationSnapshot {
        exists: true,
        byte_len: Some(after_content.len() as u64),
        content_hash: Some(file_content_hash(&after_content)),
        content: Some(after_content),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_existing_missing_unicode_crlf_empty() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        // Missing.
        let snap = read_text_snapshot(&file).unwrap();
        assert!(!snap.exists);
        assert_eq!(snap.content_hash, None);
        // Empty file.
        std::fs::write(&file, "").unwrap();
        let snap = read_text_snapshot(&file).unwrap();
        assert!(snap.exists);
        assert_eq!(snap.content.as_deref(), Some(""));
        assert_eq!(snap.byte_len, Some(0));
        // Unicode.
        std::fs::write(&file, "héllo 🦀\n").unwrap();
        let snap = read_text_snapshot(&file).unwrap();
        assert!(snap.exists);
        assert!(snap.content_hash.unwrap().starts_with("blake3:"));
        // CRLF differs from LF (exact bytes).
        let lf_hash = file_content_hash("a\n");
        let crlf_hash = file_content_hash("a\r\n");
        assert_ne!(lf_hash, crlf_hash);
        // Directory errors.
        assert!(read_text_snapshot(dir.path()).is_err());
    }

    #[test]
    fn test_mutation_changed_noop_detection() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "hello\n").unwrap();
        let before = read_text_snapshot(&file).unwrap();
        assert!(!mutation_changed(&before, "hello\n"));
        assert!(mutation_changed(&before, "hello!\n"));
        assert!(mutation_changed(&MutationSnapshot::missing(), ""));
    }

    #[test]
    fn test_commit_new_file_and_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("new.txt");
        let before = read_text_snapshot(&file).unwrap();
        assert!(!before.exists);
        let after = commit_text_candidate_blocking(&file, &before, "hello\n").unwrap();
        assert!(after.exists);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello\n");
        // Identical rewrite detected as no-op by caller helper.
        let before2 = read_text_snapshot(&file).unwrap();
        assert!(!mutation_changed(&before2, "hello\n"));
    }

    #[test]
    fn test_commit_race_rejects_stale_before() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "v1\n").unwrap();
        let before = read_text_snapshot(&file).unwrap();
        // External change before commit.
        std::fs::write(&file, "v2-external\n").unwrap();
        let err = commit_text_candidate_blocking(&file, &before, "v3\n").unwrap_err();
        assert!(matches!(err, MutationCommitError::ConcurrentModification));
        // Loser did not clobber the external write.
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "v2-external\n");
    }

    #[test]
    fn test_new_file_race_rejects_when_created() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("race.txt");
        let before = MutationSnapshot::missing();
        std::fs::write(&file, "someone created\n").unwrap();
        let err = commit_text_candidate_blocking(&file, &before, "mine\n").unwrap_err();
        assert!(matches!(err, MutationCommitError::ConcurrentModification));
    }

    #[test]
    fn test_diff_stats_single_computation() {
        let (diff, added, removed) = mutation_diff_and_stats("a\nb\n", "a\nc\n");
        assert!(diff.contains("-b"));
        assert!(diff.contains("+c"));
        assert_eq!((added, removed), (1, 1));
    }

    #[tokio::test]
    async fn test_commit_async_matches_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "v1\n").unwrap();
        let before = read_text_snapshot(&file).unwrap();
        let after = commit_text_candidate(&file, &before, "v2\n").await.unwrap();
        assert_eq!(after.content.as_deref(), Some("v2\n"));
    }
}

#[cfg(test)]
mod finalize_integration_tests {
    use super::*;
    use crate::provenance::{ChangeKind, ProvenanceEvent};
    use crate::tools::{FinalizeMutationOptions, FsTools};

    fn fs_with_project() -> (tempfile::TempDir, FsTools) {
        let proj = tempfile::tempdir().unwrap();
        let sessions_root = proj.path().join(".doge/sessions");
        let store = crate::session::SessionStore::new(sessions_root).unwrap();
        let manager = std::sync::Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            save_state: Default::default(),
            current_lease: None,
            store,
            current_session: None,
        }));
        {
            let mut mgr = manager.lock().unwrap();
            mgr.create_session(None).unwrap();
        }
        let config = std::sync::Arc::new(crate::config::AppConfig {
            project_root: proj.path().to_path_buf(),
            ..crate::config::AppConfig::default()
        });
        let fs = FsTools::new(std::sync::Arc::new(tokio::sync::RwLock::new(None)), config)
            .with_session_manager(manager);
        (proj, fs)
    }

    fn recorded_changes(fs: &FsTools) -> Vec<crate::provenance::ProvenanceEventEnvelope> {
        crate::tools::provenance::load_current_events(fs)
            .unwrap()
            .unwrap()
            .events
    }

    #[tokio::test]
    async fn test_fs_write_finalize_records_file_write() {
        let (proj, fs) = fs_with_project();
        let file = proj.path().join("a.txt");
        std::fs::write(&file, "old\n").unwrap();
        let res = fs.fs_write(file.to_str().unwrap(), "new\n").await.unwrap();
        assert!(res.changed);
        let events = recorded_changes(&fs);
        assert_eq!(events.len(), 1);
        match &events[0].event {
            ProvenanceEvent::ChangeCommitted(c) => {
                assert_eq!(c.change_kind, ChangeKind::FileWrite);
                assert!(matches!(c.target, crate::provenance::ChangeTarget::File));
                assert!(c.before.content_hash.is_some());
                assert!(c.after.content_hash.is_some());
            }
            _ => panic!("expected change"),
        }
        // Undo stack has exactly one entry; session tracks the file.
        assert_eq!(fs.undo_stack.read().await.len(), 1);
        assert!(
            fs.get_session_changed_files()
                .iter()
                .any(|p| p.to_string_lossy() == "a.txt")
        );
    }

    #[tokio::test]
    async fn test_fs_write_noop_records_nothing() {
        let (proj, fs) = fs_with_project();
        let file = proj.path().join("a.txt");
        std::fs::write(&file, "same\n").unwrap();
        let res = fs.fs_write(file.to_str().unwrap(), "same\n").await.unwrap();
        assert!(!res.changed);
        let events = recorded_changes(&fs);
        assert!(events.is_empty());
        assert_eq!(fs.undo_stack.read().await.len(), 0);
    }

    #[tokio::test]
    async fn test_cross_tool_chain_edit_patch_semantic() {
        let (proj, fs) = fs_with_project();
        // Seed a Rust file for the semantic step.
        std::fs::create_dir_all(proj.path().join("src")).unwrap();
        std::fs::write(proj.path().join("src/lib.rs"), "fn foo() {\n    1;\n}\n").unwrap();
        let file = proj.path().join("src/lib.rs");

        // 1. edit: 1; -> 2;
        let edit_exec = crate::tools::edit::edit_with_receipt(
            crate::tools::edit::EditParams {
                file_path: file.to_str().unwrap().to_string(),
                target_block: "    1;".to_string(),
                new_block: "    2;".to_string(),
                start_line: None,
                end_line: None,
                allow_multiple: None,
            },
            &fs.config,
        )
        .await
        .unwrap();
        assert!(edit_exec.result.changed);
        let edit_receipt = edit_exec.receipt.unwrap();
        let rep1 = fs
            .finalize_mutation(
                edit_receipt,
                FinalizeMutationOptions {
                    record_undo: true,
                    reverts_change_id: None,
                    attribution: Default::default(),
                },
            )
            .await;
        assert!(rep1.change_id.is_some());

        // 2. apply_patch: 2; -> 3;
        let before2 = std::fs::read_to_string(&file).unwrap();
        let after2 = before2.replacen("    2;", "    3;", 1);
        let patch = diffy::create_patch(&before2, &after2).to_string();
        let patch_exec = crate::tools::apply_patch::apply_patch_with_recovery_and_receipt(
            crate::tools::apply_patch::ApplyPatchParams {
                file_path: file.to_str().unwrap().to_string(),
                patch_content: patch,
            },
            &fs.config,
        )
        .await
        .unwrap();
        assert!(patch_exec.result.changed);
        let patch_receipt = patch_exec.receipt.unwrap();
        let rep2 = fs
            .finalize_mutation(
                patch_receipt,
                FinalizeMutationOptions {
                    record_undo: true,
                    reverts_change_id: None,
                    attribution: Default::default(),
                },
            )
            .await;
        assert!(rep2.change_id.is_some());

        // 3. semantic edit: 3; -> 4;
        let root = proj.path().to_path_buf();
        let prepared = crate::features::semantic_edit::prepare_edit(&root, &file, 2).unwrap();
        let (result, _, before_content) =
            crate::features::semantic_edit::apply_edit(&prepared, "fn foo() {\n    4;\n}\n", &root)
                .await
                .unwrap();
        let after_content = std::fs::read_to_string(&file).unwrap();
        let sem_receipt = MutationReceipt {
            kind: ChangeKind::SemanticEdit,
            path: file.clone(),
            before: MutationSnapshot {
                exists: true,
                content: Some(before_content),
                content_hash: result.before_file_hash.clone(),
                byte_len: result.before_byte_len,
            },
            after: MutationSnapshot {
                exists: true,
                content: Some(after_content),
                content_hash: result.after_file_hash.clone(),
                byte_len: result.after_byte_len,
            },
            target: MutationTargetReceipt::SemanticSymbol {
                symbol_id: result.symbol_id.as_str().to_string(),
                before_fingerprint: result.before_fingerprint.as_str().to_string(),
                after_fingerprint: result.after_fingerprint.as_str().to_string(),
            },
            diff: result.diff.clone(),
            lines_added: result.lines_added,
            lines_removed: result.lines_removed,
        };
        let rep3 = fs
            .finalize_mutation(
                sem_receipt,
                FinalizeMutationOptions {
                    record_undo: true,
                    reverts_change_id: None,
                    attribution: Default::default(),
                },
            )
            .await;
        assert!(rep3.change_id.is_some());

        // One continuous chain across tools.
        let events = recorded_changes(&fs);
        assert_eq!(events.len(), 3);
        let ids: Vec<String> = events.iter().map(|e| e.event_id.clone()).collect();
        let preds: Vec<Option<String>> = events
            .iter()
            .filter_map(|e| match &e.event {
                ProvenanceEvent::ChangeCommitted(c) => Some(c.predecessor_change_id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(preds[0], None);
        assert_eq!(preds[1], Some(ids[0].clone()));
        assert_eq!(preds[2], Some(ids[1].clone()));
        // Current chain resolves all three as active.
        let active = crate::provenance::active_change_ids(proj.path(), &events);
        assert_eq!(active.len(), 3);
    }

    #[tokio::test]
    async fn test_undo_new_file_deletes_and_records() {
        let (proj, fs) = fs_with_project();
        let file = proj.path().join("new.txt");
        let res = fs.fs_write(file.to_str().unwrap(), "data\n").await.unwrap();
        assert!(res.changed);
        assert!(file.exists());
        let undo = crate::tools::undo::undo(&fs).await.unwrap();
        assert!(undo.success);
        assert!(undo.changed);
        assert!(!file.exists(), "undo of a created file must delete it");
        // Undo itself is a tracked mutation.
        let events = recorded_changes(&fs);
        assert_eq!(events.len(), 2);
        match &events[1].event {
            ProvenanceEvent::ChangeCommitted(c) => {
                assert_eq!(c.change_kind, ChangeKind::Undo);
                assert_eq!(c.reverts_change_id, Some(events[0].event_id.clone()));
                assert_eq!(c.predecessor_change_id, Some(events[0].event_id.clone()));
            }
            _ => panic!("expected undo event"),
        }
    }

    #[tokio::test]
    async fn test_stale_undo_conflicts_and_retains_entry() {
        let (proj, fs) = fs_with_project();
        let file = proj.path().join("a.txt");
        std::fs::write(&file, "v0\n").unwrap();
        fs.fs_write(file.to_str().unwrap(), "v1\n").await.unwrap();
        // External modification after the tracked mutation.
        std::fs::write(&file, "manual\n").unwrap();
        let undo = crate::tools::undo::undo(&fs).await.unwrap();
        assert!(!undo.success);
        assert!(undo.conflict);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "manual\n");
        // Entry retained for a later retry.
        assert_eq!(fs.undo_stack.read().await.len(), 1);
    }

    #[tokio::test]
    async fn test_empty_stack_undo_is_failure() {
        let (_proj, fs) = fs_with_project();
        let undo = crate::tools::undo::undo(&fs).await.unwrap();
        assert!(!undo.success);
        assert!(!undo.changed);
    }

    #[tokio::test]
    async fn test_provenance_failure_keeps_undo_and_marks_session() {
        let (proj, fs) = fs_with_project();
        let file = proj.path().join("a.txt");
        std::fs::write(&file, "v0\n").unwrap();
        // Break only the v2 events dir (a file where the dir should be).
        let ctx = fs.current_session_storage_context().unwrap();
        let events_dir =
            crate::provenance::ProvenanceStore::new(ctx.session_dir.clone()).current_events_path();
        std::fs::create_dir_all(events_dir.parent().unwrap()).unwrap();
        std::fs::write(&events_dir, "not-a-dir").unwrap();
        let res = fs.fs_write(file.to_str().unwrap(), "v1\n").await.unwrap();
        // Source still committed; tool reports success with a warning.
        assert!(res.changed);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "v1\n");
        assert!(res.warnings.iter().any(|w| w.contains("provenance")));
        let sess = fs.get_current_session().unwrap();
        assert!(sess.provenance_incomplete);
        // Undo entry still exists despite the provenance failure.
        assert_eq!(fs.undo_stack.read().await.len(), 1);
    }

    #[tokio::test]
    async fn test_verification_observes_current_chain() {
        let (proj, fs) = fs_with_project();
        let file = proj.path().join("a.txt");
        std::fs::write(&file, "h0\n").unwrap();
        fs.fs_write(file.to_str().unwrap(), "h1\n").await.unwrap();
        fs.fs_write(file.to_str().unwrap(), "h2\n").await.unwrap();
        let events = recorded_changes(&fs);
        let active = crate::provenance::active_change_ids(proj.path(), &events);
        assert_eq!(active.len(), 2);
        // A verification snapshot captures the whole chain.
        let ctx = crate::provenance::VerificationContext {
            execution_workspace: None,
            plan_item_id: None,
            observed_change_ids: active.clone(),
            matched_obligations: Vec::new(),
            directive_id: None,
            requirement_ids: Vec::new(),
        };
        let event = crate::provenance::build_verification_event(
            crate::provenance::VerificationRecordInput {
                kind: crate::provenance::VerificationKind::Test,
                source: crate::provenance::VerificationSource::ExecuteProcess,
                program: "cargo",
                args: &["test".to_string()],
                cwd_relative: None,
                success: true,
                status: "completed",
                exit_code: Some(0),
                timed_out: false,
                stdout: "ok",
                stderr: "",
                capture_truncated: false,
                context: ctx,
                extra_warnings: vec![],
            },
        );
        assert_eq!(event.observed_change_ids, active);
    }

    #[tokio::test]
    async fn test_verification_after_undo_observes_undo_not_reverted() {
        let (proj, fs) = fs_with_project();
        let file = proj.path().join("a.txt");
        std::fs::write(&file, "h0\n").unwrap();
        fs.fs_write(file.to_str().unwrap(), "h1\n").await.unwrap();
        let before_undo = recorded_changes(&fs);
        let a_id = before_undo[0].event_id.clone();
        crate::tools::undo::undo(&fs).await.unwrap();
        let events = recorded_changes(&fs);
        let active = crate::provenance::active_change_ids(proj.path(), &events);
        assert!(!active.contains(&a_id));
        assert_eq!(active.len(), 1);
        let cov = crate::provenance::compute_coverage(proj.path(), &events, &[], false);
        assert!(cov.reverted_change_ids.contains(&a_id));
    }

    #[tokio::test]
    async fn test_outside_allowed_path_skips_provenance_not_failure() {
        let (proj, mut fs) = fs_with_project();
        let outside = tempfile::tempdir().unwrap();
        let outside_file = outside.path().join("x.txt");
        // Allow the outside dir explicitly.
        fs.config = std::sync::Arc::new(crate::config::AppConfig {
            project_root: proj.path().to_path_buf(),
            allowed_paths: vec![outside.path().to_path_buf()],
            ..crate::config::AppConfig::default()
        });
        // Direct receipt path: build and finalize an outside mutation.
        let before_missing = MutationSnapshot::missing();
        let after = commit_text_candidate(&outside_file, &before_missing, "hello\n")
            .await
            .unwrap();
        let receipt = build_receipt(
            ChangeKind::FileWrite,
            outside_file.clone(),
            before_missing,
            after,
            MutationTargetReceipt::File,
        );
        let report = fs
            .finalize_mutation(
                receipt,
                FinalizeMutationOptions {
                    record_undo: true,
                    reverts_change_id: None,
                    attribution: Default::default(),
                },
            )
            .await;
        assert!(report.change_id.is_none());
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("outside project root"))
        );
        assert!(!fs.get_current_session().unwrap().provenance_incomplete);
        // Undo still works for outside paths.
        assert_eq!(fs.undo_stack.read().await.len(), 1);
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;
    use crate::provenance::ProvenanceEvent;
    use crate::tools::{FinalizeMutationOptions, FsTools};

    fn fs_with_project() -> (tempfile::TempDir, FsTools) {
        let proj = tempfile::tempdir().unwrap();
        let sessions_root = proj.path().join(".doge/sessions");
        let store = crate::session::SessionStore::new(sessions_root).unwrap();
        let manager = std::sync::Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            save_state: Default::default(),
            current_lease: None,
            store,
            current_session: None,
        }));
        {
            let mut mgr = manager.lock().unwrap();
            mgr.create_session(None).unwrap();
        }
        let config = std::sync::Arc::new(crate::config::AppConfig {
            project_root: proj.path().to_path_buf(),
            ..crate::config::AppConfig::default()
        });
        let fs = FsTools::new(std::sync::Arc::new(tokio::sync::RwLock::new(None)), config)
            .with_session_manager(manager);
        (proj, fs)
    }

    #[test]
    fn test_canonicalize_for_scope_resolves_dotdot() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
        let dotted = dir.path().join("sub").join("..").join("a.txt");
        // Lexical comparison would fail; canonical comparison must succeed.
        assert!(!dotted.starts_with(dir.path().join("a.txt")));
        assert_eq!(
            canonicalize_for_scope(&dotted),
            canonicalize_for_scope(&dir.path().join("a.txt"))
        );
    }

    #[tokio::test]
    async fn test_dotdot_path_stays_tracked() {
        let (proj, fs) = fs_with_project();
        std::fs::create_dir_all(proj.path().join("sub")).unwrap();
        std::fs::write(proj.path().join("a.txt"), "v0\n").unwrap();
        let dotted = proj.path().join("sub").join("..").join("a.txt");
        let res = fs.fs_write(dotted.to_str().unwrap(), "v1\n").await.unwrap();
        assert!(res.changed);
        assert!(
            !res.warnings.iter().any(|w| w.contains("outside project")),
            "warnings: {:?}",
            res.warnings
        );
        let events = crate::tools::provenance::load_current_events(&fs)
            .unwrap()
            .unwrap()
            .events;
        assert_eq!(events.len(), 1);
        match &events[0].event {
            ProvenanceEvent::ChangeCommitted(c) => assert_eq!(c.file, "a.txt"),
            _ => panic!("expected change"),
        }
        assert!(
            fs.get_session_changed_files()
                .iter()
                .any(|p| p.to_string_lossy() == "a.txt")
        );
        let _ = FinalizeMutationOptions::default();
    }
}
