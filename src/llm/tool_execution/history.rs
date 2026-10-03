use crate::llm::observation::{
    MIN_OBSERVABLE_TOOL_CHARS, OBSERVATION_READ_TOOL_NAME, OBSERVATION_STUB_PREFIX,
    ObservationFootprint, ObservationGcReport, ObservationStore, SharedObservationStore,
    fallback_stub, is_observation_stub, new_shared_store, observation_stub, stub_observation_id,
};
use crate::llm::types::ChatMessage;
use crate::llm::{OpenAIClient, compact_conversation_history};
use anyhow::{Result, anyhow};
use std::collections::{BTreeMap, BTreeSet};
use tracing::{error, info, warn};

pub struct HistoryManager {
    messages: Vec<ChatMessage>,
    client: OpenAIClient,
    ui_tx: Option<std::sync::mpsc::Sender<String>>,
    fs_tools: crate::tools::FsTools,
    config: crate::config::AppConfig,
    observations: SharedObservationStore,
    unseen_tool_results: BTreeSet<String>,
}

/// Structured compaction report: overall reclaimed bytes plus how much is
/// recoverable, fallback, or protected because unseen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactionReport {
    pub reclaimed_bytes: usize,
    pub recoverable_offloads: usize,
    pub recoverable_original_bytes: usize,
    pub fallback_elisions: usize,
    pub skipped_unseen: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationReason {
    Superseded,
    Historical,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OffloadOutcome {
    Offloaded {
        reclaimed_bytes: usize,
        original_bytes: usize,
    },
    Fallback {
        reclaimed_bytes: usize,
    },
    SkippedUnseen,
    SkippedAlreadyStub,
    SkippedTooSmall,
    SkippedNoId,
    SkippedNotTool,
    SkippedNoSaving,
}

impl HistoryManager {
    pub fn new(
        client: OpenAIClient,
        messages: Vec<ChatMessage>,
        ui_tx: Option<std::sync::mpsc::Sender<String>>,
        fs_tools: crate::tools::FsTools,
        config: crate::config::AppConfig,
    ) -> Self {
        Self {
            messages,
            client,
            ui_tx,
            fs_tools,
            config,
            observations: new_shared_store(),
            unseen_tool_results: BTreeSet::new(),
        }
    }

    /// Save completed Responses tool batches before the next network request.
    /// Failed/cancelled turns retain pending results instead of reverting to old UI history.
    pub fn checkpoint_subscription(&self) -> Result<()> {
        if !self.client.is_subscription() || self.messages.is_empty() {
            return Ok(());
        }
        let Some(manager) = self
            .fs_tools
            .get_session_manager_wrapper()
            .get_session_manager()
        else {
            return Ok(());
        };
        let (persisted, observations, mut unseen) = self.persistable();
        // Durable projection only: request-scoped system messages never
        // persist. Provider state, assistant function calls, function call
        // outputs, and unseen-result protection are preserved untouched.
        let mut messages = crate::llm::durable_conversation_messages(persisted);
        let answered: BTreeSet<_> = messages
            .iter()
            .filter(|m| m.role == "tool")
            .filter_map(|m| m.tool_call_id.clone())
            .collect();
        let interrupted: Vec<_> = messages
            .iter()
            .flat_map(|m| &m.tool_calls)
            .filter_map(|call| call.id.as_ref())
            .filter(|id| !answered.contains(*id))
            .cloned()
            .collect();
        for id in interrupted {
            // The process may have stopped during a batch. Never claim that an
            // unfinished call ran or automatically replay its possible side effect.
            messages.push(ChatMessage { provider_state:None, role:"tool".into(),
                content:Some("Tool execution interrupted; outcome unknown. Inspect the workspace before deciding whether to retry.".into()),
                tool_calls:vec![], tool_call_id:Some(id.clone()) });
            unseen.insert(id);
        }
        crate::utils::safe_std_lock(manager, "session_manager")?
            .update_current_session_with_history_and_observations(
                &messages,
                Some(observations),
                Some(unseen),
            )
    }

    /// Restore a manager with a previously persisted observation store.
    /// Runs the shared restore reconciliation so legacy dead entries that
    /// are no longer reachable from `messages` are collected immediately
    /// (self-healing, no disk migration).
    pub fn with_observations(
        client: OpenAIClient,
        messages: Vec<ChatMessage>,
        ui_tx: Option<std::sync::mpsc::Sender<String>>,
        fs_tools: crate::tools::FsTools,
        config: crate::config::AppConfig,
        observations: ObservationStore,
        unseen_tool_results: BTreeSet<String>,
    ) -> Self {
        let mut this = Self {
            messages,
            client,
            ui_tx,
            fs_tools,
            config,
            observations: std::sync::Arc::new(std::sync::RwLock::new(observations)),
            unseen_tool_results,
        };
        this.reconcile_observations_after_restore("with_observations");
        this
    }

    /// Add a message to the history
    pub fn push(&mut self, message: ChatMessage) {
        self.messages.push(message);
    }

    /// Canonical tool-result insertion: appends the provider-compatible
    /// `role=tool` message and marks it unseen until a provider request
    /// successfully contains it. Preserves the existing message shape.
    pub fn push_tool_result(&mut self, tool_call_id: Option<String>, content: String) {
        let id_clone = tool_call_id.clone();
        self.messages.push(ChatMessage {
            provider_state: None,
            role: "tool".into(),
            content: Some(content),
            tool_calls: vec![],
            tool_call_id,
        });
        if let Some(id) = id_clone {
            self.unseen_tool_results.insert(id);
        }
    }

    /// Mark every pending tool result as seen after a successful provider
    /// request. Ordering invariant: call immediately after receiving the
    /// response to the request built from this manager's current state,
    /// before pushing that response's new assistant/tool messages. At that
    /// point every tool result in the request was consumed successfully.
    /// Network errors, timeouts, disconnects, or cancellations must not call
    /// this; those results stay inline.
    pub fn mark_sent_tool_results_seen(&mut self) {
        self.unseen_tool_results.clear();
    }

    pub fn unseen_count(&self) -> usize {
        self.unseen_tool_results.len()
    }

    /// Shared handle for the conversation-owned Observation Store. Cloned
    /// into the `ToolRuntime` for this run so `observation_read` sees only
    /// this conversation's offloads. Never global.
    pub fn observation_handle(&self) -> SharedObservationStore {
        self.observations.clone()
    }

    pub fn observations_snapshot(&self) -> ObservationStore {
        self.observations
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    pub fn unseen_snapshot(&self) -> BTreeSet<String> {
        self.unseen_tool_results.clone()
    }

    pub fn restore_observations(&mut self, store: ObservationStore, unseen: BTreeSet<String>) {
        match self.observations.write() {
            Ok(mut guard) => {
                *guard = store;
                self.unseen_tool_results = unseen;
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to restore observation store; keeping existing state");
                return;
            }
        }
        self.reconcile_observations_after_restore("restore");
    }

    /// Mark-and-sweep GC over the Observation Store.
    ///
    /// Roots are the current canonical `messages` only: every role's
    /// `content`, every `tool_calls[*].function.arguments`, and every string
    /// leaf in `provider_state.output` are scanned for currently stored
    /// (`known`) observation ids. Observation content itself is never a root,
    /// unknown `obs-*` strings are never added, and `unseen_tool_results` is
    /// untouched (a separate visibility concept).
    ///
    /// Fail-safe: lock failures return a zeroed (no-op, "skipped" rather
    /// than "empty store") report without deleting anything and never panic.
    /// `next_id` is preserved by the store primitive; removed ids are never
    /// reused.
    pub fn gc_unreferenced_observations(&mut self) -> ObservationGcReport {
        // Single write-lock critical section: snapshot known ids, mark live,
        // and sweep without releasing the guard. A read-then-write split
        // could sweep an entry inserted concurrently after the snapshot
        // without ever evaluating its reachability.
        let mut guard = match self.observations.write() {
            Ok(guard) => guard,
            Err(e) => {
                tracing::warn!(error = %e, "observation GC skipped: store lock unavailable");
                return ObservationGcReport::default();
            }
        };
        let known_ids = guard.ids();
        if known_ids.is_empty() {
            return ObservationGcReport::default();
        }
        let live_ids = Self::referenced_observation_ids(&self.messages, &known_ids);
        guard.retain_referenced(&live_ids)
    }

    /// Live-set marker: which `known_ids` are still reachable from canonical
    /// messages. Conservative (false positives keep an entry, false negatives
    /// must not drop a reachable one).
    fn referenced_observation_ids(
        messages: &[ChatMessage],
        known_ids: &BTreeSet<String>,
    ) -> BTreeSet<String> {
        let mut live = BTreeSet::new();
        if known_ids.is_empty() || messages.is_empty() {
            return live;
        }
        for msg in messages {
            if live.len() >= known_ids.len() {
                break;
            }
            if let Some(content) = msg.content.as_deref()
                && content.contains("obs-")
            {
                for id in known_ids {
                    if live.contains(id) {
                        continue;
                    }
                    if content.contains(id) {
                        live.insert(id.clone());
                    }
                }
            }
            for tc in &msg.tool_calls {
                if live.len() >= known_ids.len() {
                    break;
                }
                if !tc.function.arguments.contains("obs-") {
                    continue;
                }
                for id in known_ids {
                    if live.contains(id) {
                        continue;
                    }
                    if tc.function.arguments.contains(id) {
                        live.insert(id.clone());
                    }
                }
            }
            if let Some(state) = msg.provider_state.as_ref() {
                for value in &state.output {
                    if live.len() >= known_ids.len() {
                        break;
                    }
                    Self::mark_ids_in_json(value, known_ids, &mut live);
                }
            }
        }
        live
    }

    /// Current canonical messages' live observation set (for tests/telemetry).
    #[cfg(test)]
    pub(crate) fn collect_live_observation_ids(&self) -> BTreeSet<String> {
        let known_ids: BTreeSet<String> = match self.observations.read() {
            Ok(guard) => guard.ids(),
            Err(_) => return BTreeSet::new(),
        };
        Self::referenced_observation_ids(&self.messages, &known_ids)
    }

    fn mark_ids_in_json(
        value: &serde_json::Value,
        known_ids: &BTreeSet<String>,
        live: &mut BTreeSet<String>,
    ) {
        match value {
            serde_json::Value::String(s) => {
                if !s.contains("obs-") {
                    return;
                }
                for id in known_ids {
                    if live.contains(id) {
                        continue;
                    }
                    if s.contains(id) {
                        live.insert(id.clone());
                    }
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    if live.len() >= known_ids.len() {
                        break;
                    }
                    Self::mark_ids_in_json(item, known_ids, live);
                }
            }
            serde_json::Value::Object(map) => {
                for item in map.values() {
                    if live.len() >= known_ids.len() {
                        break;
                    }
                    Self::mark_ids_in_json(item, known_ids, live);
                }
            }
            _ => {}
        }
    }

    /// Shared restore reconciliation: store install -> live-set -> sweep.
    /// Used by `restore_observations` and `with_observations` so both paths
    /// share one GC contract. Logs only when entries were actually removed.
    fn reconcile_observations_after_restore(&mut self, reason: &'static str) {
        let report = self.gc_unreferenced_observations();
        if report.removed_entries > 0 {
            info!(
                observation_gc_reason = reason,
                removed_entries = report.removed_entries,
                removed_content_bytes = report.removed_content_bytes,
                remaining_entries = report.after_entries,
                remaining_content_bytes = report.after_content_bytes,
                "collected unreachable observations after restore"
            );
        }
    }

    /// Insert a message at a specific index
    pub fn insert(&mut self, index: usize, message: ChatMessage) {
        self.messages.insert(index, message);
    }

    pub fn last(&self) -> Option<&ChatMessage> {
        self.messages.last()
    }

    pub fn clear(&mut self) {
        // Full conversation reset: messages, Observation Store, and unseen
        // tracking are cleared together so no dead observation survives.
        // Matches `SessionData::clear_conversation_context()` which replaces
        // the store with a fresh one (a new conversation restarts ids).
        self.messages.clear();
        match self.observations.write() {
            Ok(mut guard) => {
                *guard = ObservationStore::new();
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to clear observation store; keeping existing state");
            }
        }
        self.unseen_tool_results.clear();
    }

    pub fn iter(&self) -> std::slice::Iter<'_, ChatMessage> {
        self.messages.iter()
    }

    pub fn as_slice(&self) -> &[ChatMessage] {
        self.messages.as_slice()
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Resolve the logical tool name for a `tool_call_id` by scanning prior
    /// assistant `tool_calls`. Falls back to `"tool"` when unknown.
    fn tool_name_for_call_id(&self, tool_call_id: &str) -> String {
        for msg in &self.messages {
            if msg.role == "assistant" {
                for tc in &msg.tool_calls {
                    if tc.id.as_deref() == Some(tool_call_id) {
                        return tc.function.name.clone();
                    }
                }
            }
        }
        "tool".to_string()
    }

    fn tool_signature_for_call_id(&self, tool_call_id: &str) -> Option<(String, String)> {
        for msg in &self.messages {
            if msg.role == "assistant" {
                for tc in &msg.tool_calls {
                    if tc.id.as_deref() == Some(tool_call_id) {
                        return Some((tc.function.name.clone(), tc.function.arguments.clone()));
                    }
                }
            }
        }
        None
    }

    fn serialized_message_bytes(msg: &ChatMessage) -> usize {
        crate::llm::context_budget::serialized_size(msg)
            .map(|n| n as usize)
            .unwrap_or(0)
    }

    /// Single helper for all deterministic offload paths. Verifies the
    /// message is `role=tool`, refuses unseen or already-offloaded results,
    /// resolves the logical tool name, checks minimum useful size, attempts
    /// bounded store insertion, and replaces content with a deterministic
    /// reference stub. Returns exact reclaimed serialized bytes.
    fn offload_tool_result(&mut self, index: usize, _reason: ObservationReason) -> OffloadOutcome {
        if index >= self.messages.len() {
            return OffloadOutcome::SkippedNotTool;
        }
        if self.messages[index].role != "tool" {
            return OffloadOutcome::SkippedNotTool;
        }
        let tool_call_id = match self.messages[index].tool_call_id.clone() {
            Some(id) => id,
            None => return OffloadOutcome::SkippedNoId,
        };
        let content = match self.messages[index].content.clone() {
            Some(c) => c,
            None => return OffloadOutcome::SkippedTooSmall,
        };
        if self.unseen_tool_results.contains(&tool_call_id) {
            return OffloadOutcome::SkippedUnseen;
        }
        if is_observation_stub(&content)
            || content.starts_with("[cleared tool result")
            || content.starts_with(crate::llm::observation::CLEARED_STUB_PREFIX)
        {
            return OffloadOutcome::SkippedAlreadyStub;
        }
        let tool_name = self.tool_name_for_call_id(&tool_call_id);
        // `observation_read` output must never create another observation.
        let is_self_read = tool_name == OBSERVATION_READ_TOOL_NAME;
        if content.chars().count() < MIN_OBSERVABLE_TOOL_CHARS {
            return OffloadOutcome::SkippedTooSmall;
        }
        let original_msg_bytes = Self::serialized_message_bytes(&self.messages[index]);
        let original_content_bytes = content.len();

        if is_self_read {
            // Fall back to the ordinary non-recoverable stub; the original
            // `obs-*` remains the retrieval authority.
            let stub = fallback_stub(&tool_name, original_content_bytes);
            let candidate = ChatMessage {
                provider_state: None,
                role: "tool".into(),
                content: Some(stub),
                tool_calls: vec![],
                tool_call_id: Some(tool_call_id),
            };
            let replacement_bytes = Self::serialized_message_bytes(&candidate);
            if replacement_bytes >= original_msg_bytes {
                return OffloadOutcome::SkippedNoSaving;
            }
            let reclaimed = original_msg_bytes.saturating_sub(replacement_bytes);
            self.messages[index] = candidate;
            return OffloadOutcome::Fallback {
                reclaimed_bytes: reclaimed,
            };
        }

        // Attempt bounded insertion of the exact model-visible content.
        // Pre-check: never perform an "optimization" that grows the prompt.
        // Estimate the stub size with the peeked next id before allocating.
        {
            let peeked = self
                .observations
                .read()
                .map(|s| s.peek_next_id())
                .unwrap_or_else(|_| "obs-000001".to_string());
            let probe_stub = observation_stub(&tool_name, original_content_bytes, &peeked);
            let probe = ChatMessage {
                provider_state: None,
                role: "tool".into(),
                content: Some(probe_stub),
                tool_calls: vec![],
                tool_call_id: Some(tool_call_id.clone()),
            };
            if Self::serialized_message_bytes(&probe) >= original_msg_bytes {
                return OffloadOutcome::SkippedNoSaving;
            }
        }
        let inserted_id = {
            let mut store = match self.observations.write() {
                Ok(g) => g,
                Err(_) => return OffloadOutcome::SkippedNoSaving,
            };
            store.insert(
                tool_call_id.clone(),
                tool_name.clone(),
                content,
                original_msg_bytes,
            )
        };
        match inserted_id {
            Some(obs_id) => {
                let stub = observation_stub(&tool_name, original_content_bytes, &obs_id);
                let candidate = ChatMessage {
                    provider_state: None,
                    role: "tool".into(),
                    content: Some(stub),
                    tool_calls: vec![],
                    tool_call_id: Some(tool_call_id),
                };
                let replacement_bytes = Self::serialized_message_bytes(&candidate);
                // Pre-check guarantees saving; this re-check is defensive
                // only (e.g. id-width rollover). Roll the insert back so no
                // orphan entry consumes store budget without a stub.
                if replacement_bytes >= original_msg_bytes {
                    if let Ok(mut store) = self.observations.write() {
                        store.remove(&obs_id);
                    }
                    return OffloadOutcome::SkippedNoSaving;
                }
                let reclaimed = original_msg_bytes.saturating_sub(replacement_bytes);
                self.messages[index] = candidate;
                OffloadOutcome::Offloaded {
                    reclaimed_bytes: reclaimed,
                    original_bytes: original_content_bytes,
                }
            }
            None => {
                // Store full: safe non-recoverable fallback, never evicting.
                let stub = fallback_stub(&tool_name, original_content_bytes);
                let candidate = ChatMessage {
                    provider_state: None,
                    role: "tool".into(),
                    content: Some(stub),
                    tool_calls: vec![],
                    tool_call_id: Some(tool_call_id),
                };
                let replacement_bytes = Self::serialized_message_bytes(&candidate);
                if replacement_bytes >= original_msg_bytes {
                    return OffloadOutcome::SkippedNoSaving;
                }
                let reclaimed = original_msg_bytes.saturating_sub(replacement_bytes);
                self.messages[index] = candidate;
                OffloadOutcome::Fallback {
                    reclaimed_bytes: reclaimed,
                }
            }
        }
    }

    /// Recoverable offload pass over stale tool results. Never offloads
    /// unseen results, and routes `observation_read` outputs to the fallback
    /// stub. Includes a superseded pass (older duplicates of the same tool
    /// name+args, which is safe to rewrite even inside the recent window
    /// because the newer copy stays inline) plus the historical pass over
    /// everything older than the recent `keep_recent` window, both through
    /// `offload_tool_result`.
    pub fn offload_stale_tool_results(
        &mut self,
        prompt_tokens: u32,
        threshold: u32,
        ratio: f32,
        keep_recent: usize,
    ) -> CompactionReport {
        if threshold == 0 || (prompt_tokens as f32) < threshold as f32 * ratio {
            return CompactionReport::default();
        }
        self.offload_stale_tool_results_for_pressure(keep_recent)
    }

    /// Unconditional recoverable offload primitive for the preflight
    /// governor. Same mechanics as [`Self::offload_stale_tool_results`]
    /// without the previous-usage threshold gate: the caller has already
    /// measured current-request pressure. Contract is unchanged (unseen
    /// protected, stubs skipped, store capacity respected, no eviction).
    pub fn offload_stale_tool_results_for_pressure(
        &mut self,
        keep_recent: usize,
    ) -> CompactionReport {
        let mut report = CompactionReport::default();
        // Collect tool message indices in order.
        let tool_indices: Vec<usize> = self
            .messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == "tool" && m.content.is_some())
            .map(|(i, _)| i)
            .collect();
        if tool_indices.is_empty() {
            return report;
        }

        // Pass 1 (superseded): older duplicates of identical name+args.
        let mut groups: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
        for &idx in &tool_indices {
            let call_id = match &self.messages[idx].tool_call_id {
                Some(id) => id.clone(),
                None => continue,
            };
            if let Some(sig) = self.tool_signature_for_call_id(&call_id) {
                groups.entry(sig).or_default().push(idx);
            }
        }
        for (_, indices) in groups {
            if indices.len() < 2 {
                continue;
            }
            // All but the last are superseded.
            for &idx in &indices[..indices.len() - 1] {
                match self.offload_tool_result(idx, ObservationReason::Superseded) {
                    OffloadOutcome::Offloaded {
                        reclaimed_bytes,
                        original_bytes,
                    } => {
                        report.reclaimed_bytes += reclaimed_bytes;
                        report.recoverable_offloads += 1;
                        report.recoverable_original_bytes += original_bytes;
                    }
                    OffloadOutcome::Fallback { reclaimed_bytes } => {
                        report.reclaimed_bytes += reclaimed_bytes;
                        report.fallback_elisions += 1;
                    }
                    OffloadOutcome::SkippedUnseen => {
                        report.skipped_unseen += 1;
                    }
                    _ => {}
                }
            }
        }

        // Pass 2 (historical): everything older than the recent window.
        // Recompute indices (messages mutated in place, indices stable).
        let tool_indices: Vec<usize> = self
            .messages
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                m.role == "tool"
                    && m.content.is_some()
                    && !m.content.as_deref().is_some_and(|c| {
                        is_observation_stub(c) || c.starts_with("[cleared tool result")
                    })
            })
            .map(|(i, _)| i)
            .collect();
        if tool_indices.len() <= keep_recent {
            return report;
        }
        let cutoff = tool_indices.len() - keep_recent;
        for &idx in &tool_indices[..cutoff] {
            match self.offload_tool_result(idx, ObservationReason::Historical) {
                OffloadOutcome::Offloaded {
                    reclaimed_bytes,
                    original_bytes,
                } => {
                    report.reclaimed_bytes += reclaimed_bytes;
                    report.recoverable_offloads += 1;
                    report.recoverable_original_bytes += original_bytes;
                }
                OffloadOutcome::Fallback { reclaimed_bytes } => {
                    report.reclaimed_bytes += reclaimed_bytes;
                    report.fallback_elisions += 1;
                }
                OffloadOutcome::SkippedUnseen => {
                    report.skipped_unseen += 1;
                }
                _ => {}
            }
        }
        report
    }

    /// Observation footprint: lifetime storage vs active prompt savings.
    pub fn observation_footprint(&self) -> ObservationFootprint {
        let store = self
            .observations
            .read()
            .map(|g| g.clone())
            .unwrap_or_default();
        let mut active_references = 0usize;
        let mut active_original = 0usize;
        let mut active_stubs = 0usize;
        for msg in &self.messages {
            if msg.role != "tool" {
                continue;
            }
            let content = match &msg.content {
                Some(c) => c,
                None => continue,
            };
            if !is_observation_stub(content) {
                continue;
            }
            let Some(id) = stub_observation_id(content) else {
                continue;
            };
            let Some(obs) = store.get(&id) else {
                continue;
            };
            active_references += 1;
            active_original += obs.original_message_json_bytes;
            active_stubs += Self::serialized_message_bytes(msg);
        }
        ObservationFootprint {
            version: crate::llm::observation::OBSERVATION_FOOTPRINT_VERSION,
            stored_entries: store.len(),
            stored_content_bytes: store.stored_content_bytes(),
            active_references,
            active_original_message_bytes: active_original,
            active_stub_message_bytes: active_stubs,
            active_reclaimed_json_bytes: active_original.saturating_sub(active_stubs),
        }
    }

    /// Total serialized JSON bytes of provider-bound messages.
    pub fn messages_json_bytes(&self) -> usize {
        crate::llm::context_budget::serialized_size(&self.messages)
            .map(|n| n as usize)
            .unwrap_or(0)
    }

    /// Serialized bytes of tool messages only.
    pub fn tool_result_json_bytes(&self) -> usize {
        let tools: Vec<&ChatMessage> = self.messages.iter().filter(|m| m.role == "tool").collect();
        crate::llm::context_budget::serialized_size(&tools)
            .map(|n| n as usize)
            .unwrap_or(0)
    }

    /// Clear stale tool results to free context space before compaction is
    /// needed (mirrors "context editing": old tool outputs are replaced with a
    /// short placeholder while the conversation flow is preserved).
    ///
    /// Only runs once the prompt token usage crosses `threshold * ratio`, and
    /// never touches the most recent `keep_recent` tool messages.
    pub fn clear_stale_tool_results(
        messages: &mut [ChatMessage],
        prompt_tokens: u32,
        threshold: u32,
        ratio: f32,
        keep_recent: usize,
    ) -> usize {
        if threshold == 0 || (prompt_tokens as f32) < threshold as f32 * ratio {
            return 0;
        }

        // Collect indices of tool messages.
        let tool_indices: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == "tool" && m.content.is_some())
            .map(|(i, _)| i)
            .collect();

        if tool_indices.len() <= keep_recent {
            return 0;
        }

        let cutoff = tool_indices.len() - keep_recent;
        let mut cleared = 0usize;
        for &idx in &tool_indices[..cutoff] {
            let msg = &mut messages[idx];
            let already_cleared = msg.content.as_deref().is_some_and(|c| {
                c.starts_with("[cleared tool result") || c.starts_with(OBSERVATION_STUB_PREFIX)
            });
            if already_cleared {
                continue;
            }
            msg.content = Some("[cleared tool result: earlier tool output removed to free context; re-run the tool if needed]".to_string());
            cleared += 1;
        }
        cleared
    }

    /// Check if proactive compaction is needed and perform it if so
    pub async fn check_and_compact_proactive(&mut self) -> Result<bool> {
        // Free stale tool results first (recoverable offload) before
        // considering a full paid compaction.
        let report = self.offload_stale_tool_results(
            self.client.get_prompt_tokens_used(),
            self.config.get_effective_compaction_limit(),
            0.6,
            3,
        );
        if report.reclaimed_bytes > 0 {
            info!(
                reclaimed_bytes = report.reclaimed_bytes,
                observation_offloads = report.recoverable_offloads,
                observation_original_bytes = report.recoverable_original_bytes,
                fallback_elisions = report.fallback_elisions,
                skipped_unseen = report.skipped_unseen,
                "Offloaded stale tool results to Observation Store"
            );
            if let Some(tx) = &self.ui_tx {
                let _ = tx.send(format!(
                    "::status:waiting:Offloaded {} tool result(s) ({} recoverable) to free context...",
                    report.recoverable_offloads + report.fallback_elisions,
                    report.recoverable_offloads
                ));
            }
        } else if report.skipped_unseen > 0 {
            // Nothing reclaimed (all candidates still unseen): debug-level
            // diagnostics only, never a user-facing "Offloaded 0" status.
            tracing::debug!(
                skipped_unseen = report.skipped_unseen,
                "offload pass skipped unseen tool results"
            );
        }

        let last_prompt_tokens = self.client.get_prompt_tokens_used();
        let effective_limit = self.config.get_effective_compaction_limit();

        // Only compact if we are over the limit AND we have enough history to meaningful compact
        if last_prompt_tokens > effective_limit && self.messages.len() > 2 {
            warn!(
                current_tokens = last_prompt_tokens,
                limit = effective_limit,
                "Proactive compaction triggered"
            );

            if let Some(tx) = &self.ui_tx {
                let _ = tx.send(
                    "::status:compacting:Context limits approaching, summarizing history..."
                        .to_string(),
                );
            }

            self.perform_compaction().await
        } else {
            Ok(false)
        }
    }

    /// Reactive compaction when context length is exceeded
    pub async fn compact_reactive(&mut self) -> Result<bool> {
        warn!("Context length exceeded in agent loop. Attempting to compact history.");

        if let Some(tx) = &self.ui_tx {
            let _ = tx.send(
                "::status:compacting:Context limits reached, summarizing history...".to_string(),
            );
        }

        self.perform_compaction().await
    }

    pub fn into_messages(mut self) -> Vec<ChatMessage> {
        if let Err(error) = self.checkpoint_subscription() {
            warn!(%error, "could not checkpoint Responses history before transfer");
        }
        std::mem::take(&mut self.messages)
    }

    /// Borrow messages plus the observation snapshot for persistence.
    pub fn persistable(&self) -> (Vec<ChatMessage>, ObservationStore, BTreeSet<String>) {
        (
            self.messages.clone(),
            self.observations_snapshot(),
            self.unseen_snapshot(),
        )
    }

    /// Oldest assistant `tool_calls` message holding an unseen result.
    ///
    /// Everything from that index onward is the protected suffix: the
    /// assistant invocation plus its tool results (parallel batches kept as
    /// a unit) plus any later messages. Returns `None` when no unseen
    /// results exist. When unseen ids exist but no assistant call resolves,
    /// callers must fail closed (no compaction) rather than split the
    /// protocol pairing.
    pub fn protected_suffix_start_for_unseen(&self) -> Option<usize> {
        if self.unseen_tool_results.is_empty() {
            return None;
        }
        let mut earliest: Option<usize> = None;
        for (idx, msg) in self.messages.iter().enumerate() {
            if msg.role != "assistant" || msg.tool_calls.is_empty() {
                continue;
            }
            let holds_unseen = msg.tool_calls.iter().any(|tc| {
                tc.id
                    .as_deref()
                    .is_some_and(|id| self.unseen_tool_results.contains(id))
            });
            if holds_unseen {
                earliest = Some(earliest.map_or(idx, |e: usize| e.min(idx)));
            }
        }
        earliest
    }

    /// Preflight/retry entry point for budget-driven compaction.
    /// Always unseen-safe; see [`Self::protected_suffix_start_for_unseen`].
    pub async fn compact_for_budget_pressure(&mut self) -> Result<bool> {
        self.perform_compaction().await
    }

    async fn perform_compaction(&mut self) -> Result<bool> {
        let before_bytes = self.messages_json_bytes();
        let unseen = self.unseen_count();
        let protect_start = self.protected_suffix_start_for_unseen();
        if unseen > 0 && protect_start.is_none() {
            warn!(
                unseen,
                "refusing compaction: unseen tool results without resolvable assistant call"
            );
            return Ok(false);
        }

        // Split into compactable prefix + exact protected suffix.
        let (prefix, suffix_len, suffix_unseen) = match protect_start {
            None => (self.messages.clone(), 0usize, 0usize),
            Some(start) => {
                if start >= self.messages.len() {
                    warn!(
                        unseen,
                        start,
                        len = self.messages.len(),
                        "refusing compaction: protected suffix out of bounds"
                    );
                    return Ok(false);
                }
                let prefix = self.messages[..start].to_vec();
                // Prefix without summarization value: keep the unseen batch
                // exact and report no compaction.
                let non_system = prefix.iter().filter(|m| m.role != "system").count();
                if prefix.len() <= 2 || non_system == 0 {
                    warn!(
                        unseen,
                        prefix_len = prefix.len(),
                        "refusing compaction: compactable prefix too small, keeping unseen batch exact"
                    );
                    return Ok(false);
                }
                let suffix_len = self.messages.len() - start;
                (prefix, suffix_len, unseen)
            }
        };
        let protected_suffix: Vec<ChatMessage> = match protect_start {
            None => Vec::new(),
            Some(start) => self.messages[start..].to_vec(),
        };

        let params = crate::llm::compact_history::CompactParams {
            client: self.client.clone(),
            model: self.config.model.clone(),
            fs_tools: self.fs_tools.clone(),
            history: prefix.clone(),
            cfg: self.config.clone(),
        };

        match compact_conversation_history(params).await {
            Ok(compact_result) => {
                if compact_result.metadata.success {
                    let after_prefix = match protect_start {
                        None => Self::merge_compacted_history(
                            &self.messages,
                            compact_result.compacted_message,
                        ),
                        Some(_) => Self::merge_compacted_with_protected_suffix(
                            &prefix,
                            &protected_suffix,
                            compact_result.compacted_message,
                        ),
                    };
                    let after_bytes = crate::llm::context_budget::serialized_size(&after_prefix)
                        .map(|n| n as usize)
                        .unwrap_or(0);
                    info!(
                        budget_action = "compact",
                        protected_unseen_count = suffix_unseen,
                        protected_suffix_messages = suffix_len,
                        before_bytes,
                        after_bytes,
                        "History compaction successful (unseen-safe)"
                    );

                    self.messages = after_prefix;

                    // Post-compaction live-set GC: the summary + tail is now
                    // canonical, so stubs dropped from the tail are no longer
                    // reachable. Mark must run after the assignment, never
                    // before (pre-mark would keep soon-dead stubs alive).
                    let gc = self.gc_unreferenced_observations();
                    if gc.removed_entries > 0 {
                        info!(
                            observation_gc_reason = "compaction",
                            removed_entries = gc.removed_entries,
                            removed_content_bytes = gc.removed_content_bytes,
                            remaining_entries = gc.after_entries,
                            remaining_content_bytes = gc.after_content_bytes,
                            "collected unreachable observations after compaction"
                        );
                    }

                    if let Some(tx) = &self.ui_tx {
                        let _ = tx
                            .send("::status:waiting:History compacted. Continuing...".to_string());
                    }
                    Ok(true)
                } else {
                    let err_msg = format!(
                        "Compaction failed: {:?}",
                        compact_result.metadata.error_message
                    );
                    error!("{}", err_msg);
                    // Return error but as a result, not breaking execution if possible
                    // Actually, if compaction fails, we can't do much. returning false or error
                    Err(anyhow!(err_msg))
                }
            }
            Err(e) => {
                error!("Compaction error: {}", e);
                Err(e)
            }
        }
    }

    /// Merge a prefix-only summary with the exact protected suffix.
    ///
    /// Layout: pruned prefix systems + summary + recent prefix tail +
    /// exact protected suffix.
    ///
    /// The suffix (assistant tool-call batch + unseen results + later
    /// messages, byte-identical) is never sent to the compactor.
    fn merge_compacted_with_protected_suffix(
        prefix: &[ChatMessage],
        protected_suffix: &[ChatMessage],
        compacted: ChatMessage,
    ) -> Vec<ChatMessage> {
        const TAIL_BUDGET_CHARS: usize = 8_000;
        const MAX_SYSTEM_MESSAGES: usize = 8;
        const MAX_SYSTEM_MESSAGE_CHARS: usize = 4_000;

        let mut new_history: Vec<ChatMessage> = Self::prune_system_messages(
            prefix.iter().filter(|m| m.role == "system"),
            MAX_SYSTEM_MESSAGES,
            MAX_SYSTEM_MESSAGE_CHARS,
        );
        new_history.push(compacted);
        new_history.extend(Self::tail_messages(prefix, TAIL_BUDGET_CHARS));
        new_history.extend(protected_suffix.iter().cloned());
        new_history
    }

    /// Helper to merge existing system messages with the compacted state and
    /// a short tail of recent messages.
    ///
    /// Keeps:
    /// - pruned system messages (deduped, bounded) so the system prompt and
    ///   recent interventions survive,
    /// - the compacted summary,
    /// - the most recent non-system messages (within a character budget) so
    ///   the model can continue mid-task without re-discovering state.
    fn merge_compacted_history(
        original: &[ChatMessage],
        compacted: ChatMessage,
    ) -> Vec<ChatMessage> {
        const TAIL_BUDGET_CHARS: usize = 8_000;
        const MAX_SYSTEM_MESSAGES: usize = 8;
        const MAX_SYSTEM_MESSAGE_CHARS: usize = 4_000;

        let mut new_history: Vec<ChatMessage> = Self::prune_system_messages(
            original.iter().filter(|m| m.role == "system"),
            MAX_SYSTEM_MESSAGES,
            MAX_SYSTEM_MESSAGE_CHARS,
        );
        new_history.push(compacted);
        new_history.extend(Self::tail_messages(original, TAIL_BUDGET_CHARS));
        new_history
    }

    /// Dedupe system messages (by normalized prefix) and cap their count and
    /// size so loop warnings cannot accumulate unbounded across compactions.
    fn prune_system_messages<'a, I>(
        messages: I,
        max_count: usize,
        max_chars: usize,
    ) -> Vec<ChatMessage>
    where
        I: Iterator<Item = &'a ChatMessage>,
    {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut kept = Vec::new();
        let mut overflow = 0usize;
        for msg in messages {
            let content = msg.content.as_deref().unwrap_or("");
            let key: String = content
                .chars()
                .take(80)
                .map(|c| if c.is_whitespace() { ' ' } else { c })
                .collect();
            if !seen.insert(key) {
                // Skip repeated interventions with the same content prefix.
                continue;
            }
            if kept.len() >= max_count {
                overflow += 1;
                continue;
            }
            let mut msg = msg.clone();
            if content.chars().count() > max_chars {
                let mut cut = max_chars;
                while cut > 0 && !content.is_char_boundary(cut) {
                    cut -= 1;
                }
                msg.content = Some(format!("{}\n[system message truncated]", &content[..cut]));
            }
            kept.push(msg);
        }
        if overflow > 0 {
            tracing::debug!(
                dropped = overflow,
                "dropped overflow system messages during compaction"
            );
        }
        kept
    }

    /// Take the most recent non-system messages whose combined size fits the
    /// budget, capped to the last few conversation units. Tool-call pairing is
    /// preserved: an assistant message with tool calls and its tool responses
    /// are kept or dropped as a unit.
    fn tail_messages(original: &[ChatMessage], budget: usize) -> Vec<ChatMessage> {
        const MAX_TAIL_GROUPS: usize = 3;

        let msg_len = |m: &ChatMessage| -> usize {
            let mut len = m.content.as_deref().map(str::len).unwrap_or(0);
            for tc in &m.tool_calls {
                len += tc.function.name.len() + tc.function.arguments.len();
            }
            len
        };

        // Group each assistant tool-call message with the tool responses that
        // immediately follow it so we never cut a pair in half.
        let mut groups: Vec<Vec<&ChatMessage>> = Vec::new();
        for msg in original.iter().filter(|m| m.role != "system") {
            match msg.role.as_str() {
                "assistant" if !msg.tool_calls.is_empty() => groups.push(vec![msg]),
                "tool" => {
                    if let Some(group) = groups.last_mut()
                        && group.first().is_some_and(|head| {
                            head.role == "assistant" && !head.tool_calls.is_empty()
                        })
                    {
                        group.push(msg);
                    } else {
                        groups.push(vec![msg]);
                    }
                }
                _ => groups.push(vec![msg]),
            }
        }

        // Only consider the most recent few units.
        let window_start = groups.len().saturating_sub(MAX_TAIL_GROUPS);

        let mut total = 0usize;
        let mut take_from = groups.len();
        for (idx, group) in groups.iter().enumerate().rev() {
            let group_len: usize = group.iter().map(|m| msg_len(m)).sum();
            if idx < window_start {
                break;
            }
            if total + group_len > budget {
                break;
            }
            total += group_len;
            take_from = idx;
        }

        if take_from >= groups.len() {
            return Vec::new();
        }
        groups[take_from..]
            .iter()
            .flat_map(|group| group.iter().map(|m| (*m).clone()))
            .collect()
    }
}

