//! Adaptive reasoning policy (v1).
//!
//! Decides how much provider-native reasoning budget (`reasoning_effort`)
//! each agent-loop iteration should request, based on what the agent just
//! did. Transport-agnostic: the same controller can be reused by a future
//! Responses-API backend.
//!
//! Responsibilities:
//! - classify tool outcomes into [`ReasoningPhase`]
//! - aggregate a batch of tool calls deterministically (max severity wins)
//! - escalate to `Recovery` on failure / loop / stall / JSON recovery and
//!   decay back to normal once progress resumes
//! - detect provider/model capability for `reasoning_effort` (pure helpers)
//!
//! This module never depends on `FsTools`, `ToolOutput`, TUI, or history.

use crate::config::{ReasoningConfig, ReasoningEffort, ReasoningMode};

/// Per-iteration reasoning phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningPhase {
    #[default]
    Initial,
    Routine,
    Deliberative,
    Recovery,
}

impl ReasoningPhase {
    /// Severity ordinal for deterministic batch aggregation.
    /// Higher wins: Recovery > Deliberative > Initial > Routine.
    fn severity(self) -> u8 {
        match self {
            Self::Routine => 0,
            Self::Initial => 1,
            Self::Deliberative => 2,
            Self::Recovery => 3,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Routine => "routine",
            Self::Deliberative => "deliberative",
            Self::Recovery => "recovery",
        }
    }

    fn max(self, other: Self) -> Self {
        if other.severity() > self.severity() {
            other
        } else {
            self
        }
    }
}

/// Single tool outcome observed by the agent loop.
///
/// Only the tool name and success flag cross the boundary; raw `ToolOutput`
/// JSON never enters the reasoning module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolObservation {
    pub name: String,
    pub success: bool,
}

impl ToolObservation {
    pub fn new(name: impl Into<String>, success: bool) -> Self {
        Self {
            name: name.into(),
            success,
        }
    }
}

/// Aggregate observation for one tool batch (one LLM response).
#[derive(Debug, Clone, Default)]
pub struct ToolBatchObservation {
    pub tools: Vec<ToolObservation>,
    pub loop_detected: bool,
    pub stall_detected: bool,
}

impl ToolBatchObservation {
    pub fn new(tools: Vec<ToolObservation>, loop_detected: bool, stall_detected: bool) -> Self {
        Self {
            tools,
            loop_detected,
            stall_detected,
        }
    }
}

/// Small state machine used by the agent loop.
#[derive(Debug, Clone)]
pub struct ReasoningController {
    config: ReasoningConfig,
    phase: ReasoningPhase,
}

impl ReasoningController {
    pub fn new(config: ReasoningConfig) -> Self {
        Self {
            config,
            phase: ReasoningPhase::Initial,
        }
    }

    pub fn current_phase(&self) -> ReasoningPhase {
        self.phase
    }

    /// Policy effort for the next LLM request.
    /// `None` only in `Off` mode (send no reasoning hint).
    pub fn current_effort(&self) -> Option<ReasoningEffort> {
        match self.config.mode {
            ReasoningMode::Off => None,
            ReasoningMode::Fixed => Some(self.config.fixed_effort),
            ReasoningMode::Auto => Some(match self.phase {
                ReasoningPhase::Initial => self.config.initial_effort,
                ReasoningPhase::Routine => self.config.routine_effort,
                ReasoningPhase::Deliberative => self.config.deliberative_effort,
                ReasoningPhase::Recovery => self.config.recovery_effort,
            }),
        }
    }

    /// Observe one finished tool batch and advance the phase.
    /// Deterministic: the heaviest phase in the batch wins, independent of
    /// tool call order.
    pub fn observe_tool_batch(&mut self, observation: ToolBatchObservation) {
        if self.config.mode == ReasoningMode::Fixed || self.config.mode == ReasoningMode::Off {
            return;
        }
        if observation.loop_detected || observation.stall_detected {
            self.phase = ReasoningPhase::Recovery;
            return;
        }
        let mut phase = ReasoningPhase::Routine;
        let mut has_tool = false;
        for tool in &observation.tools {
            has_tool = true;
            phase = phase.max(classify_tool(&tool.name, tool.success));
        }
        if !has_tool {
            // Empty batch with no loop/stall signal: keep current phase
            // unless it was Recovery (decay after a clean round-trip).
            // An empty batch means no failure, so decay Recovery.
            if self.phase == ReasoningPhase::Recovery {
                self.phase = ReasoningPhase::Routine;
            }
            return;
        }
        self.phase = phase;
    }

