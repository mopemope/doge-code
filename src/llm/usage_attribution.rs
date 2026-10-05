//! Manual foreground LLM usage attribution.
//!
//! Provider usage and local budget estimates stay separate: this helper only
//! moves provider-reported [`UsageLedger`] deltas observed on the shared
//! client into the persisted session. Local estimates never enter the session.
//!
//! Foreground LLM jobs are serialized by `JobManager`; concurrent background
//! attribution is out of scope for v1 (see contracts).

use crate::llm::usage_ledger::UsageLedger;
use crate::session::SessionManager;
use anyhow::Result;

/// Captured before a manual LLM request: the session active at start plus the
/// shared-client ledger snapshot. Consumed exactly once by [`Self::finish`].
#[derive(Debug, Clone)]
pub struct SessionUsageCheckpoint {
    expected_session_id: String,
    before: UsageLedger,
}

/// Outcome of attributing one manual LLM operation.
#[derive(Debug, Clone)]
pub struct UsageAttributionResult {
    /// Provider delta observed on the shared client (`after - before`).
    pub delta: UsageLedger,
    /// True when the delta carried activity and was applied+persisted.
    pub persisted: bool,
    /// Save outcome when persisted; `None` for empty deltas.
    #[allow(dead_code)]
    pub(crate) outcome: Option<crate::session::store::SessionSaveOutcome>,
}

impl SessionUsageCheckpoint {
    pub fn new(expected_session_id: String, before: UsageLedger) -> Self {
        Self {
            expected_session_id,
            before,
        }
    }

    /// Capture the current session id plus the shared-client snapshot.
    /// Errors when no session is active; never falls back to another session.
    pub fn capture(
        client: &crate::llm::client_core::OpenAIClient,
        session_manager: &std::sync::Mutex<SessionManager>,
    ) -> Result<Self> {
        let manager = crate::utils::safe_std_lock(session_manager, "session_manager")?;
        let id = manager
            .current_session_id()
            .ok_or_else(|| anyhow::anyhow!("no current session for usage attribution"))?;
        let before = client.usage_snapshot();
        Ok(Self::new(id, before))
    }

    /// Attribute the provider delta to the expected session exactly once.
    ///
    /// Validates that the current session id still matches the checkpoint;
    /// on mismatch no session is mutated. Empty deltas skip the save so no
    /// timestamp or write occurs. Failed saves leave the complete payload in
    /// memory as Unsaved; retry via `flush_current_session` never reapplies.
    pub fn finish(
        self,
        after: &UsageLedger,
        session_manager: &mut SessionManager,
    ) -> Result<UsageAttributionResult> {
        let delta = after.difference(&self.before);
        if !delta.has_activity() {
            return Ok(UsageAttributionResult {
                delta,
                persisted: false,
                outcome: None,
            });
        }
        let outcome = session_manager.apply_usage_delta(&self.expected_session_id, &delta)?;
        Ok(UsageAttributionResult {
            delta,
            persisted: true,
            outcome: Some(outcome),
        })
    }

    pub fn expected_session_id(&self) -> &str {
        &self.expected_session_id
    }

    pub fn before(&self) -> &UsageLedger {
        &self.before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_delta_does_not_require_save() {
        let before = UsageLedger::default();
        let checkpoint = SessionUsageCheckpoint::new("sess".into(), before);
        // No manager mutation needed for empty deltas; use a dummy manager
        // path via difference only. Finish requires a manager, so test the
        // predicate directly here.
        let delta = before.difference(&before);
        assert!(!delta.has_activity());
        let _ = checkpoint;
    }
}
