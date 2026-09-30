use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

use super::types::{
    LEGACY_PROVENANCE_SCHEMA_VERSION, PROVENANCE_SCHEMA_VERSION, PlanChangedEvent, ProvenanceEvent,
    ProvenanceEventEnvelope, from_v1_wire, from_v2_wire, to_v2_wire,
};
use super::wire::{EventHeader, v1 as wire_v1, v2 as wire_v2};

/// Durable provenance store for one session.
///
/// Layout:
/// - legacy (read-only): `<session_dir>/provenance/v1/events/<event-id>.json`
/// - current (read + write): `<session_dir>/provenance/v2/events/<event-id>.json`
///
/// v1 files are never copied, rewritten, or migrated on disk; they are
/// converted into the canonical representation on read. New events are
/// always written as v2.
#[derive(Debug, Clone)]
pub struct ProvenanceStore {
    provenance_root: PathBuf,
    current_dir: PathBuf,
}

impl ProvenanceStore {
    pub fn new(session_dir: PathBuf) -> Self {
        let provenance_root = session_dir.join("provenance");
        let current_dir = provenance_root.join("v2").join("events");
        Self {
            provenance_root,
            current_dir,
        }
    }

    /// Test/session helper: build directly from a known events directory.
    ///
    /// The given directory is treated as the current (v2) events dir; its
    /// parent structure is faked so `legacy_events_dir()` points at a
    /// sibling `v1/events` when present. Prefer [`ProvenanceStore::new`]
    /// for session paths.
    #[cfg(test)]
    pub(crate) fn from_events_dir(events_dir: PathBuf) -> Self {
        // `.../provenance/v2/events` -> provenance_root `.../provenance`.
        // Otherwise treat the parent as the provenance root for ad-hoc dirs.
        let provenance_root = events_dir
            .ancestors()
            .find(|a| a.file_name().is_some_and(|n| n == "provenance"))
            .map(Path::to_path_buf)
            .unwrap_or_else(|| events_dir.parent().unwrap_or(&events_dir).to_path_buf());
        Self {
            provenance_root,
            current_dir: events_dir,
        }
    }

    fn legacy_events_dir(&self) -> PathBuf {
        self.provenance_root.join("v1").join("events")
    }

    fn current_events_dir(&self) -> PathBuf {
        self.current_dir.clone()
    }

    /// Current v2 write directory.
    ///
    /// This is the only directory new events are written to. Reads cover
    /// both v1 (legacy) and v2; see [`ProvenanceStore::load_all`].
    pub fn events_dir(&self) -> &Path {
        &self.current_dir
    }

    /// Owned path of the current v2 events directory.
    pub fn current_events_path(&self) -> PathBuf {
        self.current_events_dir()
    }

    /// Owned path of the legacy v1 events directory (read-only).
    pub fn legacy_events_path(&self) -> PathBuf {
        self.legacy_events_dir()
    }

    /// Atomically persist one event and return its envelope.
    ///
    /// The payload is written to a sibling temp file in the v2 events
    /// directory and persisted with `persist_noclobber` (fail if the
    /// destination already exists). Never check-then-write.
    pub fn append(
        &self,
        session_id: &str,
        event: ProvenanceEvent,
    ) -> Result<ProvenanceEventEnvelope> {
        let current_dir = self.current_events_dir();
        std::fs::create_dir_all(&current_dir).with_context(|| {
            format!(
                "failed to create provenance events dir {}",
                current_dir.display()
            )
        })?;

        let mut event = event;
        let event_id = Uuid::now_v7().to_string();
        // Keep a single identity for changes: when the caller leaves
        // `transaction_id` empty, backfill it with the envelope id so
        // `observed_change_ids` (envelope ids) and `transaction_id` agree.
        if let ProvenanceEvent::ChangeCommitted(ref mut committed) = event
            && committed.transaction_id.is_empty()
        {
            committed.transaction_id = event_id.clone();
        }

        let canonical = ProvenanceEventEnvelope {
            schema_version: PROVENANCE_SCHEMA_VERSION,
            event_id: event_id.clone(),
            session_id: session_id.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            event,
        };
        let wire = to_v2_wire(&canonical);
        let payload =
            serde_json::to_string_pretty(&wire).context("failed to serialize provenance event")?;
        let dest = current_dir.join(format!("{event_id}.json"));

        persist_sibling_noclobber(&current_dir, &dest, payload.as_bytes())
            .with_context(|| format!("failed to persist provenance event {}", dest.display()))?;

        tracing::info!(
            event_id = %event_id,
            event_type = canonical.event_type().as_str(),
            session_id = %session_id,
            "provenance.event_recorded"
        );
        Ok(canonical)
    }

