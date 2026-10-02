//! Recoverable Tool Result Offloading / Observation Store.
//!
//! Large historical tool results that [`crate::llm::tool_execution::history::HistoryManager`]
//! removes from active LLM context are preserved here as exact model-visible
//! strings, keyed by conversation-local opaque IDs (`obs-000001`, ...).
//!
//! The store is conversation-owned (lives inside `HistoryManager` and is
//! persisted via `SessionData`); it is never global. It stores only the exact
//! final `content` string that was already shown to the model, after all
//! tool-specific limits, redaction, and generic truncation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Tool name for the read-only retrieval tool. Its own results are excluded
/// from recoverable offloading to avoid observation-of-observation chains.
pub const OBSERVATION_READ_TOOL_NAME: &str = "observation_read";

/// Maximum number of stored observations per conversation.
pub const MAX_OBSERVATION_ENTRIES: usize = 256;
/// Maximum total stored model-visible content bytes per conversation.
pub const MAX_OBSERVATION_STORE_BYTES: usize = 1_500_000;
/// Maximum bytes for a single observation. Derived from the largest
/// tool-output tier (`fs_read`/`fs_read_many_files`/`plan_read` = 40,000
/// chars) with headroom for JSON envelopes; anything larger falls back to
/// the ordinary non-recoverable stub.
pub const MAX_SINGLE_OBSERVATION_BYTES: usize = 128 * 1024;
/// Minimum tool-result chars worth offloading. Replacing a tiny result with
/// a stub can make the prompt larger.
pub const MIN_OBSERVABLE_TOOL_CHARS: usize = 200;
/// Maximum chars returned per `observation_read` call (fits below the 8,000
/// global tool-output cap after the header is added).
pub const MAX_OBSERVATION_READ_CHARS: usize = 6_000;

/// Prefix identifying an offloaded reference stub in `role=tool` content.
pub const OBSERVATION_STUB_PREFIX: &str = "(offloaded tool result:";
/// Prefix identifying a legacy non-recoverable cleared stub.
pub const CLEARED_STUB_PREFIX: &str = "[cleared tool result";

/// One preserved model-visible tool result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Observation {
    /// Conversation-local opaque id, e.g. `obs-000001`.
    pub id: String,
    /// Tool-call id of the original `role=tool` message.
    pub tool_call_id: String,
    /// Logical tool name (e.g. `fs_read`, `mcp__github__...`).
    pub tool_name: String,
    /// Exact content previously shown to the model.
    pub content: String,
    /// Byte length of `content` at offload time.
    pub original_content_bytes: usize,
    /// Serialized JSON bytes of the original tool message.
    pub original_message_json_bytes: usize,
}

/// Conversation-owned bounded store of offloaded tool results.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservationStore {
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    entries: BTreeMap<String, Observation>,
}

