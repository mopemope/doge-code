//! Version-one durable execution-workspace shape. Freeze these fields/enums;
//! evolve via an explicit provenance wire adapter, never implicit serde migration.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStatus {
    Complete,
    Partial,
    Unavailable,
    LimitExceeded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Regular,
    Missing,
    Symlink,
    Directory,
    Submodule,
    Unsupported,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub kind: FileKind,
    pub exists: Option<bool>,
    pub byte_len: Option<u64>,
    pub content_hash: Option<String>,
    pub executable_bits: Option<u32>,
}
impl FileEntry {
    pub fn known(&self) -> bool {
        self.kind == FileKind::Missing && self.exists == Some(false)
            || self.kind == FileKind::Regular
                && self.exists == Some(true)
                && self.byte_len.is_some()
                && self.executable_bits.is_some()
                && self.content_hash.as_deref().is_some_and(|hash| {
                    hash.strip_prefix("blake3:").is_some_and(|digest| {
                        digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
                    })
                })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Diagnostic {
    GitUnavailable,
    InvalidPath,
    UnsupportedEncoding,
    FileUnavailable,
    ChangedDuringCapture,
    PathLimit,
    FileLimit,
    InputLimit,
    JsonLimit,
    Deadline,
    UnsupportedPlatform,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub started_at: String,
    pub finished_at: String,
    pub scope: String,
    pub status: CaptureStatus,
    pub head_oid: Option<String>,
    pub project_relative_to_git_root: Option<String>,
    pub files: Vec<FileEntry>,
    pub manifest_digest: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
}
impl Snapshot {
    pub fn unavailable(reason: Diagnostic) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            version: 1,
            started_at: now.clone(),
            finished_at: now,
            scope: "project_git_files_and_provenance_references".into(),
            status: CaptureStatus::Unavailable,
            head_oid: None,
            project_relative_to_git_root: None,
            files: vec![],
            manifest_digest: None,
            diagnostics: vec![reason],
        }
    }
    pub fn same_observation(&self, other: &Self) -> bool {
        self.status == other.status
            && self.files == other.files
            && self.diagnostics == other.diagnostics
            && self.head_oid == other.head_oid
            && self.project_relative_to_git_root == other.project_relative_to_git_root
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    NotRecorded,
    StableEndpoints,
    ChangedBetweenEndpoints,
    Indeterminate,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurrentState {
    NotRecorded,
    MatchesStart,
    DiffersFromStart,
    Indeterminate,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Differences {
    pub changed: Vec<String>,
    pub added: Vec<String>,
    pub deleted: Vec<String>,
    pub unknown: Vec<String>,
}
impl Differences {
    pub fn differs(&self) -> bool {
        !self.changed.is_empty() || !self.added.is_empty() || !self.deleted.is_empty()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionWorkspace {
    pub version: u32,
    pub start: Snapshot,
    pub end: Option<Snapshot>,
    pub run_state: RunState,
    pub differences: Differences,
}
#[derive(Debug, Clone, Serialize)]
pub struct CurrentComparison {
    pub state: CurrentState,
    pub differences: Differences,
}