    /// Load all events from v1 (legacy) and v2 (current), merged and sorted
    /// by `(timestamp, event_id)`.
    ///
    /// One corrupt or future-schema file never fails the whole query; it is
    /// skipped with a warning entry. When the same event id exists in both
    /// versions (manual copy/migration mistake, not expected UUID
    /// collision), the v2 copy wins deterministically and a warning explains
    /// why.
    pub fn load_all(&self) -> Result<ProvenanceLoadResult> {
        let mut warnings = Vec::new();
        // event_id -> (envelope, from_v2)
        let mut by_id: HashMap<String, (ProvenanceEventEnvelope, bool)> = HashMap::new();
        // Track source dirs for duplicate diagnostics without cloning envelopes.
        let mut id_source: HashMap<String, &'static str> = HashMap::new();

        for (dir, is_v2) in [
            (self.legacy_events_dir(), false),
            (self.current_events_dir(), true),
        ] {
            let loaded = load_events_dir(&dir, &mut warnings);
            for env in loaded {
                let id = env.event_id.clone();
                match by_id.get(&id) {
                    None => {
                        id_source.insert(id.clone(), if is_v2 { "v2" } else { "v1" });
                        by_id.insert(id, (env, is_v2));
                    }
                    Some((_, existing_is_v2)) => {
                        // Deterministic policy: v2 wins over v1.
                        let existing_src = id_source.get(&id).copied().unwrap_or("?");
                        if is_v2 && !existing_is_v2 {
                            warnings.push(format!(
                                "Duplicate provenance event id {id} in v1 and v2; using v2 copy (v2 wins over legacy v1; check for manual copy/migration mistake)"
                            ));
                            id_source.insert(id.clone(), "v2");
                            by_id.insert(id, (env, is_v2));
                        } else if !is_v2 && *existing_is_v2 {
                            warnings.push(format!(
                                "Duplicate provenance event id {id} in {existing_src} and v1; keeping {existing_src} copy (v2 wins over legacy v1; check for manual copy/migration mistake)"
                            ));
                        } else {
                            warnings.push(format!(
                                "Duplicate provenance event id {id} within {}; keeping first copy",
                                if is_v2 { "v2" } else { "v1" }
                            ));
                        }
                    }
                }
            }
        }

        let mut events: Vec<ProvenanceEventEnvelope> =
            by_id.into_values().map(|(env, _)| env).collect();
        events.sort_by(|a, b| {
            let ord = compare_timestamps(&a.timestamp, &b.timestamp);
            if ord == std::cmp::Ordering::Equal {
                a.event_id.cmp(&b.event_id)
            } else {
                ord
            }
        });
        Ok(ProvenanceLoadResult { events, warnings })
    }

    /// Find the predecessor of a mutation: the latest v2 `ChangeCommitted`
    /// for `file` whose `after` state equals `before`.
    ///
    /// Only v2 events carry whole-file hashes, so v1 events never connect
    /// here. `load_all` order is `(timestamp, event_id)`; the latest match
    /// is returned. Separated for future indexing without changing callers.
    pub fn find_predecessor(
        events: &[ProvenanceEventEnvelope],
        file: &str,
        before: &super::types::FileStateEvidence,
    ) -> Option<String> {
        for env in events.iter().rev() {
            let ProvenanceEvent::ChangeCommitted(c) = &env.event else {
                continue;
            };
            if c.file != file {
                continue;
            }
            // v1-converted events have no whole-file hash; never guess a link.
            if c.before.content_hash.is_none() && c.after.content_hash.is_none() {
                continue;
            }
            if c.after.state_matches(before) {
                return Some(env.event_id.clone());
            }
        }
        None
    }
}