impl Drop for HistoryManager {
    fn drop(&mut self) {
        if let Err(error) = self.checkpoint_subscription() {
            warn!(%error, "could not checkpoint Responses history on exit");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            provider_state: None,
            role: role.to_string(),
            content: Some(content.to_string()),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    fn make_tool_msg(tool_call_id: &str, content: &str) -> ChatMessage {
        ChatMessage {
            provider_state: None,
            role: "tool".to_string(),
            content: Some(content.to_string()),
            tool_calls: vec![],
            tool_call_id: Some(tool_call_id.to_string()),
        }
    }

    fn make_assistant_with_tool_calls(call_id: &str, content: &str) -> ChatMessage {
        ChatMessage {
            provider_state: None,
            role: "assistant".to_string(),
            content: Some(content.to_string()),
            tool_calls: vec![crate::llm::types::ToolCall {
                id: Some(call_id.to_string()),
                r#type: "function".to_string(),
                function: crate::llm::types::ToolCallFunction {
                    name: "fs_read".to_string(),
                    arguments: "{}".to_string(),
                },
            }],
            tool_call_id: None,
        }
    }

    fn test_manager(messages: Vec<ChatMessage>) -> HistoryManager {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = crate::config::AppConfig {
            project_root: dir.path().to_path_buf(),
            ..Default::default()
        };
        let fs = crate::tools::FsTools::new(
            std::sync::Arc::new(tokio::sync::RwLock::new(None)),
            std::sync::Arc::new(config.clone()),
        );
        let client =
            crate::llm::client_core::OpenAIClient::new("http://127.0.0.1:1", "k").expect("client");
        HistoryManager::new(client, messages, None, fs, config)
    }

    #[test]
    fn test_merge_compacted_history_keeps_system_summary_and_tail() {
        let original = vec![
            make_msg("system", "System Prompt"),
            make_msg("user", "User 1"),
            make_msg("assistant", "Assistant 1"),
            make_msg("system", "Loop Warning"),
            make_msg("user", "User 2"),
        ];

        let compacted = make_msg("user", "Summary");

        let result = HistoryManager::merge_compacted_history(&original, compacted);

        assert_eq!(result.len(), 6);
        assert_eq!(result[0].role, "system");
        assert_eq!(result[0].content.as_deref(), Some("System Prompt"));
        assert_eq!(result[1].role, "system");
        assert_eq!(result[1].content.as_deref(), Some("Loop Warning"));
        assert_eq!(result[2].role, "user");
        assert_eq!(result[2].content.as_deref(), Some("Summary"));
        // Recent tail preserved after the summary.
        assert_eq!(result[3].content.as_deref(), Some("User 1"));
        assert_eq!(result[4].content.as_deref(), Some("Assistant 1"));
        assert_eq!(result[5].content.as_deref(), Some("User 2"));
    }

    #[test]
    fn test_merge_compacted_history_no_system_messages() {
        let original = vec![
            make_msg("user", "User 1"),
            make_msg("assistant", "Assistant 1"),
        ];

        let compacted = make_msg("user", "Summary");

        let result = HistoryManager::merge_compacted_history(&original, compacted);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].content.as_deref(), Some("Summary"));
        assert_eq!(result[1].content.as_deref(), Some("User 1"));
    }

