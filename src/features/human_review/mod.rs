//! Explicit local operator decisions, independent of command outcomes.
//! No LLM tool, identity claim, workspace mutation, or remote approval.
use crate::{features::evidence_report::EvidenceReport, session::SessionStore};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Write, path::Path};

const MAX_RECORDS: usize = 1000;
const MAX_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Accept,
    RequestChanges,
}
impl DecisionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::RequestChanges => "request-changes",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewTarget {
    pub format_version: u32,
    pub token: String,
    pub change_ids: Vec<String>,
    pub verification_ids: Vec<String>,
    pub workspace_manifest: String,
    pub evidence_incomplete: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRecord {
    pub schema_version: u32,
    pub id: String,
    pub session_id: String,
    pub timestamp: String,
    pub kind: DecisionKind,
    pub target: ReviewTarget,
    pub supersedes: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    Current,
    Stale,
    Superseded,
    Unknown,
}
#[derive(Debug, Serialize)]
pub struct DecisionView {
    pub recorded: DecisionRecord,
    pub status: DecisionStatus,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryState {
    NotRecorded,
    Recorded,
    Unknown,
}
#[derive(Debug, thiserror::Error)]
pub enum ReviewError {
    #[error("review storage failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("review serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("review evidence failed: {0}")]
    Evidence(#[from] crate::features::evidence_report::ReportError),
    #[error("review session failed: {0}")]
    Session(#[from] crate::session::error::SessionError),
    #[error(
        "review target is unavailable, changed, or belongs to another session; inspect fresh session evidence"
    )]
    TargetChanged,
    #[error("review history is incomplete or unsafe; no decision recorded")]
    InvalidHistory,
    #[error(
        "review decision {0} was published but durability or destination identity is unconfirmed; inspect evidence before retrying"
    )]
    PublishedUnconfirmed(String),
    #[error("review storage limit exceeded")]
    Limit,
}
type Result<T> = std::result::Result<T, ReviewError>;

/// Hash source bytes and the canonical comparison, never timestamps or decisions.
pub(crate) fn attach(
    report: &mut EvidenceReport,
    identity: &BTreeMap<String, String>,
    dir: &Path,
    execution: Option<&crate::features::verification_snapshot::Snapshot>,
) -> Result<()> {
    let source: BTreeMap<_, _> = identity
        .iter()
        .filter(|(p, _)| !p.contains("/human-review/"))
        .collect();
    if report.snapshot.complete
        && report.repository.comparison_available
        && report.repository.requested_base.is_none()
        && !report.changes.is_empty()
        && valid_ids(
            &report
                .changes
                .iter()
                .map(|c| c.id.clone())
                .collect::<Vec<_>>(),
            false,
        )
        && valid_ids(
            &report
                .verifications
                .iter()
                .map(|v| v.id.clone())
                .collect::<Vec<_>>(),
            true,
        )
        && execution.is_none_or(|s| {
            s.status == crate::features::verification_snapshot::CaptureStatus::Complete
        })
    {
        let projection = serde_json::json!({"format_version":1,"session":report.session.id,"source":source,
            "execution":execution.map(|s|serde_json::json!({"status":s.status,"head":s.head_oid,"files":s.files,"manifest":s.manifest_digest})),
            "repository":report.repository,"snapshot":report.snapshot,"changes":report.changes,
            "verifications":report.verifications,"requirements":report.requirements,"plan":report.plan,
            "plan_available":report.plan_available,"obligations":report.obligations,"workspace_comparison":report.workspace_comparison,"warnings":report.warnings});
        // Content opt-in must never alter target identity.
        let mut projection = projection;
        if let Some(changes) = projection["changes"].as_array_mut() {
            for c in changes {
                c["recorded"].as_object_mut().map(|o| o.remove("diff"));
            }
        }
        if let Some(verifications) = projection["verifications"].as_array_mut() {
            for v in verifications {
                if let Some(o) = v.as_object_mut() {
                    o.remove("stdout_excerpt");
                    o.remove("stderr_excerpt");
                }
            }
        }
        let token = format!(
            "review-v1:{}",
            blake3::hash(&serde_json::to_vec(&projection)?).to_hex()
        );
        report.review_target = Some(ReviewTarget {
            format_version: 1,
            token,
            change_ids: report.changes.iter().map(|c| c.id.clone()).collect(),
            verification_ids: report.verifications.iter().map(|v| v.id.clone()).collect(),
            workspace_manifest: report.snapshot.manifest_digest.clone(),
            evidence_incomplete: !report.summary.record_collection_complete,
        });
    }
    let (records, unknown) = load(dir, &report.session.id)?;
    report.review_decision_state = if unknown {
        HistoryState::Unknown
    } else if records.is_empty() {
        HistoryState::NotRecorded
    } else {
        HistoryState::Recorded
    };
    let superseded: std::collections::BTreeSet<_> = records
        .iter()
        .filter_map(|r| r.supersedes.as_deref())
        .collect();
    report.review_decisions = records
        .iter()
        .map(|r| DecisionView {
            recorded: r.clone(),
            status: if unknown {
                DecisionStatus::Unknown
            } else if superseded.contains(r.id.as_str()) {
                DecisionStatus::Superseded
            } else {
                match &report.review_target {
                    Some(t) if t == &r.target => DecisionStatus::Current,
                    Some(_) => DecisionStatus::Stale,
                    None => DecisionStatus::Unknown,
                }
            },
        })
        .collect();
    Ok(())
}

