//! Bounded, turn-owned rollback evidence. Git state and UI filenames are never
//! restoration authority. Capture is independent of the bounded user undo stack.
use crate::{
    diff_review::DiffReviewPayload,
    tools::{
        FinalizeMutationOptions, FsTools,
        mutation::{self, MutationReceipt, MutationSnapshot, MutationTargetReceipt},
    },
};
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

const MAX_RECEIPTS: usize = 128;
const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct ReviewCapture {
    id: String,
    session: Option<String>,
    root: PathBuf,
    receipts: Vec<(MutationReceipt, Option<String>)>,
    bytes: usize,
    restored: usize,
    reason: Option<String>,
    sealed: bool,
    rejecting: bool,
    #[cfg(test)]
    post_restore_verify_failure: bool,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct RejectReport {
    pub id: String,
    pub job_id: Option<crate::jobs::JobId>,
    pub restored: usize,
    pub pending: usize,
    pub error: Option<String>,
    pub warnings: Vec<String>,
}

struct RejectGuard(Arc<Mutex<ReviewCapture>>);
impl Drop for RejectGuard {
    fn drop(&mut self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).rejecting = false;
    }
}

/// Reject symbolic links in every component and non-files at the leaf. Only
/// lexical, project-relative paths are accepted; a missing leaf is allowed.
fn checked_path(root: &Path, path: &Path) -> anyhow::Result<PathBuf> {
    let rel = path.strip_prefix(root)?;
    anyhow::ensure!(!rel.as_os_str().is_empty(), "project root is not a file");
    let mut full = root.to_path_buf();
    let components: Vec<_> = rel.components().collect();
    for (i, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            anyhow::bail!("unsupported path component");
        };
        full.push(name);
        match std::fs::symlink_metadata(&full) {
            Ok(meta) => {
                anyhow::ensure!(
                    !meta.file_type().is_symlink(),
                    "symbolic link: {}",
                    full.display()
                );
                anyhow::ensure!(
                    if i + 1 == components.len() {
                        meta.is_file()
                    } else {
                        meta.is_dir()
                    },
                    "file type changed: {}",
                    full.display()
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && i + 1 == components.len() => {}
            Err(e) => return Err(e.into()),
        }
    }
    anyhow::ensure!(root.canonicalize()? == root, "project root changed");
    Ok(full)
}

fn known(snapshot: &MutationSnapshot) -> bool {
    !snapshot.exists || snapshot.content.is_some()
}
fn exact(a: &MutationSnapshot, b: &MutationSnapshot) -> bool {
    a.state_matches(b) && a.content == b.content
}

impl ReviewCapture {
    fn record(&mut self, mut receipt: MutationReceipt, change_id: Option<String>) {
        if self.sealed || self.reason.is_some() {
            return;
        }
        let bytes = receipt
            .before
            .content
            .as_ref()
            .map_or(0, String::len)
            .saturating_add(receipt.after.content.as_ref().map_or(0, String::len))
            .saturating_add(receipt.diff.len());
        if self.receipts.len() >= MAX_RECEIPTS || self.bytes.saturating_add(bytes) > MAX_BYTES {
            self.reason =
                Some("Rollback capture exceeded 128 receipts or 8 MiB; view only.".into());
            self.receipts.clear();
            self.bytes = 0;
            return;
        }
        if !known(&receipt.before) || !known(&receipt.after) {
            self.reason = Some("Unknown file baseline; view only.".into());
            return;
        }
        let path = match checked_path(&self.root, &receipt.path) {
            Ok(path) => path,
            Err(e) => {
                self.reason = Some(format!("Unsafe rollback path: {e}; view only."));
                return;
            }
        };
        if path.to_str().is_none() || path.to_string_lossy().contains(['\n', '\r']) {
            self.reason = Some("Unsupported filename; view only.".into());
            return;
        }
        receipt.path = path;
        if let Some((previous, _)) = self
            .receipts
            .iter()
            .rev()
            .find(|(r, _)| r.path == receipt.path)
            && !exact(&previous.after, &receipt.before)
        {
            self.reason = Some("External changes occurred between agent edits; view only.".into());
        }
        self.bytes += bytes;
        self.receipts.push((receipt, change_id));
    }

    fn aggregate(&self) -> BTreeMap<PathBuf, (MutationSnapshot, MutationSnapshot)> {
        let mut files = BTreeMap::new();
        for (r, _) in &self.receipts {
            files
                .entry(r.path.clone())
                .and_modify(|(_, after)| *after = r.after.clone())
                .or_insert_with(|| (r.before.clone(), r.after.clone()));
        }
        files
    }

    fn payload(&self) -> DiffReviewPayload {
        let mut diff = String::new();
        let mut files = Vec::new();
        for (path, (before, after)) in self.aggregate() {
            if exact(&before, &after) {
                continue;
            }
            let rel = path
                .strip_prefix(&self.root)
                .expect("checked receipt")
                .to_string_lossy()
                .to_string();
            diff.push_str(&format!("diff --git a/{rel} b/{rel}\n"));
            diff.push_str(
                &mutation::mutation_diff_and_stats(
                    before.content_or_empty(),
                    after.content_or_empty(),
                )
                .0,
            );
            files.push(rel);
        }
        DiffReviewPayload {
            diff,
            files,
            review_id: Some(self.id.clone()),
            reject_reason: self
                .reason
                .clone()
                .or_else(|| self.rejecting.then(|| "Rollback is running.".into())),
            evidence: vec![],
            evidence_warnings: vec![],
        }
    }
}

impl FsTools {
    /// Clone-local capture handle: only this job and its descendants record here.
    pub(crate) fn with_review_capture(mut self, job: crate::jobs::JobId) -> Self {
        let capture = Arc::new(Mutex::new(ReviewCapture {
            id: format!("{job}-{}", uuid::Uuid::now_v7()),
            session: self.get_current_session().map(|s| s.meta.id),
            root: mutation::canonicalize_for_scope(&self.config.project_root),
            receipts: vec![],
            bytes: 0,
            restored: 0,
            reason: None,
            sealed: false,
            rejecting: false,
            #[cfg(test)]
            post_restore_verify_failure: false,
        }));
        *self.review_registry.lock().unwrap() = Some(capture.clone());
        self.review_capture = Some(capture);
        self
    }
    pub(crate) fn capture_receipt(&self, receipt: &MutationReceipt, change_id: Option<String>) {
        if let Some(capture) = &self.review_capture {
            capture.lock().unwrap().record(receipt.clone(), change_id);
        }
    }
    pub(crate) fn seal_review(&self) -> Option<DiffReviewPayload> {
        let capture = self.review_capture.as_ref()?;
        let mut capture = capture.lock().unwrap();
        capture.sealed = true;
        let payload = capture.payload();
        (!payload.files.is_empty() || payload.reject_reason.is_some()).then_some(payload)
    }
    pub(crate) fn review_payload(&self, id: &str) -> Option<DiffReviewPayload> {
        let registry = self.review_registry.lock().unwrap();
        let capture = registry.as_ref()?.lock().unwrap();
        (capture.id == id).then(|| capture.payload())
    }
    pub(crate) fn interrupted_review_report(
        &self,
        id: &str,
        job: crate::jobs::JobId,
    ) -> Option<RejectReport> {
        let registry = self.review_registry.lock().unwrap();
        let capture = registry.as_ref()?.lock().unwrap();
        (capture.id == id && !capture.rejecting).then(|| RejectReport {
            id: id.into(), job_id: Some(job), restored: capture.restored, pending: capture.receipts.len(),
            error: Some("Rollback interrupted; restored progress retained and remaining files left applied.".into()), warnings: vec![],
        })
    }
    pub(crate) fn dismiss_review(&self, id: &str) {
        let mut registry = self.review_registry.lock().unwrap();
        if registry
            .as_ref()
            .is_some_and(|c| c.lock().unwrap().id == id)
        {
            *registry = None;
        }
    }
    #[cfg(test)]
    pub(crate) async fn reject_review(
        &self,
        id: &str,
        token: &tokio_util::sync::CancellationToken,
    ) -> RejectReport {
        self.reject_review_in_job(id, token, None).await
    }
    pub(crate) async fn reject_review_in_job(
        &self,
        id: &str,
        token: &tokio_util::sync::CancellationToken,
        owner: Option<(&crate::jobs::JobManager, crate::jobs::JobId)>,
    ) -> RejectReport {
        let mut report = RejectReport {
            id: id.into(),
            job_id: owner.map(|(_, id)| id),
            restored: 0,
            pending: 0,
            error: None,
            warnings: vec![],
        };
        let capture = self.review_registry.lock().unwrap().clone();
        let Some(capture) = capture else {
            report.error = Some("Review expired.".into());
            return report;
        };
        let prepared = (|| -> anyhow::Result<_> {
            let mut c = capture.lock().unwrap();
            report.pending = c.receipts.len();
            anyhow::ensure!(
                c.id == id && c.sealed && !c.rejecting,
                "Stale or busy review."
            );
            anyhow::ensure!(
                c.session == self.get_current_session().map(|s| s.meta.id),
                "Review belongs to another session."
            );
            anyhow::ensure!(
                c.reason.is_none(),
                "{}",
                c.reason.as_deref().unwrap_or("View only.")
            );
            for (path, (_, after)) in c.aggregate() {
                checked_path(&c.root, &path)?;
                anyhow::ensure!(
                    exact(&mutation::read_text_snapshot(&path)?, &after),
                    "Conflict: {} changed after review; nothing restored.",
                    path.display()
                );
            }
            c.rejecting = true;
            Ok((c.root.clone(), c.receipts.clone()))
        })();
        let (root, receipts) = match prepared {
            Ok(p) => p,
            Err(e) => {
                report.error = Some(e.to_string());
                return report;
            }
        };
        let _guard = RejectGuard(capture.clone());
        for (receipt, change_id) in receipts.into_iter().rev() {
            if token.is_cancelled() {
                report.error = Some("Rollback cancelled; remaining changes retained.".into());
                break;
            }
            let mut undo_stack = self.undo_stack.write().await;
            let step = || -> anyhow::Result<_> {
                anyhow::ensure!(
                    capture.lock().unwrap().session
                        == self.get_current_session().map(|s| s.meta.id),
                    "Session changed during rollback."
                );
                checked_path(&root, &receipt.path)?;
                let current = mutation::read_text_snapshot(&receipt.path)?;
                anyhow::ensure!(
                    exact(&current, &receipt.after),
                    "Conflict during rollback: {}",
                    receipt.path.display()
                );
                let (after, verification_error) = if receipt.before.exists {
                    let committed = mutation::commit_text_candidate_blocking(
                        &receipt.path,
                        &current,
                        receipt.before.content.as_deref().expect("known baseline"),
                    );
                    #[cfg(test)]
                    let committed = if committed.is_ok()
                        && capture.lock().unwrap().post_restore_verify_failure
                    {
                        Err(mutation::MutationCommitError::VerifyFailed(
                            "injected post-rename read failure".into(),
                        ))
                    } else {
                        committed
                    };
                    match committed {
                        Ok(after) => (after, None),
                        Err(mutation::MutationCommitError::VerifyFailed(error)) => {
                            // Rename committed the baseline bytes. Preserve progress even
                            // when a subsequent read fails or an external writer wins.
                            (
                                receipt.before.clone(),
                                Some(format!(
                                    "Restoration committed for {} but verification is unconfirmed; remaining targets left unchanged: {error}",
                                    receipt.path.display()
                                )),
                            )
                        }
                        Err(error) => return Err(error.into()),
                    }
                } else {
                    std::fs::remove_file(&receipt.path)?;
                    (MutationSnapshot::missing(), None)
                };
                let undo = mutation::build_receipt(
                    crate::provenance::ChangeKind::Undo,
                    receipt.path.clone(),
                    current,
                    after,
                    MutationTargetReceipt::File,
                );
                // No await between file commit, capture advancement and undo removal.
                {
                    let mut capture = capture.lock().unwrap();
                    capture.receipts.pop();
                    capture.restored += 1;
                }
                undo_stack.remove_review_entry(&receipt, change_id.as_deref());
                Ok((undo, verification_error))
            };
            let restored = match owner {
                Some((jobs, job)) => jobs.synchronous_step(job, step).unwrap_or_else(|| {
                    Err(anyhow::anyhow!(
                        "Rollback cancelled; remaining changes retained."
                    ))
                }),
                None => {
                    let mut step = step;
                    step()
                }
            };
            drop(undo_stack);
            match restored {
                Ok((undo, verification_error)) => {
                    report.restored += 1;
                    let finalized = self
                        .finalize_mutation(
                            undo,
                            FinalizeMutationOptions {
                                record_undo: false,
                                reverts_change_id: change_id,
                                ..Default::default()
                            },
                        )
                        .await;
                    report.warnings.extend(finalized.warnings);
                    if let Some(error) = verification_error {
                        report.error = Some(error);
                        break;
                    }
                }
                Err(e) => {
                    report.error = Some(e.to_string());
                    break;
                }
            }
        }
        let c = capture.lock().unwrap();
        report.pending = c.receipts.len();
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::ChangeKind;
    use tokio_util::sync::CancellationToken;

    fn setup() -> (tempfile::TempDir, FsTools) {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::config::AppConfig {
            project_root: dir.path().canonicalize().unwrap(),
            ..Default::default()
        };
        let fs = FsTools::new(Arc::new(tokio::sync::RwLock::new(None)), Arc::new(config))
            .with_review_capture(crate::jobs::JobId(1));
        (dir, fs)
    }
    async fn change(fs: &FsTools, path: &Path, text: &str) {
        let before = mutation::read_text_snapshot(path).unwrap();
        let after = mutation::commit_text_candidate(path, &before, text)
            .await
            .unwrap();
        fs.finalize_mutation(
            mutation::build_receipt(
                ChangeKind::FileWrite,
                path.to_path_buf(),
                before,
                after,
                MutationTargetReceipt::File,
            ),
            FinalizeMutationOptions {
                record_undo: true,
                ..Default::default()
            },
        )
        .await;
    }
    async fn reject(fs: &FsTools) -> RejectReport {
        let payload = fs.seal_review().unwrap();
        fs.reject_review(
            payload.review_id.as_deref().unwrap(),
            &CancellationToken::new(),
        )
        .await
    }
    fn git(root: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
    #[tokio::test]
    async fn test_reject_preserves_preexisting_untracked_content() {
        let (dir, fs) = setup();
        let path = dir.path().join("user.txt");
        std::fs::write(&path, "user baseline\n").unwrap();
        change(&fs, &path, "agent\n").await;
        let report = reject(&fs).await;
        assert!(report.error.is_none(), "{report:?}");
        assert_eq!(std::fs::read_to_string(path).unwrap(), "user baseline\n");
        assert!(fs.undo_stack.read().await.is_empty());
    }
    #[tokio::test]
    async fn test_reject_preserves_staged_unstaged_and_exact_paths() {
        let (dir, fs) = setup();
        let root = dir.path();
        git(root, &["init", "-q"]);
        std::fs::write(root.join("foo"), "staged\n").unwrap();
        git(root, &["add", "foo"]);
        std::fs::write(root.join("foo"), "user unstaged\n").unwrap();
        std::fs::write(root.join("userfoo"), "unrelated\n").unwrap();
        std::fs::create_dir(root.join("dir")).unwrap();
        std::fs::write(root.join("dir/foo"), "directory baseline\n").unwrap();
        let index = git(root, &["ls-files", "--stage", "-z"]);
        change(&fs, &root.join("foo"), "agent\n").await;
        change(&fs, &root.join("dir/foo"), "agent too\n").await;
        assert!(reject(&fs).await.error.is_none());
        assert_eq!(git(root, &["ls-files", "--stage", "-z"]), index);
        assert_eq!(
            std::fs::read_to_string(root.join("foo")).unwrap(),
            "user unstaged\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("userfoo")).unwrap(),
            "unrelated\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("dir/foo")).unwrap(),
            "directory baseline\n"
        );
    }
    #[tokio::test]
    async fn test_reject_new_file_only_removes_agent_creation() {
        let (dir, fs) = setup();
        let new = dir.path().join("new");
        change(&fs, &new, "new\n").await;
        assert!(reject(&fs).await.error.is_none());
        assert!(!new.exists());
    }
    #[tokio::test]
    async fn test_reject_preflight_conflict_leaves_all_files_untouched() {
        let (dir, fs) = setup();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        change(&fs, &a, "agent a").await;
        change(&fs, &b, "agent b").await;
        std::fs::write(&b, "user later").unwrap();
        let report = reject(&fs).await;
        assert!(report.error.as_deref().unwrap().contains("Conflict"));
        assert_eq!(report.restored, 0);
        assert_eq!(std::fs::read_to_string(a).unwrap(), "agent a");
        assert_eq!(std::fs::read_to_string(b).unwrap(), "user later");
    }
    #[tokio::test]
    async fn test_reject_continuous_edits_and_prior_turn_isolation() {
        let (dir, fs) = setup();
        let path = dir.path().join("a");
        change(&fs, &path, "accepted turn").await;
        let accepted = fs.seal_review().unwrap();
        fs.dismiss_review(accepted.review_id.as_deref().unwrap());
        let next = fs.clone().with_review_capture(crate::jobs::JobId(2));
        change(&next, &path, "edit one").await;
        change(&next, &path, "edit two").await;
        let payload = next.seal_review().unwrap();
        assert!(payload.diff.contains("-accepted turn"));
        assert!(payload.diff.contains("+edit two"));
        assert!(!payload.diff.contains("edit one"));
        let report = next
            .reject_review(
                payload.review_id.as_deref().unwrap(),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(report.restored, 2);
        assert!(report.error.is_none());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "accepted turn");
        assert_eq!(next.undo_stack.read().await.len(), 1);
    }
    #[tokio::test]
    async fn test_reject_external_change_between_edits_is_view_only() {
        let (dir, fs) = setup();
        let path = dir.path().join("a");
        change(&fs, &path, "one").await;
        std::fs::write(&path, "user mixed").unwrap();
        change(&fs, &path, "two").await;
        let payload = fs.seal_review().unwrap();
        assert!(
            payload
                .reject_reason
                .as_deref()
                .unwrap()
                .contains("External")
        );
        let report = fs
            .reject_review(
                payload.review_id.as_deref().unwrap(),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(report.restored, 0);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "two");
    }
    #[tokio::test]
    async fn test_reject_capture_survives_undo_eviction_and_bounds() {
        let (dir, fs) = setup();
        let path = dir.path().join("a");
        for n in 0..25 {
            change(&fs, &path, &format!("edit{n}")).await;
        }
        assert_eq!(fs.undo_stack.read().await.len(), 20);
        let report = reject(&fs).await;
        assert_eq!(report.restored, 25);
        assert!(!path.exists());
        let next = fs.with_review_capture(crate::jobs::JobId(2));
        for n in 0..=MAX_RECEIPTS {
            change(&next, &path, &format!("edit{n}")).await;
        }
        let payload = next.seal_review().unwrap();
        assert!(payload.reject_reason.unwrap().contains("exceeded"));
        let report = next
            .reject_review(
                payload.review_id.as_deref().unwrap(),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(report.restored, 0);
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            format!("edit{MAX_RECEIPTS}")
        );
    }
    #[tokio::test]
    async fn test_reject_stale_session_cancel_and_double_reject() {
        let (dir, fs) = setup();
        let path = dir.path().join("a");
        change(&fs, &path, "agent").await;
        let id = fs.seal_review().unwrap().review_id.unwrap();
        assert!(
            fs.reject_review("wrong", &CancellationToken::new())
                .await
                .error
                .is_some()
        );
        let capture = fs.review_capture.as_ref().unwrap();
        capture.lock().unwrap().session = Some("other".into());
        assert!(
            fs.reject_review(&id, &CancellationToken::new())
                .await
                .error
                .is_some()
        );
        capture.lock().unwrap().session = None;
        let token = CancellationToken::new();
        token.cancel();
        assert_eq!(fs.reject_review(&id, &token).await.restored, 0);
        assert_eq!(
            fs.reject_review(&id, &CancellationToken::new())
                .await
                .restored,
            1
        );
        assert_eq!(
            fs.reject_review(&id, &CancellationToken::new())
                .await
                .restored,
            0
        );
        assert!(!path.exists());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn test_reject_symlink_and_type_conflicts_leave_all_untouched() {
        let (dir, fs) = setup();
        let path = dir.path().join("a");
        let other = dir.path().join("other");
        change(&fs, &path, "agent").await;
        std::fs::write(&other, "other user").unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&other, &path).unwrap();
        assert_eq!(reject(&fs).await.restored, 0);
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "other user");
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(reject(&fs).await.restored, 0);
        assert!(path.is_dir());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn test_reject_partial_io_failure_retry_skips_restored_receipts() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, fs) = setup();
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        let a = locked.join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, "baseline").unwrap();
        change(&fs, &a, "agent a").await;
        change(&fs, &b, "agent b").await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
        let id = fs.seal_review().unwrap().review_id.unwrap();
        let report = fs.reject_review(&id, &CancellationToken::new()).await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(report.restored, 1, "{report:?}");
        assert_eq!(report.pending, 1);
        assert!(report.error.is_some());
        assert!(!b.exists());
        let retry = fs.reject_review(&id, &CancellationToken::new()).await;
        assert_eq!(retry.restored, 1);
        assert!(retry.error.is_none());
        assert_eq!(std::fs::read_to_string(a).unwrap(), "baseline");
    }
    #[tokio::test]
    async fn test_reject_post_rename_verify_failure_preserves_committed_progress() {
        let (dir, fs) = setup();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, "a baseline").unwrap();
        std::fs::write(&b, "b baseline").unwrap();
        change(&fs, &a, "agent a").await;
        change(&fs, &b, "agent b").await;
        let id = fs.seal_review().unwrap().review_id.unwrap();
        fs.review_capture
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .post_restore_verify_failure = true;
        let report = fs.reject_review(&id, &CancellationToken::new()).await;
        assert_eq!(report.restored, 1);
        assert_eq!(report.pending, 1);
        assert!(
            report
                .error
                .as_deref()
                .unwrap()
                .contains("verification is unconfirmed")
        );
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "b baseline");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "agent a");
        fs.review_capture
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .post_restore_verify_failure = false;
        let retry = fs.reject_review(&id, &CancellationToken::new()).await;
        assert_eq!(retry.restored, 1);
        assert_eq!(retry.pending, 0);
        assert!(retry.error.is_none());
    }

    #[tokio::test]
    async fn test_reject_abort_resets_running_flag_without_losing_receipts() {
        let (dir, fs) = setup();
        let path = dir.path().join("a");
        change(&fs, &path, "agent").await;
        let id = fs.seal_review().unwrap().review_id.unwrap();
        let held = fs.undo_stack.write().await;
        let worker = fs.clone();
        let job_id = id.clone();
        let task = tokio::spawn(async move {
            worker
                .reject_review(&job_id, &CancellationToken::new())
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !fs
                .review_capture
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .rejecting
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        drop(held);
        assert!(
            !fs.review_capture
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .rejecting
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "agent");
        assert_eq!(
            fs.reject_review(&id, &CancellationToken::new())
                .await
                .restored,
            1
        );
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn test_capture_is_clone_local_and_byte_bounded() {
        let (dir, fs) = setup();
        let plain = fs.clone();
        let captured = plain.clone().with_review_capture(crate::jobs::JobId(2));
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        change(&plain, &a, "unrelated job").await;
        change(&captured, &b, "tracked").await;
        assert_eq!(captured.seal_review().unwrap().files, ["b"]);
        let bytes = dir.path().join("large");
        change(&captured, &bytes, &"x".repeat(MAX_BYTES)).await;
        // Sealed capture deliberately cannot receive late mutations.
        assert_eq!(
            captured
                .review_capture
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .receipts
                .len(),
            1
        );
        let next = captured.with_review_capture(crate::jobs::JobId(3));
        change(&next, &bytes, &"y".repeat(MAX_BYTES)).await;
        assert!(
            next.seal_review()
                .unwrap()
                .reject_reason
                .unwrap()
                .contains("8 MiB")
        );
    }
}