    #[test]
    fn test_merge_compacted_history_keeps_tool_call_pairs_intact() {
        let original = vec![
            make_msg("system", "System Prompt"),
            make_msg("user", "Task"),
            make_assistant_with_tool_calls("call_1", "reading file"),
            make_tool_msg("call_1", "{\"content\": \"file body\"}"),
            make_msg("assistant", "done"),
        ];

        let compacted = make_msg("user", "Summary");
        let result = HistoryManager::merge_compacted_history(&original, compacted);

        // Summary + tail (user, assistant+tool pair, assistant).
        assert_eq!(result.len(), 6);
        // The tail must not start with an orphan tool message.
        assert_eq!(result[2].role, "user");
        assert_eq!(result[3].role, "assistant");
        assert_eq!(result[4].role, "tool");
        assert_eq!(result[4].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(result[5].role, "assistant");
    }

    #[test]
    fn test_merge_compacted_history_dedupes_repeated_system_warnings() {
        let original: Vec<ChatMessage> = vec![
            make_msg("system", "System Prompt"),
            make_msg("system", "WARNING: stalled progress"),
            make_msg("user", "u1"),
            make_msg("system", "WARNING: stalled progress"),
            make_msg("system", "WARNING: stalled progress"),
        ];
        let compacted = make_msg("user", "Summary");
        let result = HistoryManager::merge_compacted_history(&original, compacted);
        let system_texts: Vec<_> = result
            .iter()
            .filter(|m| m.role == "system")
            .map(|m| m.content.clone().unwrap_or_default())
            .collect();
        assert_eq!(
            system_texts.len(),
            2,
            "dedupe repeated warnings: {system_texts:?}"
        );
    }

    #[test]
    fn test_merge_compacted_history_tail_respects_budget() {
        let big = "x".repeat(8_500);
        let original = vec![
            make_msg("system", "System Prompt"),
            make_msg("user", "old task"),
            make_msg("assistant", big.as_str()),
            make_msg("assistant", "middle"),
            make_msg("user", "recent question"),
        ];
        let compacted = make_msg("user", "Summary");
        let result = HistoryManager::merge_compacted_history(&original, compacted);

        // Tail keeps at most the last 3 units within the 8,000 budget: the
        // 8,500-char assistant message exceeds it, so the tail is
        // ["middle", "recent question"] and older units are dropped.
        let contents: Vec<_> = result
            .iter()
            .map(|m| m.content.clone().unwrap_or_default())
            .collect();
        assert!(contents.contains(&"recent question".to_string()));
        assert!(contents.contains(&"middle".to_string()));
        assert!(!contents.contains(&big));
        assert!(!contents.contains(&"old task".to_string()));
    }

    #[test]
    fn test_prune_system_messages_caps_count_and_keeps_first() {
        let original: Vec<ChatMessage> = (0..20)
            .map(|i| {
                make_msg(
                    "system",
                    &format!("unique-warning-{i} with enough text here!!"),
                )
            })
            .collect();
        let pruned = HistoryManager::prune_system_messages(original.iter(), 8, 4_000);
        assert_eq!(pruned.len(), 8);
    }

    #[test]
    fn test_clear_stale_tool_results_clears_old_but_keeps_recent() {
        let mut messages = vec![
            make_msg("system", "System Prompt"),
            make_msg("user", "task"),
            make_tool_msg("c1", "old result 1"),
            make_tool_msg("c2", "old result 2"),
            make_tool_msg("c3", "old result 3"),
            make_tool_msg("c4", "recent result 4"),
            make_tool_msg("c5", "recent result 5"),
        ];

        let cleared = HistoryManager::clear_stale_tool_results(&mut messages, 800, 1_000, 0.6, 2);
        assert_eq!(cleared, 3);
        assert!(
            messages[2]
                .content
                .as_deref()
                .unwrap()
                .starts_with("[cleared tool result"),
            "old results cleared"
        );
        assert_eq!(messages[5].content.as_deref(), Some("recent result 4"));
        assert_eq!(messages[6].content.as_deref(), Some("recent result 5"));
    }

    #[test]
    fn test_clear_stale_tool_results_skips_below_threshold() {
        let mut messages = vec![
            make_tool_msg("c1", "result"),
            make_tool_msg("c2", "result"),
            make_tool_msg("c3", "result"),
            make_tool_msg("c4", "result"),
        ];
        let cleared = HistoryManager::clear_stale_tool_results(&mut messages, 100, 1_000, 0.6, 2);
        assert_eq!(cleared, 0);
        assert_eq!(messages[0].content.as_deref(), Some("result"));
    }

    #[test]
    fn test_clear_stale_tool_results_idempotent() {
        let mut messages = vec![
            make_tool_msg("c1", "old"),
            make_tool_msg("c2", "old"),
            make_tool_msg("c3", "old"),
            make_tool_msg("c4", "recent"),
        ];
        let first = HistoryManager::clear_stale_tool_results(&mut messages, 900, 1_000, 0.6, 1);
        let second = HistoryManager::clear_stale_tool_results(&mut messages, 900, 1_000, 0.6, 1);
        assert_eq!(first, 3);
        assert_eq!(second, 0, "already-cleared messages are not double-counted");
    }

    // --- Observation Store tests ---

    fn large_tool_history() -> Vec<ChatMessage> {
        let big_a = "a".repeat(3000);
        let big_b = "b".repeat(3000);
        vec![
            make_msg("system", "sys"),
            make_msg("user", "task"),
            make_assistant_with_tool_calls("call-a", "read a"),
            make_tool_msg("call-a", &big_a),
            make_assistant_with_tool_calls("call-b", "read b"),
            make_tool_msg("call-b", &big_b),
            make_msg("assistant", "recent answer"),
            make_msg("user", "follow up"),
        ]
    }

    #[test]
    fn test_unseen_result_never_offloaded_until_seen() {
        let mut mgr = test_manager(large_tool_history());
        // Simulate canonical insertion: both tool results are unseen.
        mgr.unseen_tool_results.insert("call-a".into());
        mgr.unseen_tool_results.insert("call-b".into());
        let report = mgr.offload_stale_tool_results(10_000, 1_000, 0.6, 0);
        assert_eq!(report.recoverable_offloads, 0);
        assert_eq!(report.fallback_elisions, 0);
        assert!(
            report.skipped_unseen >= 2,
            "all unseen candidates protected: {report:?}"
        );
        // Both remain inline.
        assert!(
            mgr.as_slice()
                .iter()
                .any(|m| m.tool_call_id.as_deref() == Some("call-a")
                    && m.content.as_deref().is_some_and(|c| c.contains('a')))
        );
        // After the model sees them, offload becomes possible.
        mgr.mark_sent_tool_results_seen();
        let report = mgr.offload_stale_tool_results(10_000, 1_000, 0.6, 0);
        assert!(report.recoverable_offloads >= 1);
        assert!(report.reclaimed_bytes > 0);
    }

    #[test]
    fn test_parallel_batch_never_loses_unseen_early_result() {
        // 10+ parallel tool results before any successful provider response.
        let mut msgs = vec![make_msg("system", "sys"), make_msg("user", "batch")];
        let mut call_ids = Vec::new();
        // One assistant message with many tool calls.
        let tcs: Vec<crate::llm::types::ToolCall> = (0..12)
            .map(|i| crate::llm::types::ToolCall {
                id: Some(format!("call-{i}")),
                r#type: "function".into(),
                function: crate::llm::types::ToolCallFunction {
                    name: "fs_read".into(),
                    arguments: format!("{{\"n\":{i}}}"),
                },
            })
            .collect();
        msgs.push(ChatMessage {
            provider_state: None,
            role: "assistant".into(),
            content: Some("batch".into()),
            tool_calls: tcs,
            tool_call_id: None,
        });
        for i in 0..12 {
            let id = format!("call-{i}");
            call_ids.push(id.clone());
            msgs.push(make_tool_msg(&id, &"x".repeat(2000)));
        }
        let mut mgr = test_manager(msgs);
        for id in &call_ids {
            mgr.unseen_tool_results.insert(id.clone());
        }
        let report = mgr.offload_stale_tool_results(10_000, 1_000, 0.6, 3);
        assert_eq!(
            report.recoverable_offloads, 0,
            "unseen batch must not offload"
        );
        assert_eq!(report.fallback_elisions, 0);
        // 12 unseen, 3 protected by keep_recent window: 9 attempted + skipped.
        assert_eq!(report.skipped_unseen, 9, "report: {report:?}");
        // None lost merely for falling outside the recent window.
        for id in &call_ids {
            let content = mgr
                .as_slice()
                .iter()
                .find(|m| m.tool_call_id.as_deref() == Some(id.as_str()))
                .and_then(|m| m.content.clone())
                .expect("result stays inline");
            assert!(!crate::llm::observation::is_observation_stub(&content));
        }
        mgr.mark_sent_tool_results_seen();
        let report = mgr.offload_stale_tool_results(10_000, 1_000, 0.6, 3);
        assert!(report.recoverable_offloads > 0);
    }

    #[test]
    fn test_superseded_result_becomes_recoverable() {
        // Same exact call twice: first result superseded, second stays.
        let big_a = "A".repeat(2500);
        let big_b = "B".repeat(2500);
        let mk_assistant = |id: &str| ChatMessage {
            provider_state: None,
            role: "assistant".into(),
            content: Some("read".into()),
            tool_calls: vec![crate::llm::types::ToolCall {
                id: Some(id.to_string()),
                r#type: "function".into(),
                function: crate::llm::types::ToolCallFunction {
                    name: "fs_read".into(),
                    arguments: "{\"path\":\"/x\"}".into(),
                },
            }],
            tool_call_id: None,
        };
        let mut mgr = test_manager(vec![
            make_msg("system", "sys"),
            make_msg("user", "u"),
            mk_assistant("call-a"),
            make_tool_msg("call-a", &big_a),
            mk_assistant("call-b"),
            make_tool_msg("call-b", &big_b),
            make_msg("assistant", "done"),
        ]);
        mgr.mark_sent_tool_results_seen();
        let report = mgr.offload_stale_tool_results(10_000, 1_000, 0.6, 10);
        // Superseded pass should offload call-a even though it is "recent".
        assert!(report.recoverable_offloads >= 1);
        let stub = mgr
            .as_slice()
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("call-a"))
            .and_then(|m| m.content.clone())
            .expect("call-a stub");
        assert!(is_observation_stub(&stub));
        let id = stub_observation_id(&stub).expect("obs id");
        let stored = mgr.observations_snapshot();
        assert_eq!(stored.get(&id).unwrap().content, big_a);
        // call-b keeps its own content (not pointed at A).
        let b = mgr
            .as_slice()
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("call-b"))
            .and_then(|m| m.content.clone())
            .unwrap();
        assert!(b.contains('B'));
    }

    #[test]
    fn test_observation_read_result_does_not_recurse() {
        let mk_obs_assistant = |id: &str| ChatMessage {
            provider_state: None,
            role: "assistant".into(),
            content: Some("r".into()),
            tool_calls: vec![crate::llm::types::ToolCall {
                id: Some(id.to_string()),
                r#type: "function".into(),
                function: crate::llm::types::ToolCallFunction {
                    name: OBSERVATION_READ_TOOL_NAME.into(),
                    arguments: "{\"id\":\"obs-000001\"}".into(),
                },
            }],
            tool_call_id: None,
        };
        let mut mgr = test_manager(vec![
            make_msg("system", "sys"),
            make_msg("user", "u"),
            mk_obs_assistant("call-obs"),
            make_tool_msg("call-obs", &"y".repeat(3000)),
            make_msg("assistant", "done"),
        ]);
        mgr.mark_sent_tool_results_seen();
        let report = mgr.offload_stale_tool_results(10_000, 1_000, 0.6, 0);
        // observation_read output uses fallback, never a new observation.
        assert_eq!(report.recoverable_offloads, 0);
        assert_eq!(mgr.observations_snapshot().len(), 0);
    }

    #[test]
    fn test_store_capacity_fallback_without_eviction() {
        let mut mgr = test_manager(vec![make_msg("system", "sys")]);
        // Fill the store to its entry limit with tiny direct inserts.
        {
            let mut store = mgr.observations.write().unwrap();
            for i in 0..crate::llm::observation::MAX_OBSERVATION_ENTRIES {
                let id = store.insert(format!("fill-{i}"), "fs_read".into(), "z".repeat(500), 600);
                assert!(id.is_some());
            }
        }
        let before_ids = mgr.observations_snapshot().ids();
        // Now offload one more large result: must fall back, not evict.
        let big = "Q".repeat(3000);
        mgr.push(ChatMessage {
            provider_state: None,
            role: "assistant".into(),
            content: Some("r".into()),
            tool_calls: vec![crate::llm::types::ToolCall {
                id: Some("call-new".into()),
                r#type: "function".into(),
                function: crate::llm::types::ToolCallFunction {
                    name: "fs_read".into(),
                    arguments: "{}".into(),
                },
            }],
            tool_call_id: None,
        });
        mgr.push_tool_result(Some("call-new".into()), big.clone());
        mgr.mark_sent_tool_results_seen();
        let report = mgr.offload_stale_tool_results(10_000, 1_000, 0.6, 0);
        assert_eq!(report.fallback_elisions, 1);
        assert_eq!(mgr.observations_snapshot().ids(), before_ids);
    }

    #[test]
    fn test_footprint_context_reduction() {
        let mut mgr = test_manager(large_tool_history());
        mgr.mark_sent_tool_results_seen();
        let before_tools = mgr.tool_result_json_bytes();
        let before_all = mgr.messages_json_bytes();
        let report = mgr.offload_stale_tool_results(10_000, 1_000, 0.6, 0);
        assert!(report.reclaimed_bytes > 0);
        let after_tools = mgr.tool_result_json_bytes();
        let after_all = mgr.messages_json_bytes();
        assert!(after_tools < before_tools);
        assert!(after_all < before_all);
        let fp = mgr.observation_footprint();
        assert!(fp.active_references > 0);
        assert!(fp.active_reclaimed_json_bytes > 0);
        // Deterministic: reclaimed equals measured message-byte delta
        // attributable to stubs (within envelope overhead of the JSON array).
        assert_eq!(
            fp.active_reclaimed_json_bytes,
            fp.active_original_message_bytes
                .saturating_sub(fp.active_stub_message_bytes)
        );
        // Never claim tokens.
        assert_eq!(before_all.saturating_sub(after_all), report.reclaimed_bytes);
    }

    #[test]
    fn test_failed_provider_request_keeps_unseen_inline() {
        // push_tool_result marks unseen; a failed request never calls
        // mark_sent_tool_results_seen, so compaction must skip it.
        let mut mgr = test_manager(vec![make_msg("system", "sys"), make_msg("user", "u")]);
        mgr.push(ChatMessage {
            provider_state: None,
            role: "assistant".into(),
            content: Some("read".into()),
            tool_calls: vec![crate::llm::types::ToolCall {
                id: Some("call-fail".into()),
                r#type: "function".into(),
                function: crate::llm::types::ToolCallFunction {
                    name: "fs_read".into(),
                    arguments: "{}".into(),
                },
            }],
            tool_call_id: None,
        });
        mgr.push_tool_result(Some("call-fail".into()), "x".repeat(3000));
        assert_eq!(mgr.unseen_count(), 1);
        // Simulate failed provider request: no mark_seen call.
        let report = mgr.offload_stale_tool_results(10_000, 1_000, 0.6, 0);
        assert_eq!(report.recoverable_offloads, 0);
        let content = mgr
            .as_slice()
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("call-fail"))
            .and_then(|m| m.content.clone())
            .unwrap();
        assert!(content.contains('x'), "failed request keeps full result");
        // Retry succeeds: now it may offload.
        mgr.mark_sent_tool_results_seen();
        let report = mgr.offload_stale_tool_results(10_000, 1_000, 0.6, 0);
        assert!(report.recoverable_offloads >= 1);
    }

    #[test]
    fn test_push_tool_result_marks_unseen_and_preserves_shape() {
        let mut mgr = test_manager(vec![]);
        mgr.push_tool_result(Some("call-1".into()), "hello".into());
        assert_eq!(mgr.unseen_count(), 1);
        let msg = mgr.as_slice().last().expect("tool msg");
        assert_eq!(msg.role, "tool");
        assert_eq!(msg.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(msg.content.as_deref(), Some("hello"));
        mgr.mark_sent_tool_results_seen();
        assert_eq!(mgr.unseen_count(), 0);
    }

    #[test]
    fn test_rewind_keeps_old_observation_retrievable() {
        // Old completed history offloaded, then a new turn is rewound
        // (new messages removed). The observation must survive.
        let mut mgr = test_manager(large_tool_history());
        mgr.mark_sent_tool_results_seen();
        let report = mgr.offload_stale_tool_results(10_000, 1_000, 0.6, 0);
        assert!(report.recoverable_offloads > 0);
        let before = mgr.messages_json_bytes();
        // Mark turn start: record current len, append new turn messages.
        let turn_start = mgr.len();
        mgr.push(make_msg("user", "new turn question"));
        mgr.push(make_msg("assistant", "new turn answer"));
        assert!(mgr.len() > turn_start);
        // Rewind: remove new turn messages only (never restores old content).
        let mut msgs: Vec<ChatMessage> = mgr.as_slice().to_vec();
        msgs.truncate(turn_start);
        let stored = mgr.observations_snapshot();
        let unseen = mgr.unseen_snapshot();
        let mut rewound = test_manager(msgs);
        rewound.restore_observations(stored, unseen);
        // Old observation remains retrievable with exact content.
        let fp = rewound.observation_footprint();
        assert!(fp.active_references > 0);
        assert!(rewound.messages_json_bytes() <= before + 1000);
        for msg in rewound.as_slice() {
            if msg.role == "tool"
                && let Some(c) = &msg.content
                && crate::llm::observation::is_observation_stub(c)
            {
                let id = crate::llm::observation::stub_observation_id(c).unwrap();
                let obs = rewound.observations_snapshot();
                assert!(obs.contains(&id));
            }
        }
    }

    #[test]
    fn test_session_round_trip_preserves_observations() {
        let mut mgr = test_manager(large_tool_history());
        mgr.mark_sent_tool_results_seen();
        let report = mgr.offload_stale_tool_results(10_000, 1_000, 0.6, 0);
        assert!(report.recoverable_offloads > 0);
        let (messages, store, unseen) = mgr.persistable();
        // Serialize manager through the same value path used by session save
        // (SessionData serde), then restore and verify retrieval.
        let mut session = crate::session::SessionData::new();
        session.conversation = messages
            .iter()
            .map(|m| {
                serde_json::to_value(m)
                    .unwrap()
                    .as_object()
                    .unwrap()
                    .clone()
                    .into_iter()
                    .collect()
            })
            .collect();
        session.observations = store.clone();
        session.unseen_tool_results = unseen.clone();
        let raw = serde_json::to_value(&session).expect("serialize session");
        let restored: crate::session::SessionData =
            serde_json::from_value(raw).expect("deserialize session");
        // New observation ids must not collide after restore.
        let mut restored_store = restored.observations.clone();
        let before_next = restored_store.next_id();
        let new_id = restored_store
            .insert("call-new".into(), "fs_read".into(), "z".repeat(500), 600)
            .expect("new id after restore");
        assert_ne!(new_id, "");
        assert!(restored_store.next_id() > before_next);
        // Old observation still retrievable.
        let first_id = store.ids().into_iter().next().expect("one obs");
        let original = store.get(&first_id).unwrap().content.clone();
        assert_eq!(
            restored.observations.get(&first_id).unwrap().content,
            original
        );
        // Legacy fixture without observation fields loads empty.
        let legacy = serde_json::json!({
            "meta": {"id": "sess-1", "created_at": "2026-01-01T00:00:00+00:00", "title": "t", "title_is_default": true},
            "timestamp": "2026-01-01T00:00:00+00:00",
            "conversation": [],
            "token_count": 0,
            "requests": 0,
            "tool_calls": 0,
            "lines_edited": 0,
            "tool_call_successes": {},
            "tool_call_failures": {},
            "changed_files": []
        });
        let legacy_data: crate::session::SessionData =
            serde_json::from_value(legacy).expect("legacy loads");
        assert!(legacy_data.observations.is_empty());
        assert!(legacy_data.unseen_tool_results.is_empty());
    }

    // --- Preflight governor / unseen-safe compaction tests ---

    fn parallel_history() -> Vec<ChatMessage> {
        let tcs: Vec<crate::llm::types::ToolCall> = (0..3)
            .map(|i| crate::llm::types::ToolCall {
                id: Some(format!("call-{i}")),
                r#type: "function".into(),
                function: crate::llm::types::ToolCallFunction {
                    name: "fs_read".into(),
                    arguments: format!("{{\"n\":{i}}}"),
                },
            })
            .collect();
        let mut msgs = vec![make_msg("system", "sys"), make_msg("user", "batch")];
        msgs.push(ChatMessage {
            provider_state: None,
            role: "assistant".into(),
            content: Some("batch".into()),
            tool_calls: tcs,
            tool_call_id: None,
        });
        for i in 0..3 {
            msgs.push(make_tool_msg(&format!("call-{i}"), &"x".repeat(2000)));
        }
        msgs
    }

    #[test]
    fn test_protected_suffix_starts_at_oldest_unseen_assistant() {
        let mut mgr = test_manager(large_tool_history());
        // large_tool_history has call-a then call-b; mark only call-b unseen.
        mgr.unseen_tool_results.insert("call-b".into());
        let start = mgr
            .protected_suffix_start_for_unseen()
            .expect("protects call-b");
        assert_eq!(mgr.as_slice()[start].role, "assistant");
        assert!(
            mgr.as_slice()[start]
                .tool_calls
                .iter()
                .any(|tc| tc.id.as_deref() == Some("call-b"))
        );
    }

    #[test]
    fn test_parallel_batch_protects_whole_unit() {
        let mut mgr = test_manager(parallel_history());
        // Only the middle call unseen: whole assistant(A,B,C) batch protected.
        mgr.unseen_tool_results.insert("call-1".into());
        let start = mgr
            .protected_suffix_start_for_unseen()
            .expect("protects batch");
        assert_eq!(mgr.as_slice()[start].role, "assistant");
        assert_eq!(mgr.as_slice()[start].tool_calls.len(), 3);
        // Merge helper keeps the entire batch exact after the summary.
        let prefix = mgr.as_slice()[..start].to_vec();
        let suffix = mgr.as_slice()[start..].to_vec();
        let merged = HistoryManager::merge_compacted_with_protected_suffix(
            &prefix,
            &suffix,
            make_msg("user", "Summary"),
        );
        let tail: Vec<_> = merged.iter().rev().take(suffix.len()).collect();
        assert_eq!(tail.len(), suffix.len());
        for (a, b) in merged[merged.len() - suffix.len()..]
            .iter()
            .zip(suffix.iter())
        {
            assert_eq!(a.role, b.role);
            assert_eq!(a.content, b.content);
            assert_eq!(a.tool_call_id, b.tool_call_id);
        }
    }

    #[test]
    fn test_unseen_compaction_keeps_exact_content() {
        let big = "UNSEEN-EXACT-".repeat(300);
        let mut mgr = test_manager(vec![
            make_msg("system", "sys"),
            make_msg("user", "old task"),
            make_msg("assistant", "old answer with context"),
            make_msg("user", "more history for prefix value"),
            make_assistant_with_tool_calls("call-a", "reading"),
            make_tool_msg("call-a", &big),
        ]);
        mgr.unseen_tool_results.insert("call-a".into());
        let start = mgr
            .protected_suffix_start_for_unseen()
            .expect("protects unseen");
        let prefix = mgr.as_slice()[..start].to_vec();
        let suffix = mgr.as_slice()[start..].to_vec();
        // Compactor only sees the prefix.
        assert!(
            !prefix
                .iter()
                .any(|m| m.tool_call_id.as_deref().is_some_and(|id| id == "call-a"))
        );
        let merged = HistoryManager::merge_compacted_with_protected_suffix(
            &prefix,
            &suffix,
            make_msg("user", "Summary"),
        );
        let kept = merged
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("call-a"))
            .and_then(|m| m.content.clone())
            .expect("unseen stays inline");
        assert_eq!(kept, big, "unseen body must be byte-identical");
        // Pairing preserved: assistant invocation directly precedes result.
        let pos = merged
            .iter()
            .position(|m| m.tool_call_id.as_deref() == Some("call-a"))
            .expect("tool pos");
        assert!(pos > 0);
        let prev = &merged[pos - 1];
        assert_eq!(prev.role, "assistant");
        assert!(
            prev.tool_calls
                .iter()
                .any(|tc| tc.id.as_deref() == Some("call-a"))
        );
    }

    #[test]
    fn test_offload_for_pressure_is_recoverable_and_skips_unseen() {
        let mut mgr = test_manager(large_tool_history());
        mgr.mark_sent_tool_results_seen();
        // Make call-a unseen again (e.g. new result arrived after success).
        mgr.push_tool_result(Some("call-c".into()), "c".repeat(3000));
        assert!(mgr.unseen_count() >= 1);
        let before = mgr.messages_json_bytes();
        let report = mgr.offload_stale_tool_results_for_pressure(0);
        assert!(report.recoverable_offloads >= 1);
        assert!(report.skipped_unseen >= 1, "unseen protected: {report:?}");
        assert!(report.reclaimed_bytes > 0);
        assert!(mgr.messages_json_bytes() < before);
        // Recoverable: stub resolves to exact original via the store.
        for m in mgr.as_slice() {
            if m.role == "tool"
                && let Some(c) = &m.content
                && is_observation_stub(c)
            {
                let id = stub_observation_id(c).expect("obs id");
                let obs = mgr.observations_snapshot();
                let stored = obs.get(&id).expect("resolvable");
                assert!(stored.content.len() >= 200);
            }
        }
        // Unseen content stays exact inline.
        let unseen_content = mgr
            .as_slice()
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("call-c"))
            .and_then(|m| m.content.clone())
            .expect("unseen inline");
        assert!(unseen_content.contains('c'));
        assert!(!is_observation_stub(&unseen_content));
    }

    #[tokio::test]
    async fn test_missing_assistant_call_fails_closed() {
        let mut mgr = test_manager(vec![make_msg("system", "sys"), make_msg("user", "u")]);
        mgr.unseen_tool_results.insert("call_missing".into());
        assert!(mgr.protected_suffix_start_for_unseen().is_none());
        // Must not panic and must not compact.
        let compacted = mgr
            .compact_for_budget_pressure()
            .await
            .expect("fail closed, not error");
        assert!(!compacted);
        assert_eq!(mgr.len(), 2);
    }

    #[tokio::test]
    async fn test_prefix_too_small_refuses_compaction() {
        // system + user + assistant(tool) + huge unseen: prefix is only
        // [system, user], no summarization value.
        let big = "Z".repeat(20_000);
        let mut mgr = test_manager(vec![
            make_msg("system", "sys"),
            make_msg("user", "u"),
            make_assistant_with_tool_calls("call-huge", "read"),
            make_tool_msg("call-huge", &big),
        ]);
        mgr.unseen_tool_results.insert("call-huge".into());
        let compacted = mgr.compact_for_budget_pressure().await.expect("no error");
        assert!(!compacted, "tiny prefix must not compact");
        let kept = mgr
            .as_slice()
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("call-huge"))
            .and_then(|m| m.content.clone())
            .unwrap();
        assert_eq!(kept, big);
    }

    #[test]
    fn test_observation_plus_tool_growth_offload_lowers_footprint() {
        // Representative E2E: large seen results + newly activated schemas.
        // Recoverable offload alone must reduce the footprint without
        // touching the unseen batch.
        use crate::llm::context_budget::ContextBudgetGovernor;
        let mut mgr = test_manager(large_tool_history());
        mgr.mark_sent_tool_results_seen();
        mgr.push(make_assistant_with_tool_calls("call-new", "read"));
        mgr.push_tool_result(Some("call-new".into()), "n".repeat(5000));
        // call-new is unseen; call-a/call-b are seen and offloadable.
        let gov = ContextBudgetGovernor::new(crate::config::ContextBudgetConfig::default());
        let tools_before = vec![make_msg("user", "x")]; // placeholder for schema count proxy
        let _ = tools_before;
        let before = mgr.messages_json_bytes();
        let report = mgr.offload_stale_tool_results_for_pressure(1);
        assert!(report.recoverable_offloads >= 1);
        let after = mgr.messages_json_bytes();
        assert!(after < before, "offload must lower footprint");
        let unseen = mgr
            .as_slice()
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("call-new"))
            .and_then(|m| m.content.clone())
            .unwrap();
        assert!(unseen.contains('n'));
        let _ = gov;
    }

    // --- Observation GC reachability tests ---

    fn store_with_one(content: &str) -> (ObservationStore, String) {
        let mut store = ObservationStore::new();
        let id = store
            .insert("call-1".into(), "fs_read".into(), content.into(), 100)
            .expect("insert");
        (store, id)
    }

    fn manager_with_store(messages: Vec<ChatMessage>, store: ObservationStore) -> HistoryManager {
        let mgr = test_manager(messages);
        // Install without triggering restore GC yet; tests control GC timing
        // via direct lock install for pure marking checks.
        if let Ok(mut guard) = mgr.observations.write() {
            *guard = store;
        }
        mgr
    }

    #[test]
    fn gc_keeps_stub_reference() {
        let (store, id) = store_with_one(&"x".repeat(500));
        let stub = crate::llm::observation::observation_stub("fs_read", 500, &id);
        let mgr = manager_with_store(vec![make_tool_msg("call-1", &stub)], store);
        let live = mgr.collect_live_observation_ids();
        assert!(live.contains(&id), "tool stub must keep observation live");
    }

    #[test]
    fn gc_keeps_assistant_summary_reference() {
        let (store, id) = store_with_one(&"y".repeat(500));
        let mgr = manager_with_store(
            vec![make_msg(
                "assistant",
                &format!("Important evidence remains in {id}"),
            )],
            store,
        );
        let live = mgr.collect_live_observation_ids();
        assert!(live.contains(&id), "assistant text ref must keep live");
    }

    #[test]
    fn gc_keeps_user_reference() {
        let (store, id) = store_with_one(&"z".repeat(500));
        let mgr = manager_with_store(
            vec![make_msg("user", &format!("Please inspect {id} again"))],
            store,
        );
        let live = mgr.collect_live_observation_ids();
        assert!(live.contains(&id), "user text ref must keep live");
    }

    #[test]
    fn gc_keeps_tool_argument_reference() {
        let (store, id) = store_with_one(&"w".repeat(500));
        let args = format!("{{\"id\":\"{id}\",\"offset\":6000}}");
        let msg = ChatMessage {
            provider_state: None,
            role: "assistant".into(),
            content: Some("read".into()),
            tool_calls: vec![crate::llm::types::ToolCall {
                id: Some("call-read".into()),
                r#type: "function".into(),
                function: crate::llm::types::ToolCallFunction {
                    name: "observation_read".into(),
                    arguments: args,
                },
            }],
            tool_call_id: None,
        };
        let mgr = manager_with_store(vec![msg], store);
        let live = mgr.collect_live_observation_ids();
        assert!(
            live.contains(&id),
            "observation_read args must keep pagination source live"
        );
    }

    #[test]
    fn gc_keeps_provider_state_reference() {
        let (store, id) = store_with_one(&"v".repeat(500));
        let output = vec![serde_json::json!({
            "type": "reasoning",
            "summary": [{"text": format!("evidence {id} here")}]
        })];
        let msg = ChatMessage {
            provider_state: Some(crate::features::openai_subscription::ProviderState {
                version: 1,
                account: "test".into(),
                model: "m".into(),
                output,
            }),
            role: "assistant".into(),
            content: Some("done".into()),
            tool_calls: vec![],
            tool_call_id: None,
        };
        let mgr = manager_with_store(vec![msg], store);
        let live = mgr.collect_live_observation_ids();
        assert!(live.contains(&id), "provider_state string must keep live");
    }

    #[test]
    fn gc_ignores_unknown_id() {
        let (store, _) = store_with_one(&"u".repeat(500));
        let mgr = manager_with_store(
            vec![make_msg("user", "Please inspect obs-999999 again")],
            store,
        );
        let live = mgr.collect_live_observation_ids();
        assert!(
            !live.contains("obs-999999"),
            "unknown ids must never enter the live set"
        );
        assert!(live.is_empty());
    }

    #[test]
    fn gc_does_not_mark_through_observation_content() {
        let mut store = ObservationStore::new();
        let dead = store
            .insert("c-dead".into(), "fs_read".into(), "dead-body".into(), 100)
            .expect("dead");
        let holder = store
            .insert(
                "c-holder".into(),
                "fs_read".into(),
                format!("See {dead}"),
                100,
            )
            .expect("holder");
        // Canonical history references only the holder stub; the dead id
        // appears solely inside another observation's content.
        let stub_holder = crate::llm::observation::observation_stub("fs_read", 100, &holder);
        let mut mgr = manager_with_store(vec![make_tool_msg("c-holder", &stub_holder)], store);
        let report = mgr.gc_unreferenced_observations();
        assert_eq!(report.removed_entries, 1);
        assert!(mgr.observations_snapshot().contains(&holder));
        assert!(!mgr.observations_snapshot().contains(&dead));
    }

    #[test]
    fn gc_compaction_shaped_history_removes_dropped_stub() {
        // Old history has stubs for A and B; after compaction only B's stub
        // survives in the recent tail.
        let mut store = ObservationStore::new();
        let obs_a = store
            .insert("call-a".into(), "fs_read".into(), "A".repeat(500), 600)
            .expect("A");
        let obs_b = store
            .insert("call-b".into(), "fs_read".into(), "B".repeat(500), 600)
            .expect("B");
        let stub_a = crate::llm::observation::observation_stub("fs_read", 500, &obs_a);
        let stub_b = crate::llm::observation::observation_stub("fs_read", 500, &obs_b);
        let old_history = vec![
            make_msg("user", "task"),
            make_assistant_with_tool_calls("call-a", "read a"),
            make_tool_msg("call-a", &stub_a),
            make_assistant_with_tool_calls("call-b", "read b"),
            make_tool_msg("call-b", &stub_b),
        ];
        let mut mgr = manager_with_store(old_history, store);
        // Simulate post-compaction canonical history: summary + tail with B.
        let compacted_tail = vec![
            make_msg("user", "Summary of old work"),
            make_assistant_with_tool_calls("call-b", "read b"),
            make_tool_msg("call-b", &stub_b),
        ];
        mgr.messages = compacted_tail;
        let report = mgr.gc_unreferenced_observations();
        assert_eq!(report.removed_entries, 1, "report: {report:?}");
        let snap = mgr.observations_snapshot();
        assert!(!snap.contains(&obs_a), "dropped stub must be collected");
        assert!(snap.contains(&obs_b), "tail stub must survive");
        // next_id keeps moving forward.
        assert!(snap.next_id() >= 2);
    }

    #[test]
    fn gc_keeps_observation_referenced_only_by_summary() {
        let mut store = ObservationStore::new();
        let obs_a = store
            .insert("call-a".into(), "fs_read".into(), "A".repeat(500), 600)
            .expect("A");
        // Recent tail has no stub, but the summary text keeps the id.
        let mut mgr = manager_with_store(
            vec![
                make_msg(
                    "user",
                    &format!("Summary: use {obs_a} if the exact prior output is required."),
                ),
                make_msg("user", "recent question"),
            ],
            store,
        );
        let report = mgr.gc_unreferenced_observations();
        assert_eq!(report.removed_entries, 0);
        assert!(mgr.observations_snapshot().contains(&obs_a));
    }

    #[test]
    fn gc_restore_heals_legacy_dead_entries() {
        let mut store = ObservationStore::new();
        let live = store
            .insert("call-live".into(), "fs_read".into(), "L".repeat(500), 600)
            .expect("live");
        let dead = store
            .insert("call-dead".into(), "fs_read".into(), "D".repeat(500), 600)
            .expect("dead");
        let stub_live = crate::llm::observation::observation_stub("fs_read", 500, &live);
        let mut mgr = test_manager(vec![
            make_msg("user", "task"),
            make_tool_msg("call-live", &stub_live),
        ]);
        mgr.restore_observations(store, BTreeSet::new());
        let snap = mgr.observations_snapshot();
        assert!(snap.contains(&live));
        assert!(!snap.contains(&dead), "restore must GC dead entries");
    }

    #[test]
    fn gc_with_observations_path_shares_restore_contract() {
        let mut store = ObservationStore::new();
        let live = store
            .insert("call-live".into(), "fs_read".into(), "L".repeat(500), 600)
            .expect("live");
        let dead = store
            .insert("call-dead".into(), "fs_read".into(), "D".repeat(500), 600)
            .expect("dead");
        let stub_live = crate::llm::observation::observation_stub("fs_read", 500, &live);
        let messages = vec![
            make_msg("user", "task"),
            make_tool_msg("call-live", &stub_live),
        ];
        let dir = tempfile::tempdir().expect("tempdir");
        let config = crate::config::AppConfig {
            project_root: dir.path().to_path_buf(),
            ..Default::default()
        };
        let fs = crate::tools::FsTools::new(
            std::sync::Arc::new(tokio::sync::RwLock::new(None)),
            std::sync::Arc::new(config.clone()),
        );
        let client =
            crate::llm::client_core::OpenAIClient::new("http://127.0.0.1:1", "k").expect("client");
        let mgr = HistoryManager::with_observations(
            client,
            messages,
            None,
            fs,
            config,
            store,
            BTreeSet::new(),
        );
        let snap = mgr.observations_snapshot();
        assert!(snap.contains(&live));
        assert!(!snap.contains(&dead));
    }

    #[test]
    fn gc_never_touches_unseen_tool_results() {
        let (store, id) = store_with_one(&"q".repeat(500));
        let stub = crate::llm::observation::observation_stub("fs_read", 500, &id);
        let mut mgr = manager_with_store(
            vec![make_msg("user", "task"), make_tool_msg("call-1", &stub)],
            store,
        );
        mgr.unseen_tool_results.insert("call-pending".into());
        let unseen_before = mgr.unseen_snapshot();
        let report = mgr.gc_unreferenced_observations();
        assert_eq!(report.removed_entries, 0);
        assert_eq!(mgr.unseen_snapshot(), unseen_before);
        // Even when GC removes something, unseen stays intact.
        mgr.messages = vec![make_msg("user", "fresh")];
        let report = mgr.gc_unreferenced_observations();
        assert_eq!(report.removed_entries, 1);
        assert_eq!(mgr.unseen_snapshot(), unseen_before);
    }

    #[test]
    fn gc_live_entry_paged_read_stays_lossless() {
        let content = "日本語🎉".repeat(300);
        let (store, id) = store_with_one(&content);
        let stub = crate::llm::observation::observation_stub("fs_read", content.len(), &id);
        let mut mgr = manager_with_store(vec![make_tool_msg("call-1", &stub)], store);
        let report = mgr.gc_unreferenced_observations();
        assert_eq!(report.removed_entries, 0);
        let snap = mgr.observations_snapshot();
        let mut assembled = String::new();
        let mut offset = 0usize;
        loop {
            let page = snap.read_paged(&id, offset, 37).expect("page");
            assembled.push_str(&page.page);
            match page.next_cursor {
                Some(next) => offset = next,
                None => break,
            }
        }
        assert_eq!(assembled, content);
    }

    #[test]
    fn gc_clear_resets_messages_observations_and_unseen() {
        let (store, id) = store_with_one(&"c".repeat(500));
        let stub = crate::llm::observation::observation_stub("fs_read", 500, &id);
        let mut mgr = manager_with_store(vec![make_tool_msg("call-1", &stub)], store);
        mgr.unseen_tool_results.insert("call-pending".into());
        mgr.clear();
        assert!(mgr.is_empty());
        assert!(mgr.observations_snapshot().is_empty());
        assert_eq!(mgr.unseen_count(), 0);
    }
}