    /// A response failed JSON deserialization and will be retried.
    pub fn observe_json_recovery(&mut self) {
        if self.config.mode == ReasoningMode::Auto {
            self.phase = ReasoningPhase::Recovery;
        }
    }

    /// Compaction bookkeeping hook (v1: no phase change).
    /// Present so callers have a single place to notify the controller.
    pub fn observe_compaction(&mut self) {}
}

/// Classify a single tool outcome.
///
/// Failure always escalates to `Recovery`. On success, routine tools keep
/// the next request cheap while deliberative tools (and unknown / remote
/// MCP tools) keep medium reasoning for interpretation.
pub fn classify_tool(name: &str, success: bool) -> ReasoningPhase {
    if !success {
        return ReasoningPhase::Recovery;
    }
    if is_routine_tool(name) {
        ReasoningPhase::Routine
    } else {
        ReasoningPhase::Deliberative
    }
}

fn is_routine_tool(name: &str) -> bool {
    matches!(
        name,
        "tool_search"
            | "fs_list"
            | "find_file"
            | "search_text"
            | "search_repomap"
            | "plan_read"
            | "plan_write"
            | "requirements_read"
            | "requirements_write"
            | "provenance_read"
            | "list_memories"
            | "search_memory"
            | "execute_process"
            | "execute_bash"
            | "execute_shell"
    )
}

pub use crate::llm::capabilities::ReasoningSupport as ReasoningProviderSupport;

/// Compatibility helper for the Chat Completions route.
pub fn provider_support(base_url: &str, model: &str) -> ReasoningProviderSupport {
    crate::llm::capabilities::resolve(
        crate::features::openai_subscription::ProviderKind::OpenaiCompatible,
        base_url,
        crate::llm::capabilities::ApiKind::ChatCompletions,
        model,
        false,
    )
    .reasoning
}

/// Resolve the final hint to serialize.
///
/// - `Off`: never send.
/// - `Fixed`: always send `fixed` effort (escape hatch for custom providers),
///   bypassing capability detection.
/// - `Auto`: send only when the provider is known to support it.
pub fn resolve_reasoning_hint(
    base_url: &str,
    model: &str,
    mode: &ReasoningMode,
    effort: Option<ReasoningEffort>,
) -> Option<ReasoningEffort> {
    resolve_hint_for_support(provider_support(base_url, model), mode, effort)
}

