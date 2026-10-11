//! Numeric, process-local projection of an existing main-agent preflight.
//! Viewing it never measures new input, requests a model, or mutates history.
use crate::llm::context_budget::{RequestFootprint, TokenEstimate};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub captured_at: String,
    pub model: String,
    pub footprint: RequestFootprint,
    pub estimate: TokenEstimate,
    pub compaction_limit: u64,
    pub context_window: Option<u32>,
}
impl Snapshot {
    pub fn capture(
        model: &str,
        footprint: RequestFootprint,
        estimate: TokenEstimate,
        compaction_limit: u64,
        context_window: Option<u32>,
    ) -> Self {
        Self {
            captured_at: chrono::Utc::now().to_rfc3339(),
            model: model
                .chars()
                .filter(|c| !c.is_control())
                .take(128)
                .collect(),
            footprint,
            estimate,
            compaction_limit,
            context_window,
        }
    }
    pub fn lines(&self) -> Vec<String> {
        let f = self.footprint;
        vec![
            "Last main-agent preflight (process-local; may span sessions).".into(),
            format!("Captured at: {}; model: {}", self.captured_at, self.model),
            "Historical estimate, not the current draft, a provider token count or remaining capacity.".into(),
            "Reopening reads the latest captured preflight. Edits alone do not remeasure it.".into(),
            format!("Messages: {} projected JSON bytes",f.message_json_bytes),
            format!("Active tool schemas: {} projected JSON bytes",f.tool_schema_json_bytes),
            format!("Fixed request overhead: {} estimated bytes",f.request_overhead_bytes),
            format!("Total footprint: {} bytes (messages + schemas + fixed overhead)",f.total_json_bytes),
            format!("Runtime overlay: {} raw text bytes (message telemetry; not added to total again)",f.runtime_overlay_bytes),
            format!("Prompt estimate: ~{} tokens; source={:?}",self.estimate.prompt_tokens,self.estimate.source),
            format!("Compaction threshold at capture: {} tokens (configuration, not model capacity)",self.compaction_limit),
            match self.context_window { Some(n)=>format!("Known context window at capture: {n} tokens"),None=>"Context window at capture: unknown".into() },
            "Cached tokens still consume context. Cache/usage metrics remain separate under /tokens.".into(),
            "Provider wire projections and fixed overhead are estimates; opaque Responses state is excluded by the existing estimator.".into(),
        ]
    }
}
#[derive(Debug, Clone)]
pub struct View {
    pub lines: Vec<String>,
    pub scroll: u16,
}
impl View {
    pub fn new(snapshot: Option<Snapshot>) -> Self {
        Self { scroll:0, lines:snapshot.map_or_else(||vec!["Context footprint unavailable: no measured main-agent preflight in this process/client.".into(),"Off mode, a failed measurement or a fresh/resumed client may have no capture. Unknown is not zero.".into(),"Viewing makes no model request and performs no compaction.".into()], |s|s.lines()) }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::context_budget::TokenEstimateSource;
    #[test]
    fn footprint_components_overlay_and_unknown_capacity_are_distinct() {
        let f = RequestFootprint::new(1000, 500, 200);
        let s = Snapshot::capture(
            "model\x1b",
            f,
            TokenEstimate {
                prompt_tokens: 800,
                source: TokenEstimateSource::Calibrated,
            },
            2000,
            None,
        );
        let text = s.lines().join("\n");
        assert!(text.contains(&format!("Total footprint: {} bytes", f.total_json_bytes)));
        assert!(text.contains("Runtime overlay: 200 raw text bytes"));
        assert!(text.contains("not added to total again"));
        assert!(text.contains("source=Calibrated"));
        assert!(text.contains("window at capture: unknown"));
        assert!(!text.contains('\x1b'));
        assert!(text.contains("not the current draft"));
        assert!(text.contains("Cached tokens still consume context"));
    }
    #[test]
    fn absent_capture_never_invents_zero_or_success() {
        let text = View::new(None).lines.join("\n");
        assert!(text.contains("unavailable"));
        assert!(text.contains("Unknown is not zero"));
        assert!(!text.contains("0 tokens"));
    }
}