fn load_events_dir(dir: &Path, warnings: &mut Vec<String>) -> Vec<ProvenanceEventEnvelope> {
    let mut events = Vec::new();
    if !dir.exists() {
        return events;
    }
    let read_dir = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!(
                "Skipping unreadable provenance events dir {}: {e}",
                dir.display()
            ));
            return events;
        }
    };
    for entry in read_dir {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warnings.push(format!(
                    "Skipping unreadable provenance directory entry in {}: {e}",
                    dir.display()
                ));
                continue;
            }
        };
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // Skip temp files and non-JSON files.
        if name.starts_with('.') || name.starts_with(".tmp_provenance_") {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) => {
                warnings.push(format!(
                    "Skipping unreadable provenance event {}: {e}",
                    path.display()
                ));
                continue;
            }
        };
        // Version dispatch first: never deserialize directly into canonical.
        let header: EventHeader = match serde_json::from_str(&raw) {
            Ok(h) => h,
            Err(e) => {
                warnings.push(format!(
                    "Skipping malformed provenance event {}: {e}",
                    path.display()
                ));
                continue;
            }
        };
        if header.schema_version == LEGACY_PROVENANCE_SCHEMA_VERSION {
            match serde_json::from_str::<wire_v1::V1Envelope>(&raw) {
                Ok(env) => events.push(from_v1_wire(env)),
                Err(e) => warnings.push(format!(
                    "Skipping malformed provenance event {}: {e}",
                    path.display()
                )),
            }
        } else if header.schema_version == PROVENANCE_SCHEMA_VERSION {
            match serde_json::from_str::<wire_v2::V2Envelope>(&raw) {
                Ok(env) => events.push(from_v2_wire(env)),
                Err(e) => warnings.push(format!(
                    "Skipping malformed provenance event {}: {e}",
                    path.display()
                )),
            }
        } else {
            warnings.push(format!(
                "Unsupported provenance schema version {} in {}",
                header.schema_version,
                path.display()
            ));
        }
    }
    events
}

#[derive(Debug, Clone, Default)]
pub struct ProvenanceLoadResult {
    pub events: Vec<ProvenanceEventEnvelope>,
    pub warnings: Vec<String>,
}

fn compare_timestamps(a: &str, b: &str) -> std::cmp::Ordering {
    match (
        chrono::DateTime::parse_from_rfc3339(a),
        chrono::DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(a), Ok(b)) => a.cmp(&b),
        _ => a.cmp(b),
    }
}

/// Write `bytes` to a sibling temp file in `dir`, fsync it, then persist
/// without clobbering an existing destination.
///
/// This is sibling-temp + no-clobber persistence: `persist_noclobber`
/// refuses to overwrite but is not documented as atomic on all platforms,
/// so callers must not describe it as an atomic no-clobber guarantee.
fn persist_sibling_noclobber(dir: &Path, dest: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    let mut temp = tempfile::Builder::new()
        .prefix(".tmp_provenance_")
        .tempfile_in(dir)
        .context("failed to create provenance temp file")?;
    temp.write_all(bytes)
        .context("failed to write provenance temp file")?;
    temp.as_file_mut()
        .sync_all()
        .context("failed to flush provenance temp file")?;
    temp.persist_noclobber(dest).map_err(|e| {
        anyhow::anyhow!("provenance destination already exists or persist failed: {e}")
    })?;
    Ok(())
}

impl From<PlanChangedEvent> for ProvenanceEvent {
    fn from(e: PlanChangedEvent) -> Self {
        Self::PlanChanged(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::types::{
        ChangeCommittedEvent, ChangeKind, ChangeTarget, FileStateEvidence, PlanItemTransition,
        file_content_hash,
    };

    fn change_event(plan_item_id: Option<&str>) -> ProvenanceEvent {
        ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
            transaction_id: String::new(),
            plan_item_id: plan_item_id.map(str::to_string),
            change_kind: ChangeKind::SemanticEdit,
            file: "src/lib.rs".to_string(),
            target: ChangeTarget::SemanticSymbol {
                symbol_id: "sym-v1-abc".to_string(),
                before_fingerprint: "fp-v1-a".to_string(),
                after_fingerprint: "fp-v1-b".to_string(),
            },
            before: FileStateEvidence {
                exists: true,
                content_hash: Some(file_content_hash("before")),
                byte_len: Some(6),
            },
            after: FileStateEvidence {
                exists: true,
                content_hash: Some(file_content_hash("after")),
                byte_len: Some(5),
            },
            predecessor_change_id: None,
            reverts_change_id: None,
            diff: "diff".to_string(),
            diff_hash: "blake3:abc".to_string(),
            lines_added: 1,
            lines_removed: 0,
        })
    }