pub(crate) fn resolve_hint_for_support(
    support: ReasoningProviderSupport,
    mode: &ReasoningMode,
    effort: Option<ReasoningEffort>,
) -> Option<ReasoningEffort> {
    match mode {
        ReasoningMode::Off => None,
        ReasoningMode::Fixed => effort,
        ReasoningMode::Auto => {
            let effort = effort?;
            match support {
                ReasoningProviderSupport::Supported => Some(effort),
                ReasoningProviderSupport::Unsupported | ReasoningProviderSupport::Unknown => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto() -> ReasoningConfig {
        ReasoningConfig::default()
    }

    fn batch(tools: &[(&str, bool)], loop_detected: bool, stall: bool) -> ToolBatchObservation {
        ToolBatchObservation {
            tools: tools
                .iter()
                .map(|(n, s)| ToolObservation::new(*n, *s))
                .collect(),
            loop_detected,
            stall_detected: stall,
        }
    }

    #[test]
    fn test_initial_is_medium() {
        let c = ReasoningController::new(auto());
        assert_eq!(c.current_phase(), ReasoningPhase::Initial);
        assert_eq!(c.current_effort(), Some(ReasoningEffort::Medium));
    }

    #[test]
    fn test_tool_search_success_goes_low() {
        let mut c = ReasoningController::new(auto());
        c.observe_tool_batch(batch(&[("tool_search", true)], false, false));
        assert_eq!(c.current_phase(), ReasoningPhase::Routine);
        assert_eq!(c.current_effort(), Some(ReasoningEffort::Low));
    }

    #[test]
    fn test_search_repomap_success_goes_low() {
        let mut c = ReasoningController::new(auto());
        c.observe_tool_batch(batch(&[("search_repomap", true)], false, false));
        assert_eq!(c.current_effort(), Some(ReasoningEffort::Low));
    }

    #[test]
    fn test_fs_read_success_goes_medium() {
        let mut c = ReasoningController::new(auto());
        c.observe_tool_batch(batch(&[("fs_read", true)], false, false));
        assert_eq!(c.current_phase(), ReasoningPhase::Deliberative);
        assert_eq!(c.current_effort(), Some(ReasoningEffort::Medium));
    }

    #[test]
    fn test_task_success_goes_medium() {
        let mut c = ReasoningController::new(auto());
        c.observe_tool_batch(batch(&[("task", true)], false, false));
        assert_eq!(c.current_effort(), Some(ReasoningEffort::Medium));
    }

    #[test]
    fn test_edit_success_goes_medium() {
        let mut c = ReasoningController::new(auto());
        c.observe_tool_batch(batch(&[("edit", true)], false, false));
        assert_eq!(c.current_effort(), Some(ReasoningEffort::Medium));
    }

    #[test]
    fn test_execute_process_success_goes_low() {
        let mut c = ReasoningController::new(auto());
        c.observe_tool_batch(batch(&[("execute_process", true)], false, false));
        assert_eq!(c.current_effort(), Some(ReasoningEffort::Low));
    }

    #[test]
    fn test_execute_process_failure_goes_high() {
        let mut c = ReasoningController::new(auto());
        c.observe_tool_batch(batch(&[("execute_process", false)], false, false));
        assert_eq!(c.current_phase(), ReasoningPhase::Recovery);
        assert_eq!(c.current_effort(), Some(ReasoningEffort::High));
    }

    #[test]
    fn test_remote_success_goes_medium_failure_high() {
        let mut c = ReasoningController::new(auto());
        c.observe_tool_batch(batch(
            &[("mcp_github_get_pull_request", true)],
            false,
            false,
        ));
        assert_eq!(c.current_effort(), Some(ReasoningEffort::Medium));
        c.observe_tool_batch(batch(
            &[("mcp_github_get_pull_request", false)],
            false,
            false,
        ));
        assert_eq!(c.current_effort(), Some(ReasoningEffort::High));
    }

    #[test]
    fn test_loop_and_stall_escalate() {
        let mut c = ReasoningController::new(auto());
        c.observe_tool_batch(batch(&[("search_text", true)], true, false));
        assert_eq!(c.current_effort(), Some(ReasoningEffort::High));
        let mut c = ReasoningController::new(auto());
        c.observe_tool_batch(batch(&[("search_text", true)], false, true));
        assert_eq!(c.current_effort(), Some(ReasoningEffort::High));
    }

    #[test]
    fn test_json_recovery_goes_high() {
        let mut c = ReasoningController::new(auto());
        c.observe_json_recovery();
        assert_eq!(c.current_effort(), Some(ReasoningEffort::High));
    }

    #[test]
    fn test_recovery_decays_after_normal_batch() {
        let mut c = ReasoningController::new(auto());
        c.observe_tool_batch(batch(&[("edit", false)], false, false));
        assert_eq!(c.current_effort(), Some(ReasoningEffort::High));
        c.observe_tool_batch(batch(&[("search_text", true)], false, false));
        assert_eq!(c.current_effort(), Some(ReasoningEffort::Low));
    }

    #[test]
    fn test_batch_severity_routine_plus_deliberative_is_medium() {
        let mut c = ReasoningController::new(auto());
        c.observe_tool_batch(batch(
            &[("search_text", true), ("fs_read", true)],
            false,
            false,
        ));
        assert_eq!(c.current_effort(), Some(ReasoningEffort::Medium));
        // Order-independent.
        let mut c2 = ReasoningController::new(auto());
        c2.observe_tool_batch(batch(
            &[("fs_read", true), ("search_text", true)],
            false,
            false,
        ));
        assert_eq!(c2.current_effort(), Some(ReasoningEffort::Medium));
    }

    #[test]
    fn test_batch_severity_failure_wins() {
        let mut c = ReasoningController::new(auto());
        c.observe_tool_batch(batch(
            &[("fs_read", true), ("execute_process", false)],
            false,
            false,
        ));
        assert_eq!(c.current_effort(), Some(ReasoningEffort::High));
        let mut c2 = ReasoningController::new(auto());
        c2.observe_tool_batch(batch(
            &[("execute_process", false), ("fs_read", true)],
            false,
            false,
        ));
        assert_eq!(c2.current_effort(), Some(ReasoningEffort::High));
    }

    #[test]
    fn test_fixed_and_off_modes() {
        let fixed_cfg = ReasoningConfig {
            mode: ReasoningMode::Fixed,
            fixed_effort: ReasoningEffort::High,
            ..ReasoningConfig::default()
        };
        let mut c = ReasoningController::new(fixed_cfg);
        assert_eq!(c.current_effort(), Some(ReasoningEffort::High));
        c.observe_tool_batch(batch(&[("edit", false)], false, false));
        assert_eq!(c.current_effort(), Some(ReasoningEffort::High));

        let off_cfg = ReasoningConfig {
            mode: ReasoningMode::Off,
            ..ReasoningConfig::default()
        };
        let c = ReasoningController::new(off_cfg);
        assert_eq!(c.current_effort(), None);
    }

    #[test]
    fn capability_endpoint_identity_is_not_url_substring() {
        for url in [
            "https://api.openai.com.example/v1",
            "https://example.invalid/api.openai.com/v1",
            "https://example.invalid/?next=https://openrouter.ai",
            "not-a-url-api.openai.com",
        ] {
            assert_eq!(
                provider_support(url, "gpt-5-mini"),
                ReasoningProviderSupport::Unknown,
                "{url}"
            );
        }
    }

    #[test]
    fn capability_unknown_models_do_not_inherit_reasoning() {
        for model in [
            "acme/gpt-5-mini",
            "gpt-5-future-private",
            "o3-custom-private",
        ] {
            assert_eq!(
                provider_support("https://api.openai.com/v1", model),
                ReasoningProviderSupport::Unknown,
                "{model}"
            );
        }
    }

    #[test]
    fn test_provider_openai_reasoning_supported() {
        assert_eq!(
            provider_support("https://api.openai.com/v1", "gpt-5-mini"),
            ReasoningProviderSupport::Supported
        );
        assert_eq!(
            provider_support("https://api.openai.com/v1", "openai/gpt-5-mini"),
            ReasoningProviderSupport::Supported
        );
        assert_eq!(
            provider_support("https://api.openai.com/v1", "o3-mini"),
            ReasoningProviderSupport::Supported
        );
    }

    #[test]
    fn test_provider_openai_non_reasoning_unsupported() {
        assert_eq!(
            provider_support("https://api.openai.com/v1", "gpt-4o-mini"),
            ReasoningProviderSupport::Unsupported
        );
        assert_eq!(
            provider_support("https://api.openai.com/v1", "gpt-4o"),
            ReasoningProviderSupport::Unsupported
        );
    }

    #[test]
    fn test_provider_openrouter_supported() {
        assert_eq!(
            provider_support("https://openrouter.ai/api/v1", "anything/model"),
            ReasoningProviderSupport::Supported
        );
    }

    #[test]
    fn test_provider_unknown() {
        assert_eq!(
            provider_support("https://example.invalid/v1", "some-model"),
            ReasoningProviderSupport::Unknown
        );
    }

    #[test]
    fn test_resolve_hint_modes() {
        // Auto + capable -> send.
        assert_eq!(
            resolve_reasoning_hint(
                "https://api.openai.com/v1",
                "gpt-5-mini",
                &ReasoningMode::Auto,
                Some(ReasoningEffort::Medium)
            ),
            Some(ReasoningEffort::Medium)
        );
        // Auto + non-reasoning -> none.
        assert_eq!(
            resolve_reasoning_hint(
                "https://api.openai.com/v1",
                "gpt-4o-mini",
                &ReasoningMode::Auto,
                Some(ReasoningEffort::Medium)
            ),
            None
        );
        // Auto + unknown -> none (fail-safe).
        assert_eq!(
            resolve_reasoning_hint(
                "https://example.invalid/v1",
                "m",
                &ReasoningMode::Auto,
                Some(ReasoningEffort::Medium)
            ),
            None
        );
        // Fixed + unknown -> send (escape hatch).
        assert_eq!(
            resolve_reasoning_hint(
                "https://example.invalid/v1",
                "m",
                &ReasoningMode::Fixed,
                Some(ReasoningEffort::Medium)
            ),
            Some(ReasoningEffort::Medium)
        );
        // Off -> never.
        assert_eq!(
            resolve_reasoning_hint(
                "https://api.openai.com/v1",
                "gpt-5-mini",
                &ReasoningMode::Off,
                Some(ReasoningEffort::High)
            ),
            None
        );
    }
}
