use serde::{Deserialize, Serialize};

/// Category of a changed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Unknown,
}

impl ChangeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Unknown => "unknown",
        }
    }
}

/// One project-relative changed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    /// Project-relative `/`-separated path. Never absolute.
    pub path: String,
    pub change_kind: ChangeKind,
}

/// How a changed symbol was identified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentificationConfidence {
    /// Narrowed via a trustworthy changed range (reserved for future use).
    Precise,
    /// All symbols in the file are candidates (current default).
    FileLevel,
    /// No symbol could be resolved (deleted/missing/unsupported).
    Unresolved,
}

impl IdentificationConfidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Precise => "precise",
            Self::FileLevel => "file_level",
            Self::Unresolved => "unresolved",
        }
    }
}

/// A symbol that may have changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedSymbol {
    pub symbol_id: Option<String>,
    pub file: String,
    pub name: String,
    pub identification_confidence: IdentificationConfidence,
}

/// Confidence of a dependency edge.
///
/// No numeric scores: without empirical calibration a number would imply a
/// precision we do not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeConfidence {
    Exact,
    Inferred,
    Ambiguous,
}

impl EdgeConfidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Inferred => "inferred",
            Self::Ambiguous => "ambiguous",
        }
    }
}

/// One reachable symbol in the impact graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImpactNode {
    pub symbol_id: Option<String>,
    pub file: String,
    pub name: String,
    pub depth: usize,
}

/// One dependency edge (caller -> callee direction is `source -> target`
/// where `source` depends on `target`; traversal follows incoming edges).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImpactEdge {
    pub source: String,
    pub target: String,
    pub relation_kind: String,
    pub confidence: EdgeConfidence,
}

/// Bounded reverse-dependency traversal result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImpactGraph {
    #[serde(default)]
    pub nodes: Vec<ImpactNode>,
    #[serde(default)]
    pub edges: Vec<ImpactEdge>,
    #[serde(default)]
    pub truncated: bool,
}

/// Confidence of a test candidate.
///
/// `Exact` requires explicit evidence (test definition in a changed file,
/// verified relation, reliable package association). Filename similarity
/// alone never yields `Exact`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestConfidence {
    Exact,
    Likely,
    Unknown,
}

impl TestConfidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Likely => "likely",
            Self::Unknown => "unknown",
        }
    }
}

/// One candidate test file. Static relationships are not runtime coverage
/// evidence; a candidate is not proof of coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateTest {
    /// Project-relative path.
    pub path: String,
    pub confidence: TestConfidence,
    pub language: String,
}

/// One recommended verification command.
///
/// Structured `program` + `args`: never an opaque shell string and never
/// built by interpolating source-controlled text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationRecommendation {
    /// Verification kind (e.g. `test`, `build`).
    pub kind: String,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub reason: String,
    /// Recommendation confidence: `high` / `medium` / `low`.
    pub confidence: String,
    /// `file` / `package` / `project`.
    pub coverage_scope: String,
}

/// Overall analysis completeness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisStatus {
    Complete,
    Partial,
    NoChanges,
}

impl AnalysisStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::NoChanges => "no_changes",
        }
    }
}

/// Full `impact_analyze` response body (the `value` of the tool output).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactAnalysisResponse {
    pub ok: bool,
    pub analysis_status: AnalysisStatus,
    #[serde(default)]
    pub changed_files: Vec<String>,
    #[serde(default)]
    pub impacted_files: Vec<String>,
    pub graph: ImpactGraph,
    #[serde(default)]
    pub candidate_tests: Vec<CandidateTest>,
    #[serde(default)]
    pub verification: Vec<VerificationRecommendation>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}
