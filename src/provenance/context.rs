//! Per-turn provenance attribution (no global mutable state).
//!
//! A `ProvenanceAttribution` travels with `ToolRuntime` and `run_agent_loop`
//! so concurrent foreground/background/sub-agent work can never misattribute
//! a mutation to the wrong user directive. Never store a "current directive"
//! on `FsTools` or in a global.

/// Which user-observed directive the current agent turn belongs to, if any.
///
/// `None` means no observed directive (internal fix, manual `/test`, or a
/// context where no directive was recorded). Requirements must never be
/// created without a directive; mutations and verifications may still carry
/// plan/requirement links when available.
#[derive(Debug, Clone, Default)]
pub struct ProvenanceAttribution {
    pub directive_id: Option<String>,
}

impl ProvenanceAttribution {
    pub fn none() -> Self {
        Self { directive_id: None }
    }

    pub fn with_directive(id: impl Into<String>) -> Self {
        Self {
            directive_id: Some(id.into()),
        }
    }
}