    fn current_dir(store: &ProvenanceStore) -> PathBuf {
        store.current_events_path()
    }

    #[test]
    fn test_events_dir_returns_current_v2_write_dir() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(dir.path().join("s"));
        assert!(
            store
                .events_dir()
                .to_string_lossy()
                .ends_with("provenance/v2/events")
        );
    }

    #[test]
    fn test_event_persist_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join("sess-1");
        let store = ProvenanceStore::new(session_dir);
        let env = store
            .append("sess-1", change_event(Some("step-2")))
            .unwrap();
        assert!(!env.event_id.is_empty());
        assert_eq!(env.schema_version, PROVENANCE_SCHEMA_VERSION);
        // Store round-trip with a fresh handle (session resume path).
        let store2 = ProvenanceStore::new(dir.path().join("sess-1"));
        let loaded = store2.load_all().unwrap();
        assert_eq!(loaded.events.len(), 1);
        assert!(loaded.warnings.is_empty());
        assert_eq!(loaded.events[0].event_id, env.event_id);
    }

    #[test]
    fn test_v2_roundtrip_preserves_hashes_and_links() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(dir.path().join("s"));
        let mut event = change_event(None);
        if let ProvenanceEvent::ChangeCommitted(ref mut c) = event {
            c.predecessor_change_id = Some("prev-1".to_string());
            c.reverts_change_id = Some("rev-1".to_string());
        }
        let env = store.append("s", event).unwrap();
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 1);
        match &loaded.events[0].event {
            ProvenanceEvent::ChangeCommitted(c) => {
                assert_eq!(c.predecessor_change_id.as_deref(), Some("prev-1"));
                assert_eq!(c.reverts_change_id.as_deref(), Some("rev-1"));
                assert!(c.before.content_hash.is_some());
            }
            _ => panic!("expected change"),
        }
        let _ = env;
    }

    #[test]
    fn test_v1_fixture_loads_as_canonical() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join("s");
        let v1_dir = session_dir.join("provenance/v1/events");
        std::fs::create_dir_all(&v1_dir).unwrap();
        // Hand-written legacy v1 JSON (frozen shape).
        let payload = serde_json::json!({
            "schema_version": 1,
            "event_id": "evt-v1-1",
            "session_id": "s",
            "timestamp": "2026-01-01T00:00:00+00:00",
            "event": {
                "type": "change_committed",
                "transaction_id": "",
                "change_kind": "semantic_edit",
                "file": "src/lib.rs",
                "symbol_id": "sym-v1-abc",
                "before_fingerprint": "fp-a",
                "after_fingerprint": "fp-b",
                "diff": "d",
                "diff_hash": "blake3:x",
                "lines_added": 1,
                "lines_removed": 0
            }
        });
        std::fs::write(
            v1_dir.join("evt-v1-1.json"),
            serde_json::to_string_pretty(&payload).unwrap(),
        )
        .unwrap();
        let store = ProvenanceStore::new(session_dir);
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 1);
        match &loaded.events[0].event {
            ProvenanceEvent::ChangeCommitted(c) => {
                assert_eq!(c.change_kind, ChangeKind::SemanticEdit);
                assert!(matches!(c.target, ChangeTarget::SemanticSymbol { .. }));
                assert_eq!(c.before.content_hash, None);
                assert_eq!(c.after.content_hash, None);
            }
            _ => panic!("expected change"),
        }
    }

    #[test]
    fn test_mixed_v1_v2_merges_and_sorts() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join("s");
        let v1_dir = session_dir.join("provenance/v1/events");
        std::fs::create_dir_all(&v1_dir).unwrap();
        let payload = serde_json::json!({
            "schema_version": 1,
            "event_id": "a-id",
            "session_id": "s",
            "timestamp": "2026-01-01T00:00:00+00:00",
            "event": {
                "type": "change_committed",
                "transaction_id": "",
                "change_kind": "semantic_edit",
                "file": "src/lib.rs",
                "symbol_id": "sym",
                "before_fingerprint": "a",
                "after_fingerprint": "b",
                "diff": "d",
                "diff_hash": "blake3:x",
                "lines_added": 1,
                "lines_removed": 0
            }
        });
        std::fs::write(
            v1_dir.join("a-id.json"),
            serde_json::to_string_pretty(&payload).unwrap(),
        )
        .unwrap();
        let store = ProvenanceStore::new(session_dir.clone());
        // v2 event with a later timestamp.
        store.append("s", change_event(None)).unwrap();
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 2);
        assert_eq!(loaded.events[0].event_id, "a-id");
    }

    #[test]
    fn test_duplicate_event_id_v2_wins() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join("s");
        let store = ProvenanceStore::new(session_dir.clone());
        let env = store.append("s", change_event(None)).unwrap();
        // Manually copy the v2 file content into v1 with the same id.
        let v1_dir = session_dir.join("provenance/v1/events");
        std::fs::create_dir_all(&v1_dir).unwrap();
        let v1_payload = serde_json::json!({
            "schema_version": 1,
            "event_id": env.event_id,
            "session_id": "s",
            "timestamp": "2026-01-01T00:00:00+00:00",
            "event": {
                "type": "change_committed",
                "transaction_id": "",
                "change_kind": "semantic_edit",
                "file": "src/lib.rs",
                "symbol_id": "sym",
                "before_fingerprint": "a",
                "after_fingerprint": "b",
                "diff": "d",
                "diff_hash": "blake3:x",
                "lines_added": 1,
                "lines_removed": 0
            }
        });
        std::fs::write(
            v1_dir.join(format!("{}.json", env.event_id)),
            serde_json::to_string_pretty(&v1_payload).unwrap(),
        )
        .unwrap();
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 1);
        assert!(loaded.warnings.iter().any(|w| w.contains("v2 wins")));
        // The surviving copy is the v2 one (it has whole-file hashes).
        match &loaded.events[0].event {
            ProvenanceEvent::ChangeCommitted(c) => {
                assert!(c.after.content_hash.is_some());
            }
            _ => panic!("expected change"),
        }
    }

    #[test]
    fn test_unique_event_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(dir.path().join("s"));
        for _ in 0..10 {
            store.append("s", change_event(None)).unwrap();
        }
        let files: Vec<_> = std::fs::read_dir(current_dir(&store))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(files.len(), 10);
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 10);
    }

    #[test]
    fn test_ordering_by_timestamp_then_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::from_events_dir(dir.path().to_path_buf());
        std::fs::create_dir_all(store.current_events_path()).unwrap();
        // Hand-write two v2 events with explicit timestamps out of filename order.
        let mk = |id: &str, ts: &str| {
            let wire = wire_v2::V2Envelope {
                schema_version: PROVENANCE_SCHEMA_VERSION,
                event_id: id.to_string(),
                session_id: "s".to_string(),
                timestamp: ts.to_string(),
                event: wire_v2::V2Event::PlanChanged(wire_v2::V2PlanChanged { changes: vec![] }),
            };
            let payload = serde_json::to_string_pretty(&wire).unwrap();
            std::fs::write(
                store.current_events_path().join(format!("{id}.json")),
                payload,
            )
            .unwrap();
        };
        mk("b-id", "2026-01-02T00:00:00+00:00");
        mk("a-id", "2026-01-01T00:00:00+00:00");
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 2);
        assert_eq!(loaded.events[0].event_id, "a-id");
        assert_eq!(loaded.events[1].event_id, "b-id");
    }

    #[test]
    fn test_malformed_json_skipped_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(dir.path().join("s"));
        store.append("s", change_event(None)).unwrap();
        std::fs::write(current_dir(&store).join("broken.json"), "{not json").unwrap();
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 1);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("malformed"));
    }

    #[test]
    fn test_corrupt_v1_and_v2_both_skip_independently() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join("s");
        let store = ProvenanceStore::new(session_dir.clone());
        store.append("s", change_event(None)).unwrap();
        std::fs::write(current_dir(&store).join("bad-v2.json"), "{nope").unwrap();
        let v1_dir = session_dir.join("provenance/v1/events");
        std::fs::create_dir_all(&v1_dir).unwrap();
        std::fs::write(v1_dir.join("bad-v1.json"), "{nope").unwrap();
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 1);
        assert_eq!(loaded.warnings.len(), 2);
    }

    #[test]
    fn test_unknown_version_skipped_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::from_events_dir(dir.path().to_path_buf());
        std::fs::create_dir_all(store.current_events_path()).unwrap();
        let payload = serde_json::json!({
            "schema_version": 3,
            "event_id": "future",
            "session_id": "s",
            "timestamp": "2026-01-01T00:00:00+00:00",
            "event": {"type": "plan_changed", "changes": []}
        });
        std::fs::write(
            store.current_events_path().join("future.json"),
            serde_json::to_string_pretty(&payload).unwrap(),
        )
        .unwrap();
        let loaded = store.load_all().unwrap();
        assert!(loaded.events.is_empty());
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("Unsupported provenance schema version 3"));
    }

    #[test]
    fn test_future_schema_v2_position_skipped_with_warning() {
        // A v2-shaped file claiming version 2 but with an unknown event tag
        // must not fail the whole load.
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::from_events_dir(dir.path().to_path_buf());
        std::fs::create_dir_all(store.current_events_path()).unwrap();
        let payload = serde_json::json!({
            "schema_version": 2,
            "event_id": "weird",
            "session_id": "s",
            "timestamp": "2026-01-01T00:00:00+00:00",
            "event": {"type": "intent_declared", "foo": 1}
        });
        std::fs::write(
            store.current_events_path().join("weird.json"),
            serde_json::to_string_pretty(&payload).unwrap(),
        )
        .unwrap();
        let loaded = store.load_all().unwrap();
        assert!(loaded.events.is_empty());
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("malformed"));
    }

    #[test]
    fn test_transaction_id_backfilled_from_event_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(dir.path().join("s"));
        let env = store.append("s", change_event(None)).unwrap();
        match &env.event {
            ProvenanceEvent::ChangeCommitted(c) => {
                assert_eq!(c.transaction_id, env.event_id);
            }
            _ => panic!("expected change"),
        }
    }

    #[test]
    fn test_plan_changed_envelope_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(dir.path().join("s"));
        let event = ProvenanceEvent::PlanChanged(PlanChangedEvent {
            changes: vec![PlanItemTransition {
                plan_item_id: "step-1".to_string(),
                parent_id: None,
                content: "Do work".to_string(),
                before_status: None,
                after_status: Some("pending".to_string()),
            }],
        });
        let env = store.append("s", event).unwrap();
        assert_eq!(env.schema_version, PROVENANCE_SCHEMA_VERSION);
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 1);
    }

    #[test]
    fn test_find_predecessor_linear_and_broken() {
        use crate::provenance::types::ChangeTarget as CT;
        let before0 = FileStateEvidence {
            exists: true,
            content_hash: Some("blake3:h0".to_string()),
            byte_len: Some(1),
        };
        let after1 = FileStateEvidence {
            exists: true,
            content_hash: Some("blake3:h1".to_string()),
            byte_len: Some(1),
        };
        let after2 = FileStateEvidence {
            exists: true,
            content_hash: Some("blake3:h2".to_string()),
            byte_len: Some(1),
        };
        let mk = |id: &str, before: FileStateEvidence, after: FileStateEvidence| {
            ProvenanceEventEnvelope {
                schema_version: PROVENANCE_SCHEMA_VERSION,
                event_id: id.to_string(),
                session_id: "s".to_string(),
                timestamp: "2026-01-01T00:00:00+00:00".to_string(),
                event: ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                    transaction_id: id.to_string(),
                    plan_item_id: None,
                    change_kind: ChangeKind::TextEdit,
                    file: "a.txt".to_string(),
                    target: CT::File,
                    before,
                    after,
                    predecessor_change_id: None,
                    reverts_change_id: None,
                    diff: String::new(),
                    diff_hash: String::new(),
                    lines_added: 0,
                    lines_removed: 0,
                }),
            }
        };
        let a = mk("A", before0.clone(), after1.clone());
        // Linear: B.before == A.after.
        assert_eq!(
            ProvenanceStore::find_predecessor(std::slice::from_ref(&a), "a.txt", &after1),
            Some("A".to_string())
        );
        // Broken: external hx matches nothing.
        let hx = FileStateEvidence {
            exists: true,
            content_hash: Some("blake3:hx".to_string()),
            byte_len: Some(1),
        };
        let b_before = hx.clone();
        assert_eq!(
            ProvenanceStore::find_predecessor(std::slice::from_ref(&a), "a.txt", &b_before),
            None
        );
        let _ = after2;
    }
}