impl ObservationStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    /// Total stored content bytes (lifetime storage, not active savings).
    pub fn stored_content_bytes(&self) -> usize {
        self.entries.values().map(|e| e.content.len()).sum()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.entries.contains_key(id)
    }

    pub fn get(&self, id: &str) -> Option<&Observation> {
        self.entries.get(id)
    }

    pub fn ids(&self) -> BTreeSet<String> {
        self.entries.keys().cloned().collect()
    }

    /// Remove one entry by id (rollback only; never used for eviction).
    /// The monotonic counter is intentionally not decremented so ids stay
    /// unique across the conversation lifetime.
    pub fn remove(&mut self, id: &str) -> bool {
        self.entries.remove(id).is_some()
    }

    /// Allocate the next conversation-local opaque id without inserting.
    fn allocate_id(&mut self) -> String {
        self.next_id += 1;
        format!("obs-{:06}", self.next_id)
    }

    /// Peek the next id without allocating (for size pre-checks).
    pub fn peek_next_id(&self) -> String {
        format!("obs-{:06}", self.next_id + 1)
    }

    /// Attempt to store a model-visible tool result.
    ///
    /// Returns `None` when the store cannot safely retain the candidate
    /// (too large, too many entries, or store byte cap reached). Callers
    /// must then use the ordinary non-recoverable fallback stub. Never
    /// evicts still-addressable observations.
    pub fn insert(
        &mut self,
        tool_call_id: String,
        tool_name: String,
        content: String,
        original_message_json_bytes: usize,
    ) -> Option<String> {
        let content_bytes = content.len();
        if content_bytes > MAX_SINGLE_OBSERVATION_BYTES {
            return None;
        }
        if self.entries.len() >= MAX_OBSERVATION_ENTRIES {
            return None;
        }
        if self.stored_content_bytes().saturating_add(content_bytes) > MAX_OBSERVATION_STORE_BYTES {
            return None;
        }
        let id = self.allocate_id();
        let obs = Observation {
            id: id.clone(),
            tool_call_id,
            tool_name,
            original_content_bytes: content_bytes,
            original_message_json_bytes,
            content,
        };
        self.entries.insert(id.clone(), obs);
        Some(id)
    }

    /// Bounded UTF-8-safe paged read. `offset`/`limit` are byte offsets.
    /// `limit == 0` means the default page size.
    pub fn read_paged(
        &self,
        id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<PagedObservation, ObservationReadError> {
        let obs = self
            .get(id)
            .ok_or_else(|| ObservationReadError::UnknownId(id.to_string()))?;
        let total = obs.content.len();
        let limit = if limit == 0 {
            MAX_OBSERVATION_READ_CHARS
        } else {
            limit.min(MAX_OBSERVATION_READ_CHARS)
        };
        let start = ceil_char_boundary(&obs.content, offset.min(total));
        let mut end = start.saturating_add(limit).min(total);
        end = floor_char_boundary(&obs.content, end);
        // Guarantee progress: a limit smaller than the char at `start`
        // would otherwise floor back to `start`, returning an empty page
        // whose cursor never advances (caller livelock). Extend past one
        // full char instead.
        if end <= start && start < total {
            end = ceil_char_boundary(&obs.content, start + 1);
        }
        let page = obs.content[start..end].to_string();
        let next_cursor = if end < total { Some(end) } else { None };
        Ok(PagedObservation {
            id: obs.id.clone(),
            tool_name: obs.tool_name.clone(),
            total_bytes: total,
            start_byte: start,
            end_byte: end,
            page,
            next_cursor,
        })
    }
}

/// One page of an observation read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PagedObservation {
    pub id: String,
    pub tool_name: String,
    pub total_bytes: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub page: String,
    pub next_cursor: Option<usize>,
}

impl PagedObservation {
    /// Concise header; repeated reads with `next_cursor` are lossless.
    pub fn render(&self) -> String {
        let mut out = format!(
            "observation {}: bytes {}-{} of {}",
            self.id, self.start_byte, self.end_byte, self.total_bytes
        );
        if let Some(next) = self.next_cursor {
            out.push_str(&format!(
                "\n{}\n... continue with observation_read(id=\"{}\", offset={})",
                self.page, self.id, next
            ));
        } else {
            out.push_str(&format!("\n{}", self.page));
        }
        out
    }
}

/// Error reading an observation. Never falls back to re-running the tool.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ObservationReadError {
    #[error("unknown observation id: {0}")]
    UnknownId(String),
}

/// Round `offset` up to the next UTF-8 char boundary.
pub fn ceil_char_boundary(s: &str, mut offset: usize) -> usize {
    offset = offset.min(s.len());
    while offset < s.len() && !s.is_char_boundary(offset) {
        offset += 1;
    }
    offset
}

