use crate::llm::types::{ToolDef, ToolFunctionDef};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;

use crate::tools::mutation::MutationSnapshot;

const UNDO_STACK_CAPACITY: usize = 20;

/// File existence + content captured before a committed mutation.
#[derive(Debug, Clone)]
pub enum UndoFileState {
    Missing,
    Text { content: String },
}

/// One undoable committed mutation.
///
/// `expected_after` is the exact state the file must still be in for the
/// undo to proceed. `before` is what we restore. `change_id` links the
/// undo's provenance `reverts_change_id`.
#[derive(Debug, Clone)]
pub struct BackupEntry {
    pub entry_id: String,
    pub path: PathBuf,
    pub before: UndoFileState,
    pub expected_after: crate::provenance::FileStateEvidence,
    pub change_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct UndoStack {
    stack: VecDeque<BackupEntry>,
}

impl UndoStack {
    pub fn new() -> Self {
        Self {
            stack: VecDeque::with_capacity(UNDO_STACK_CAPACITY),
        }
    }

    pub fn len(&self) -> usize {
        self.stack.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    /// Push a committed mutation. Only called after a successful commit;
    /// failed/no-op mutations never touch the stack.
    pub fn push_entry(&mut self, entry: BackupEntry) {
        if self.stack.len() >= UNDO_STACK_CAPACITY {
            self.stack.pop_front();
        }
        self.stack.push_back(entry);
    }

    pub(crate) fn remove_review_entry(
        &mut self,
        receipt: &crate::tools::mutation::MutationReceipt,
        change_id: Option<&str>,
    ) {
        if let Some(index) = self.stack.iter().rposition(|entry| {
            entry.path == receipt.path
                && entry.change_id.as_deref() == change_id
                && entry.expected_after.exists == receipt.after.exists
                && entry.expected_after.content_hash == receipt.after.content_hash
        }) {
            self.stack.remove(index);
        }
    }

    pub fn pop(&mut self) -> Option<BackupEntry> {
        self.stack.pop_back()
    }

    /// Peek at the most recent entry without removing it.
    pub fn peek_last(&self) -> Option<BackupEntry> {
        self.stack.back().cloned()
    }

    /// Remove the most recent entry after a successful restore.
    pub fn remove_last(&mut self) -> Option<BackupEntry> {
        self.stack.pop_back()
    }
}

pub fn undo_tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "undo".to_string(),
            description: "Safe LIFO rollback of the last tracked file mutation. Refuses to overwrite files changed since the mutation (conflict), deletes files that were created, and never pushes the undo itself back onto the stack. No redo.".to_string(),
            strict: Some(true),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
        },
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UndoResult {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub changed: bool,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub conflict: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_id: Option<String>,
}

pub async fn undo(fs_tools: &crate::tools::FsTools) -> Result<UndoResult> {
    undo_with_attribution(fs_tools, &crate::provenance::ProvenanceAttribution::none()).await
}

pub async fn undo_with_attribution(
    fs_tools: &crate::tools::FsTools,
    attribution: &crate::provenance::ProvenanceAttribution,
) -> Result<UndoResult> {
    use crate::provenance::{ChangeKind, FileStateEvidence};
    use crate::tools::mutation::{MutationTargetReceipt, commit_text_candidate};

    // Peek first; never pop before a successful restore.
    let entry = {
        let stack = fs_tools.undo_stack.read().await;
        stack.peek_last()
    };
    let Some(entry) = entry else {
        return Ok(UndoResult {
            success: false,
            changed: false,
            path: String::new(),
            message: "Undo stack is empty. Cannot revert any more changes.".to_string(),
            conflict: false,
            warnings: vec![],
            change_id: None,
        });
    };

    // Read current exact state once.
    let current = match current_file_evidence(&entry.path).await {
        Ok(s) => s,
        Err(e) => {
            return Ok(UndoResult {
                success: false,
                changed: false,
                path: entry.path.to_string_lossy().to_string(),
                message: format!("Undo failed to read current file state: {e}"),
                conflict: false,
                warnings: vec![],
                change_id: None,
            });
        }
    };

    // Stale guard: current must still equal the state committed earlier.
    if !evidence_matches(&current.0, &entry.expected_after) {
        tracing::warn!(
            file = %entry.path.display(),
            expected_hash = entry.expected_after.content_hash.as_deref().unwrap_or("missing"),
            "undo.conflict"
        );
        return Ok(UndoResult {
            success: false,
            changed: false,
            path: entry.path.to_string_lossy().to_string(),
            message: "Undo refused: the file changed since the tracked mutation (conflict). The current content was left untouched.".to_string(),
            conflict: true,
            warnings: vec![],
            change_id: None,
        });
    }

    // Restore: created file -> delete; otherwise atomic replace via the
    // common mutation writer.
    let before_snapshot = current.1;
    let receipt_kind = ChangeKind::Undo;
    let (restore_receipt_before, restore_receipt_after) = match &entry.before {
        UndoFileState::Missing => {
            // Delete the created file. Fail closed on any error.
            if let Err(e) = tokio::fs::remove_file(&entry.path).await {
                return Ok(UndoResult {
                    success: false,
                    changed: false,
                    path: entry.path.to_string_lossy().to_string(),
                    message: format!("Undo failed to delete created file: {e}"),
                    conflict: false,
                    warnings: vec![],
                    change_id: None,
                });
            }
            // Build evidence directly (no read-back race beyond remove).
            let after = FileStateEvidence::missing();
            (before_snapshot.clone(), after_to_snapshot(&after, None))
        }
        UndoFileState::Text { content } => {
            let after = match commit_text_candidate(&entry.path, &before_snapshot, content).await {
                Ok(after) => after,
                Err(crate::tools::mutation::MutationCommitError::ConcurrentModification) => {
                    return Ok(UndoResult {
                        success: false,
                        changed: false,
                        path: entry.path.to_string_lossy().to_string(),
                        message: "Undo refused: the file changed during the undo (conflict). The current content was left untouched.".to_string(),
                        conflict: true,
                        warnings: vec![],
                        change_id: None,
                    });
                }
                Err(e) => {
                    return Ok(UndoResult {
                        success: false,
                        changed: false,
                        path: entry.path.to_string_lossy().to_string(),
                        message: format!("Undo failed to restore file: {e}"),
                        conflict: false,
                        warnings: vec![],
                        change_id: None,
                    });
                }
            };
            (before_snapshot.clone(), after)
        }
    };

    // The restore succeeded: now remove the stack entry (never before).
    {
        let mut stack = fs_tools.undo_stack.write().await;
        // Remove only if the top is still our entry (no concurrent push/pop
        // confusion: compare entry_id when present).
        let top_matches = stack
            .peek_last()
            .is_some_and(|t| t.entry_id == entry.entry_id);
        if top_matches {
            stack.remove_last();
        } else {
            // Top moved (another mutation pushed concurrently); remove our
            // entry by id while preserving the remaining LIFO order.
            let mut kept: Vec<BackupEntry> = Vec::new();
            let mut removed = false;
            while let Some(e) = stack.pop() {
                if !removed && e.entry_id == entry.entry_id {
                    removed = true;
                    continue;
                }
                kept.push(e);
            }
            // `kept` is newest-first; reverse to restore oldest-first before
            // re-pushing so the stack order is unchanged.
            for e in kept.into_iter().rev() {
                stack.push_entry(e);
            }
        }
    }

    // Record the undo itself as provenance (ChangeKind::Undo) without
    // pushing a new undo entry. Failures here warn but never roll back.
    let target = MutationTargetReceipt::File;
    let (diff, lines_added, lines_removed) = crate::tools::mutation::mutation_diff_and_stats(
        restore_receipt_before.content_or_empty(),
        restore_receipt_after.content_or_empty(),
    );
    let receipt = crate::tools::mutation::MutationReceipt {
        kind: receipt_kind,
        path: entry.path.clone(),
        before: restore_receipt_before.clone(),
        after: restore_receipt_after.clone(),
        target,
        diff,
        lines_added,
        lines_removed,
    };

    let finalize_opts = crate::tools::FinalizeMutationOptions {
        record_undo: false,
        reverts_change_id: entry.change_id.clone(),
        attribution: attribution.clone(),
    };
    let report = fs_tools.finalize_mutation(receipt, finalize_opts).await;

    Ok(UndoResult {
        success: true,
        changed: true,
        path: entry.path.to_string_lossy().to_string(),
        message: format!(
            "Successfully restored {} to previous version.",
            entry.path.display()
        ),
        conflict: false,
        warnings: report.warnings,
        change_id: report.change_id,
    })
}

/// Current evidence + snapshot for the undo guard.
async fn current_file_evidence(
    path: &std::path::Path,
) -> Result<(crate::provenance::FileStateEvidence, MutationSnapshot)> {
    let snap = crate::tools::mutation::read_text_snapshot_async(path)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok((snapshot_to_evidence(&snap), snap))
}

fn snapshot_to_evidence(snap: &MutationSnapshot) -> crate::provenance::FileStateEvidence {
    crate::provenance::FileStateEvidence {
        exists: snap.exists,
        content_hash: snap.content_hash.clone(),
        byte_len: snap.byte_len,
    }
}

fn after_to_snapshot(
    evidence: &crate::provenance::FileStateEvidence,
    content: Option<String>,
) -> MutationSnapshot {
    MutationSnapshot {
        exists: evidence.exists,
        content,
        content_hash: evidence.content_hash.clone(),
        byte_len: evidence.byte_len,
    }
}

fn evidence_matches(
    current: &crate::provenance::FileStateEvidence,
    expected: &crate::provenance::FileStateEvidence,
) -> bool {
    // Legacy entries have no hash (content_hash = None, exists = true):
    // fall back to content comparison via snapshot when possible.
    // Here `current` always has a hash when the file exists, so a legacy
    // `expected` with None hash cannot prove equality -> treat as conflict
    // only when hashes are present on both sides; otherwise compare existence.
    if expected.content_hash.is_none() || current.content_hash.is_none() {
        // Fail closed when we cannot prove equality, except the both-missing
        // case which is trivially equal.
        if !expected.exists && !current.exists {
            return true;
        }
        // Legacy entries without hashes: allow the undo to proceed only if
        // both exist (best-effort back-compat). New entries always carry
        // hashes, so this path disappears as the stack turns over.
        return expected.exists && current.exists;
    }
    current.state_matches(expected)
}