fn load(session_dir: &Path, session_id: &str) -> Result<(Vec<DecisionRecord>, bool)> {
    let parent = session_dir.join("human-review");
    let mut unknown = false;
    if parent.exists() {
        for entry in std::fs::read_dir(&parent)? {
            if entry?.file_name() != "v1" {
                unknown = true;
            }
        }
    }
    if parent.join("v1").is_dir() {
        for entry in std::fs::read_dir(parent.join("v1"))? {
            if entry?.file_name() != "decisions" {
                unknown = true;
            }
        }
    }
    let dir = parent.join("v1/decisions");
    if !dir.exists() {
        return Ok((vec![], unknown));
    }
    let mut records = Vec::new();
    let mut count = 0;
    // Frozen report inputs already reject aliases; writer calls also validate paths.
    for entry in std::fs::read_dir(&dir)? {
        count += 1;
        if count > MAX_RECORDS {
            return Err(ReviewError::Limit);
        }
        let entry = entry?;
        let metadata = entry.metadata()?;
        if !entry.file_type()?.is_file() || metadata.len() > MAX_BYTES {
            unknown = true;
            continue;
        }
        if entry.file_name().to_string_lossy().starts_with(".tmp_") {
            unknown = true;
            continue;
        }
        let bytes = std::fs::read(entry.path())?;
        match serde_json::from_slice::<DecisionRecord>(&bytes) {
            Ok(r)
                if r.schema_version == 1
                    && r.target.format_version == 1
                    && r.session_id == session_id
                    && canonical_id(&r.id)
                    && chrono::DateTime::parse_from_rfc3339(&r.timestamp).is_ok()
                    && hash(&r.target.workspace_manifest, "blake3:")
                    && valid_ids(&r.target.change_ids, false)
                    && valid_ids(&r.target.verification_ids, true)
                    && r.supersedes.as_deref().is_none_or(canonical_id)
                    && entry.file_name().to_string_lossy() == format!("{}.json", r.id)
                    && valid_token(&r.target.token)
                    && r.target.change_ids.len() <= 10000
                    && r.target.verification_ids.len() <= 10000 =>
            {
                records.push(r)
            }
            _ => unknown = true,
        }
        if records.len() > MAX_RECORDS {
            return Err(ReviewError::Limit);
        }
    }
    let mut by_id: BTreeMap<String, DecisionRecord> =
        records.into_iter().map(|r| (r.id.clone(), r)).collect();
    let roots: Vec<_> = by_id
        .values()
        .filter(|r| r.supersedes.is_none())
        .map(|r| r.id.clone())
        .collect();
    let mut ordered = Vec::new();
    if !by_id.is_empty() {
        if roots.len() != 1 {
            unknown = true;
        } else {
            let mut next = Some(roots[0].clone());
            while let Some(id) = next {
                let Some(record) = by_id.remove(&id) else {
                    unknown = true;
                    break;
                };
                let successors: Vec<_> = by_id
                    .values()
                    .filter(|r| r.supersedes.as_deref() == Some(&id))
                    .map(|r| r.id.clone())
                    .collect();
                ordered.push(record);
                if successors.len() > 1 {
                    unknown = true;
                    break;
                }
                next = successors.first().cloned();
            }
            if !by_id.is_empty() {
                unknown = true;
            }
        }
    }
    // Valid but disconnected records remain historical with Unknown status.
    ordered.extend(by_id.into_values());
    Ok((ordered, unknown))
}
fn canonical_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|v| v.to_string() == id)
}
fn valid_ids(ids: &[String], empty: bool) -> bool {
    (empty || !ids.is_empty())
        && ids.len() <= 10000
        && ids.iter().all(|s| canonical_id(s))
        && ids.iter().collect::<std::collections::BTreeSet<_>>().len() == ids.len()
}
fn hash(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

pub fn valid_token(token: &str) -> bool {
    hash(token, "review-v1:")
}

/// Called only after an explicit operator confirmation. Re-read under ownership.
pub async fn record(
    root: &Path,
    id: &str,
    kind: DecisionKind,
    token: &str,
) -> Result<(DecisionRecord, bool)> {
    if uuid::Uuid::parse_str(id).is_err() || !valid_token(token) {
        return Err(ReviewError::TargetChanged);
    }
    let root = root.canonicalize()?;
    // Read-only collector establishes bounded, safe session storage before any write.
    let initial = crate::features::evidence_report::inspect(&root, id).await?;
    if initial
        .review_target
        .as_ref()
        .is_none_or(|t| t.token != token)
    {
        return Err(ReviewError::TargetChanged);
    }
    let store = SessionStore::new(root.join(".doge/sessions"))?;
    let lease = store.try_lease(id)?;
    let report = crate::features::evidence_report::inspect(&root, id).await?;
    let target = report
        .review_target
        .filter(|t| t.token == token)
        .ok_or(ReviewError::TargetChanged)?;
    if report.review_decision_state == HistoryState::Unknown {
        return Err(ReviewError::InvalidHistory);
    }
    let previous = report.review_decisions.last().map(|v| &v.recorded);
    if let Some(r) = previous
        && r.kind == kind
        && r.target == target
    {
        crate::features::verification_snapshot::open_relative(
            &root,
            &format!(".doge/sessions/{id}/human-review/v1/decisions"),
        )?
        .sync_all()
        .map_err(|_| ReviewError::PublishedUnconfirmed(r.id.clone()))?;
        return Ok((r.clone(), false));
    }
    if report.review_decisions.len() >= MAX_RECORDS {
        return Err(ReviewError::Limit);
    }
    let record = DecisionRecord {
        schema_version: 1,
        id: uuid::Uuid::now_v7().to_string(),
        session_id: id.into(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        kind,
        target,
        supersedes: previous.map(|r| r.id.clone()),
    };
    let bytes = serde_json::to_vec_pretty(&record)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(ReviewError::Limit);
    }
    lease.validate(&root.join(".doge/sessions"), id)?;
    publish(
        &root,
        id,
        &record.id,
        &bytes,
        #[cfg(test)]
        None,
    )?;
    Ok((record, true))
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    BeforePublish,
    DirectorySync,
    ReplaceDirectory,
}

#[cfg(unix)]
fn publish(
    root: &Path,
    session: &str,
    id: &str,
    bytes: &[u8],
    #[cfg(test)] fault: Option<Fault>,
) -> Result<()> {
    use std::{
        ffi::CString,
        fs::File,
        os::fd::{AsRawFd, FromRawFd},
        os::unix::fs::MetadataExt,
    };
    let relative = format!(".doge/sessions/{session}");
    let mut parent = crate::features::verification_snapshot::open_relative(root, &relative)?;
    if !parent.metadata()?.is_dir() {
        return Err(ReviewError::InvalidHistory);
    }
    for name in ["human-review", "v1", "decisions"] {
        let name = CString::new(name).map_err(|_| ReviewError::InvalidHistory)?;
        // SAFETY: stable opened parent directory; NUL-terminated static component.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(e.into());
            }
        }
        parent.sync_all()?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        parent = unsafe { File::from_raw_fd(fd) };
    }
    let relative = format!("{relative}/human-review/v1/decisions");
    let same_directory = || -> Result<bool> {
        let current = crate::features::verification_snapshot::open_relative(root, &relative)?;
        let a = current.metadata()?;
        let b = parent.metadata()?;
        Ok((a.dev(), a.ino()) == (b.dev(), b.ino()))
    };
    #[cfg(test)]
    if fault == Some(Fault::ReplaceDirectory) {
        let path = root.join(&relative);
        std::fs::rename(&path, path.with_extension("old"))?;
        std::os::unix::fs::symlink(root.join("outside"), &path)?;
    }
    if !same_directory()? {
        return Err(ReviewError::InvalidHistory);
    }
    let temp = CString::new(format!(".tmp_review_{}", uuid::Uuid::now_v7()))
        .map_err(|_| ReviewError::InvalidHistory)?;
    let dest = CString::new(format!("{id}.json")).map_err(|_| ReviewError::InvalidHistory)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            temp.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        #[cfg(test)]
        if fault == Some(Fault::BeforePublish) {
            return Err(std::io::Error::other("injected publication failure").into());
        }
        if !same_directory()? {
            return Err(ReviewError::InvalidHistory);
        }
        // linkat publishes atomically and never replaces an existing destination.
        if unsafe {
            libc::linkat(
                parent.as_raw_fd(),
                temp.as_ptr(),
                parent.as_raw_fd(),
                dest.as_ptr(),
                0,
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        if unsafe { libc::unlinkat(parent.as_raw_fd(), temp.as_ptr(), 0) } < 0 {
            return Err(ReviewError::PublishedUnconfirmed(id.into()));
        }
        #[cfg(test)]
        if fault == Some(Fault::DirectorySync) {
            return Err(ReviewError::PublishedUnconfirmed(id.into()));
        }
        if parent.sync_all().is_err() || !same_directory().unwrap_or(false) {
            return Err(ReviewError::PublishedUnconfirmed(id.into()));
        }
        Ok(())
    })();
    // SAFETY: remove only this temporary entry from the held directory.
    unsafe { libc::unlinkat(parent.as_raw_fd(), temp.as_ptr(), 0) };
    result
}
#[cfg(not(unix))]
fn publish(
    _root: &Path,
    _session: &str,
    _id: &str,
    _bytes: &[u8],
    #[cfg(test)] _fault: Option<Fault>,
) -> Result<()> {
    Err(ReviewError::InvalidHistory)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn publication_failure_durability_and_directory_replacement_are_distinct() {
        let root = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::now_v7().to_string();
        let session = root.path().join(".doge/sessions").join(&id);
        std::fs::create_dir_all(&session).unwrap();
        let record = uuid::Uuid::now_v7().to_string();
        let path = session.join(format!("human-review/v1/decisions/{record}.json"));
        assert!(
            publish(
                root.path(),
                &id,
                &record,
                b"fixture",
                Some(Fault::BeforePublish)
            )
            .is_err()
        );
        assert!(!path.exists());
        assert!(matches!(
            publish(
                root.path(),
                &id,
                &record,
                b"fixture",
                Some(Fault::DirectorySync)
            ),
            Err(ReviewError::PublishedUnconfirmed(_))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"fixture");
        assert!(publish(root.path(), &id, &record, b"overwrite", None).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"fixture");
        std::fs::create_dir(root.path().join("outside")).unwrap();
        assert!(
            publish(
                root.path(),
                &id,
                "other",
                b"external",
                Some(Fault::ReplaceDirectory)
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read_dir(root.path().join("outside"))
                .unwrap()
                .count(),
            0
        );
    }
}