/// Round `offset` down to the previous UTF-8 char boundary.
pub fn floor_char_boundary(s: &str, mut offset: usize) -> usize {
    offset = offset.min(s.len());
    while offset > 0 && !s.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Compact deterministic reference stub replacing offloaded content.
///
/// Short, stable, includes the observation id, logical tool name, and
/// original size, and tells the model how to recover. Never copies
/// arguments, paths, or result snippets.
pub fn observation_stub(tool_name: &str, original_bytes: usize, observation_id: &str) -> String {
    format!(
        "(offloaded tool result: {tool_name}, {original_bytes} B, observation {observation_id}; use observation_read if needed)"
    )
}

/// Legacy non-recoverable fallback stub (store full or candidate unsuitable).
pub fn fallback_stub(tool_name: &str, original_bytes: usize) -> String {
    format!("[cleared tool result: {tool_name}, {original_bytes} bytes; re-run the tool if needed]")
}

/// Whether `content` is an offloaded observation reference.
pub fn is_observation_stub(content: &str) -> bool {
    content.starts_with(OBSERVATION_STUB_PREFIX)
}

/// Extract the observation id from a stub, if present (`obs-NNNNNN`).
pub fn stub_observation_id(content: &str) -> Option<String> {
    if !is_observation_stub(content) {
        return None;
    }
    // Stub shape: "(offloaded tool result: <name>, <N> B, observation obs-000001; ..."
    let marker = "observation ";
    let start = content.find(marker)? + marker.len();
    let rest = &content[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        .unwrap_or(rest.len());
    let id = &rest[..end];
    if id.starts_with("obs-") && !id.is_empty() {
        Some(id.to_string())
    } else {
        None
    }
}

/// Footprint attribution for the Observation Store.
///
/// `stored_*` is lifetime storage; `active_*` counts stubs currently in the
/// provider-bound conversation. Only active references contribute current
/// prompt savings, measured as exact deterministic JSON bytes reclaimed
/// (never estimated tokens).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservationFootprint {
    /// Schema version for the footprint report.
    pub version: u32,
    pub stored_entries: usize,
    pub stored_content_bytes: usize,
    pub active_references: usize,
    pub active_original_message_bytes: usize,
    pub active_stub_message_bytes: usize,
    pub active_reclaimed_json_bytes: usize,
}

/// Current footprint report version.
pub const OBSERVATION_FOOTPRINT_VERSION: u32 = 2;

/// Shared handle type threading conversation state into the tool seam
/// without global state.
pub type SharedObservationStore = std::sync::Arc<std::sync::RwLock<ObservationStore>>;

/// Create an empty shared handle (one per conversation).
pub fn new_shared_store() -> SharedObservationStore {
    std::sync::Arc::new(std::sync::RwLock::new(ObservationStore::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_round_trip_and_paging() {
        let mut store = ObservationStore::new();
        let content = "hello world".repeat(100);
        let id = store
            .insert("call-1".into(), "fs_read".into(), content.clone(), 1200)
            .expect("insert");
        assert_eq!(id, "obs-000001");
        let page = store.read_paged(&id, 0, 0).expect("read");
        assert!(page.page.len() <= MAX_OBSERVATION_READ_CHARS);
        // Full reassembly over pages equals original.
        let mut assembled = String::new();
        let mut offset = 0usize;
        loop {
            let p = store.read_paged(&id, offset, 100).expect("page");
            assembled.push_str(&p.page);
            match p.next_cursor {
                Some(next) => offset = next,
                None => break,
            }
        }
        assert_eq!(assembled, content);
    }

    #[test]
    fn test_unicode_paging_never_splits_codepoint() {
        let mut store = ObservationStore::new();
        let content = "日本語🎉emoji".repeat(500);
        let id = store
            .insert("call-u".into(), "fs_read".into(), content.clone(), 9999)
            .expect("insert");
        // Full reassembly through small pages must be lossless and never
        // panic on mid-codepoint offsets.
        let mut assembled = String::new();
        let mut offset = 0usize;
        loop {
            let p = store.read_paged(&id, offset, 37).expect("page");
            // Every page boundary must be a char boundary.
            assert!(content.is_char_boundary(p.start_byte));
            assert!(content.is_char_boundary(p.end_byte));
            assert!(p.page.len() <= 37 + 4, "page respects byte limit");
            assembled.push_str(&p.page);
            match p.next_cursor {
                Some(next) => {
                    assert!(next > offset, "progress");
                    offset = next;
                }
                None => break,
            }
        }
        assert_eq!(assembled, content);
        // Mid-codepoint offsets are rounded up, never panic.
        for probe in [1usize, 2, 4, 5, 7] {
            let p = store.read_paged(&id, probe, 10).expect("probe read");
            assert!(content.is_char_boundary(p.start_byte));
            assert!(content.is_char_boundary(p.end_byte));
        }
    }

    #[test]
    fn test_tiny_limit_on_multibyte_content_still_progresses() {
        // Regression: a limit smaller than the char at `start` must not
        // return an empty page whose cursor never advances (caller livelock).
        let mut store = ObservationStore::new();
        let content = "🎉".repeat(100);
        let id = store
            .insert("call-e".into(), "fs_read".into(), content.clone(), 9999)
            .expect("insert");
        let p = store.read_paged(&id, 0, 1).expect("page");
        assert!(!p.page.is_empty(), "tiny limit must still return one char");
        assert_eq!(p.page, "🎉");
        assert_eq!(p.next_cursor, Some(4));
        // Full walk with limit=1 terminates and reassembles losslessly.
        let mut assembled = String::new();
        let mut offset = 0usize;
        let mut steps = 0usize;
        loop {
            let p = store.read_paged(&id, offset, 1).expect("page");
            assembled.push_str(&p.page);
            match p.next_cursor {
                Some(next) => {
                    assert!(next > offset, "cursor must advance");
                    offset = next;
                }
                None => break,
            }
            steps += 1;
            assert!(steps <= 101, "must terminate");
        }
        assert_eq!(assembled, content);
    }

    #[test]
    fn test_store_capacity_never_evicts() {
        let mut store = ObservationStore::new();
        // Fill to entry cap with small entries.
        for i in 0..MAX_OBSERVATION_ENTRIES {
            let id = store.insert(
                format!("call-{i}"),
                "search_text".into(),
                format!("content-{i}"),
                100,
            );
            assert!(id.is_some(), "should fit until cap");
        }
        assert_eq!(store.len(), MAX_OBSERVATION_ENTRIES);
        // Next insert must fail without evicting.
        let before: Vec<String> = store.entries.keys().cloned().collect();
        assert!(
            store
                .insert("call-overflow".into(), "fs_read".into(), "x".into(), 10)
                .is_none()
        );
        let after: Vec<String> = store.entries.keys().cloned().collect();
        assert_eq!(before, after);
    }

    #[test]
    fn test_single_observation_limit() {
        let mut store = ObservationStore::new();
        let big = "x".repeat(MAX_SINGLE_OBSERVATION_BYTES + 1);
        assert!(
            store
                .insert("c".into(), "fs_read".into(), big, 10)
                .is_none()
        );
    }

    #[test]
    fn test_unknown_id_errors() {
        let store = ObservationStore::new();
        assert!(matches!(
            store.read_paged("obs-999999", 0, 10),
            Err(ObservationReadError::UnknownId(_))
        ));
    }

    #[test]
    fn test_stub_round_trip() {
        let stub = observation_stub("fs_read", 6124, "obs-000123");
        assert!(is_observation_stub(&stub));
        assert_eq!(stub_observation_id(&stub).as_deref(), Some("obs-000123"));
        assert!(stub.contains("fs_read"));
        assert!(stub.contains("6124"));
        assert!(!stub.contains("/tmp/"));
    }

    #[test]
    fn test_ids_monotonic_and_stable() {
        let mut store = ObservationStore::new();
        let a = store
            .insert("c1".into(), "a".into(), "x".into(), 1)
            .unwrap();
        let b = store
            .insert("c2".into(), "b".into(), "y".into(), 1)
            .unwrap();
        assert_eq!(a, "obs-000001");
        assert_eq!(b, "obs-000002");
    }

    #[test]
    fn test_legacy_deserialization_defaults_empty() {
        // Old payloads without observation fields must load as empty store.
        let v = serde_json::json!({});
        let store: ObservationStore = serde_json::from_value(v).unwrap();
        assert!(store.is_empty());
    }
}
