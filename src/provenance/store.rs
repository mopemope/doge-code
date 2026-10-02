use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

use super::types::{
    LEGACY_PROVENANCE_SCHEMA_VERSION, PROVENANCE_SCHEMA_VERSION, PlanChangedEvent, ProvenanceEvent,
    ProvenanceEventEnvelope, V2_PROVENANCE_SCHEMA_VERSION, V3_PROVENANCE_SCHEMA_VERSION,
    V4_PROVENANCE_SCHEMA_VERSION, from_v1_wire, from_v2_wire, from_v3_wire, from_v4_wire,
    from_v5_wire, to_v5_wire,
};
use super::wire::{
    EventHeader, v1 as wire_v1, v2 as wire_v2, v3 as wire_v3, v4 as wire_v4, v5 as wire_v5,
};

/// Durable provenance store for one session.
///
/// Layout:
/// - legacy v1 (read-only): `<session_dir>/provenance/v1/events/<event-id>.json`
/// - legacy v2 (read-only): `<session_dir>/provenance/v2/events/<event-id>.json`
/// - legacy v3 (read-only): `<session_dir>/provenance/v3/events/<event-id>.json`
/// - legacy v4 (read-only): `<session_dir>/provenance/v4/events/<event-id>.json`
/// - current v5 (read + write): `<session_dir>/provenance/v5/events/<event-id>.json`
///
/// v1/v2/v3/v4 files are never copied, rewritten, or migrated on disk; they are
/// converted into the canonical representation on read. New events are
/// always written as v5.
#[derive(Debug, Clone)]
pub struct ProvenanceStore {
    provenance_root: PathBuf,
    current_dir: PathBuf,
}

impl ProvenanceStore {
    pub fn new(session_dir: PathBuf) -> Self {
        let provenance_root = session_dir.join("provenance");
        let current_dir = provenance_root.join("v5").join("events");
        Self {
            provenance_root,
            current_dir,
        }
    }

    /// Test/session helper: build directly from a known events directory.
    ///
    /// The given directory is treated as the current (v3) events dir; its
    /// parent structure is faked so `legacy_events_dir()` points at a
    /// sibling `v1/events` when present. Prefer [`ProvenanceStore::new`]
    /// for session paths.
    #[cfg(test)]
    pub(crate) fn from_events_dir(events_dir: PathBuf) -> Self {
        // `.../provenance/v3/events` -> provenance_root `.../provenance`.
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

    fn v1_events_dir(&self) -> PathBuf {
        self.provenance_root.join("v1").join("events")
    }

    fn v2_events_dir(&self) -> PathBuf {
        self.provenance_root.join("v2").join("events")
    }

    fn v3_events_dir(&self) -> PathBuf {
        self.provenance_root.join("v3").join("events")
    }

    fn v4_events_dir(&self) -> PathBuf {
        self.provenance_root.join("v4").join("events")
    }

    fn current_events_dir(&self) -> PathBuf {
        self.current_dir.clone()
    }

    /// Current v5 write directory.
    ///
    /// This is the only directory new events are written to. Reads cover
    /// v1/v2/v3/v4 (legacy) and v5; see [`ProvenanceStore::load_all`].
    pub fn events_dir(&self) -> &Path {
        &self.current_dir
    }

    /// Owned path of the current v5 events directory.
    pub fn current_events_path(&self) -> PathBuf {
        self.current_events_dir()
    }

    /// Owned path of the legacy v1 events directory (read-only).
    pub fn legacy_events_path(&self) -> PathBuf {
        self.v1_events_dir()
    }

    /// Owned path of the legacy v2 events directory (read-only).
    pub fn v2_events_path(&self) -> PathBuf {
        self.v2_events_dir()
    }

    /// Owned path of the legacy v3 events directory (read-only).
    pub fn v3_events_path(&self) -> PathBuf {
        self.v3_events_dir()
    }

    /// Atomically persist one event and return its envelope.
    ///
    /// The payload is written to a sibling temp file in the v5 events
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
        let wire = to_v5_wire(&canonical);
        let payload =
            serde_json::to_string_pretty(&wire).context("failed to serialize provenance event")?;
        let dest = current_dir.join(format!("{event_id}.json"));

        persist_sibling_noclobber(&current_dir, &dest, payload.as_bytes())
            .with_context(|| format!("failed to persist provenance event {}", dest.display()))?;

        // Never log directive content: only ids, origin, and hashes.
        match &canonical.event {
            ProvenanceEvent::DirectiveObserved(d) => {
                tracing::info!(
                    event_id = %event_id,
                    event_type = canonical.event_type().as_str(),
                    session_id = %session_id,
                    origin = d.origin.as_str(),
                    raw_input_hash = %d.raw_input_hash,
                    "provenance.event_recorded"
                );
            }
            _ => {
                tracing::info!(
                    event_id = %event_id,
                    event_type = canonical.event_type().as_str(),
                    session_id = %session_id,
                    "provenance.event_recorded"
                );
            }
        }
        Ok(canonical)
    }

    /// Load all events from v1 + v2 + v3 + v4 (legacy) and v5 (current), merged and
    /// sorted by `(timestamp, event_id)`.
    ///
    /// One corrupt or future-schema file never fails the whole query; it is
    /// skipped with a warning entry. When the same event id exists in
    /// multiple versions (manual copy/migration mistake, not expected UUID
    /// collision), the highest version wins deterministically (v5 > v4 > v3 > v2 > v1)
    /// and a warning explains why.
    pub fn load_all(&self) -> Result<ProvenanceLoadResult> {
        let mut warnings = Vec::new();
        // event_id -> (envelope, version_rank)
        let mut by_id: HashMap<String, (ProvenanceEventEnvelope, u32)> = HashMap::new();
        // Track source dirs for duplicate diagnostics without cloning envelopes.
        let mut id_source: HashMap<String, &'static str> = HashMap::new();

        for (dir, rank, label) in [
            (self.v1_events_dir(), 1u32, "v1"),
            (self.v2_events_dir(), 2u32, "v2"),
            (self.v3_events_dir(), 3u32, "v3"),
            (self.v4_events_dir(), 4u32, "v4"),
            (self.current_events_dir(), 5u32, "v5"),
        ] {
            let loaded = load_events_dir(&dir, &mut warnings);
            for env in loaded {
                let id = env.event_id.clone();
                match by_id.get(&id) {
                    None => {
                        id_source.insert(id.clone(), label);
                        by_id.insert(id, (env, rank));
                    }
                    Some((_, existing_rank)) => {
                        let existing_src = id_source.get(&id).copied().unwrap_or("?");
                        if rank > *existing_rank {
                            warnings.push(format!(
                                "Duplicate provenance event id {id} in {existing_src} and {label}; using {label} copy ({label} wins; check for manual copy/migration mistake)"
                            ));
                            id_source.insert(id.clone(), label);
                            by_id.insert(id, (env, rank));
                        } else if rank < *existing_rank {
                            warnings.push(format!(
                                "Duplicate provenance event id {id} in {existing_src} and {label}; keeping {existing_src} copy ({existing_src} wins; check for manual copy/migration mistake)"
                            ));
                        } else {
                            warnings.push(format!(
                                "Duplicate provenance event id {id} within {label}; keeping first copy"
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

    /// Find the predecessor of a mutation: the latest `ChangeCommitted`
    /// for `file` whose `after` state equals `before`.
    ///
    /// Only v2/v3 events carry whole-file hashes, so v1 events never connect
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
        } else if header.schema_version == V2_PROVENANCE_SCHEMA_VERSION {
            match serde_json::from_str::<wire_v2::V2Envelope>(&raw) {
                Ok(env) => events.push(from_v2_wire(env)),
                Err(e) => warnings.push(format!(
                    "Skipping malformed provenance event {}: {e}",
                    path.display()
                )),
            }
        } else if header.schema_version == V3_PROVENANCE_SCHEMA_VERSION {
            match serde_json::from_str::<wire_v3::V3Envelope>(&raw) {
                Ok(env) => events.push(from_v3_wire(env)),
                Err(e) => warnings.push(format!(
                    "Skipping malformed provenance event {}: {e}",
                    path.display()
                )),
            }
        } else if header.schema_version == V4_PROVENANCE_SCHEMA_VERSION {
            match serde_json::from_str::<wire_v4::V4Envelope>(&raw) {
                Ok(env) => events.push(from_v4_wire(env)),
                Err(e) => warnings.push(format!(
                    "Skipping malformed provenance event {}: {e}",
                    path.display()
                )),
            }
        } else if header.schema_version == PROVENANCE_SCHEMA_VERSION {
            match serde_json::from_str::<wire_v5::V5Envelope>(&raw) {
                Ok(env) => events.push(from_v5_wire(env)),
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
            directive_id: None,
            plan_item_id: plan_item_id.map(str::to_string),
            requirement_ids: Vec::new(),
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
    fn test_events_dir_returns_current_v3_write_dir() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(dir.path().join("s"));
        assert!(
            store
                .events_dir()
                .to_string_lossy()
                .ends_with("provenance/v5/events")
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
    fn test_v3_roundtrip_preserves_hashes_and_links() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(dir.path().join("s"));
        let mut event = change_event(None);
        if let ProvenanceEvent::ChangeCommitted(ref mut c) = event {
            c.predecessor_change_id = Some("prev-1".to_string());
            c.reverts_change_id = Some("rev-1".to_string());
            c.directive_id = Some("d1".to_string());
            c.requirement_ids = vec!["r1".to_string()];
        }
        let env = store.append("s", event).unwrap();
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 1);
        match &loaded.events[0].event {
            ProvenanceEvent::ChangeCommitted(c) => {
                assert_eq!(c.predecessor_change_id.as_deref(), Some("prev-1"));
                assert_eq!(c.reverts_change_id.as_deref(), Some("rev-1"));
                assert!(c.before.content_hash.is_some());
                assert_eq!(c.directive_id.as_deref(), Some("d1"));
                assert_eq!(c.requirement_ids, vec!["r1".to_string()]);
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
                assert_eq!(c.directive_id, None);
                assert!(c.requirement_ids.is_empty());
            }
            _ => panic!("expected change"),
        }
    }

    #[test]
    fn test_v2_fixture_loads_with_empty_attribution() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join("s");
        let v2_dir = session_dir.join("provenance/v2/events");
        std::fs::create_dir_all(&v2_dir).unwrap();
        let payload = serde_json::json!({
            "schema_version": 2,
            "event_id": "evt-v2-1",
            "session_id": "s",
            "timestamp": "2026-01-01T00:00:00+00:00",
            "event": {
                "type": "change_committed",
                "transaction_id": "",
                "plan_item_id": "step-1",
                "change_kind": "text_edit",
                "file": "a.txt",
                "target": {"scope": "file"},
                "before": {"exists": true, "content_hash": "blake3:x", "byte_len": 1},
                "after": {"exists": true, "content_hash": "blake3:y", "byte_len": 1},
                "diff": "d",
                "diff_hash": "blake3:z",
                "lines_added": 1,
                "lines_removed": 0
            }
        });
        std::fs::write(
            v2_dir.join("evt-v2-1.json"),
            serde_json::to_string_pretty(&payload).unwrap(),
        )
        .unwrap();
        let store = ProvenanceStore::new(session_dir);
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 1);
        match &loaded.events[0].event {
            ProvenanceEvent::ChangeCommitted(c) => {
                assert_eq!(c.directive_id, None);
                assert!(c.requirement_ids.is_empty());
            }
            _ => panic!("expected change"),
        }
    }

    #[test]
    fn test_mixed_v1_v2_v3_merges_and_sorts() {
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
        // v2 middle timestamp.
        let v2_dir = session_dir.join("provenance/v2/events");
        std::fs::create_dir_all(&v2_dir).unwrap();
        let v2_payload = serde_json::json!({
            "schema_version": 2,
            "event_id": "m-id",
            "session_id": "s",
            "timestamp": "2026-01-02T00:00:00+00:00",
            "event": {"type": "plan_changed", "changes": []}
        });
        std::fs::write(
            v2_dir.join("m-id.json"),
            serde_json::to_string_pretty(&v2_payload).unwrap(),
        )
        .unwrap();
        let store = ProvenanceStore::new(session_dir.clone());
        // v3 event with a later timestamp.
        store.append("s", change_event(None)).unwrap();
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 3);
        assert_eq!(loaded.events[0].event_id, "a-id");
        assert_eq!(loaded.events[1].event_id, "m-id");
    }

    #[test]
    fn test_duplicate_event_id_v3_wins() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join("s");
        let store = ProvenanceStore::new(session_dir.clone());
        let env = store.append("s", change_event(None)).unwrap();
        // Manually copy the v3 id into v1 and v2 with the same id.
        for (subdir, version) in [("v1", 1), ("v2", 2)] {
            let legacy_dir = session_dir.join(format!("provenance/{subdir}/events"));
            std::fs::create_dir_all(&legacy_dir).unwrap();
            let payload = if version == 1 {
                serde_json::json!({
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
                })
            } else {
                serde_json::json!({
                    "schema_version": 2,
                    "event_id": env.event_id,
                    "session_id": "s",
                    "timestamp": "2026-01-01T00:00:00+00:00",
                    "event": {
                        "type": "change_committed",
                        "transaction_id": "",
                        "change_kind": "semantic_edit",
                        "file": "src/lib.rs",
                        "target": {"scope": "file"},
                        "before": {"exists": true},
                        "after": {"exists": true},
                        "diff": "d",
                        "diff_hash": "blake3:x",
                        "lines_added": 1,
                        "lines_removed": 0
                    }
                })
            };
            std::fs::write(
                legacy_dir.join(format!("{}.json", env.event_id)),
                serde_json::to_string_pretty(&payload).unwrap(),
            )
            .unwrap();
        }
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 1);
        assert!(
            loaded
                .warnings
                .iter()
                .any(|w| w.contains("v3 wins") || w.contains("wins"))
        );
        // The surviving copy is the v3 one (it has whole-file hashes).
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
        // Hand-write two v3 events with explicit timestamps out of filename order.
        let mk = |id: &str, ts: &str| {
            let wire = wire_v3::V3Envelope {
                schema_version: PROVENANCE_SCHEMA_VERSION,
                event_id: id.to_string(),
                session_id: "s".to_string(),
                timestamp: ts.to_string(),
                event: wire_v3::V3Event::PlanChanged(wire_v3::V3PlanChanged {
                    directive_id: None,
                    changes: vec![],
                }),
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
    fn test_corrupt_v1_v2_v3_all_skip_independently() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join("s");
        let store = ProvenanceStore::new(session_dir.clone());
        store.append("s", change_event(None)).unwrap();
        std::fs::write(current_dir(&store).join("bad-v3.json"), "{nope").unwrap();
        let v1_dir = session_dir.join("provenance/v1/events");
        std::fs::create_dir_all(&v1_dir).unwrap();
        std::fs::write(v1_dir.join("bad-v1.json"), "{nope").unwrap();
        let v2_dir = session_dir.join("provenance/v2/events");
        std::fs::create_dir_all(&v2_dir).unwrap();
        std::fs::write(v2_dir.join("bad-v2.json"), "{nope").unwrap();
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 1);
        assert_eq!(loaded.warnings.len(), 3);
    }

    #[test]
    fn test_unknown_version_skipped_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::from_events_dir(dir.path().to_path_buf());
        std::fs::create_dir_all(store.current_events_path()).unwrap();
        let payload = serde_json::json!({
            "schema_version": 99,
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
        assert!(loaded.warnings[0].contains("Unsupported provenance schema version 99"));
    }

    #[test]
    fn test_future_schema_v3_position_skipped_with_warning() {
        // A v3-shaped file claiming version 3 but with an unknown event tag
        // must not fail the whole load.
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::from_events_dir(dir.path().to_path_buf());
        std::fs::create_dir_all(store.current_events_path()).unwrap();
        let payload = serde_json::json!({
            "schema_version": 3,
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
            directive_id: Some("d1".to_string()),
            changes: vec![PlanItemTransition {
                plan_item_id: "step-1".to_string(),
                parent_id: None,
                content: "Do work".to_string(),
                before_status: None,
                after_status: Some("pending".to_string()),
                before_requirement_ids: Vec::new(),
                before_verification_obligations: Vec::new(),
                after_requirement_ids: vec!["r1".to_string()],
                after_verification_obligations: Vec::new(),
            }],
        });
        let env = store.append("s", event).unwrap();
        assert_eq!(env.schema_version, PROVENANCE_SCHEMA_VERSION);
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 1);
    }

    #[test]
    fn test_v3_event_reads_with_empty_obligations() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join("s");
        let v3_dir = session_dir.join("provenance/v3/events");
        std::fs::create_dir_all(&v3_dir).unwrap();
        // v3 plan_changed without obligation fields.
        let payload = serde_json::json!({
            "schema_version": 3,
            "event_id": "evt-v3-1",
            "session_id": "s",
            "timestamp": "2026-01-01T00:00:00+00:00",
            "event": {
                "type": "plan_changed",
                "changes": [{
                    "plan_item_id": "step-1",
                    "content": "a",
                    "before_requirement_ids": [],
                    "after_requirement_ids": []
                }]
            }
        });
        std::fs::write(
            v3_dir.join("evt-v3-1.json"),
            serde_json::to_string_pretty(&payload).unwrap(),
        )
        .unwrap();
        // v3 verification without matched_obligations.
        let vpayload = serde_json::json!({
            "schema_version": 3,
            "event_id": "evt-v3-2",
            "session_id": "s",
            "timestamp": "2026-01-02T00:00:00+00:00",
            "event": {
                "type": "verification_observed",
                "verification_kind": "test",
                "source": "execute_process",
                "command": {"program": "cargo", "args": ["test"]},
                "outcome": {"success": true, "status": "completed"},
                "observed_change_ids": []
            }
        });
        std::fs::write(
            v3_dir.join("evt-v3-2.json"),
            serde_json::to_string_pretty(&vpayload).unwrap(),
        )
        .unwrap();
        let store = ProvenanceStore::new(session_dir);
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 2);
        for e in &loaded.events {
            match &e.event {
                ProvenanceEvent::PlanChanged(p) => {
                    for t in &p.changes {
                        assert!(t.before_verification_obligations.is_empty());
                        assert!(t.after_verification_obligations.is_empty());
                    }
                }
                ProvenanceEvent::VerificationObserved(v) => {
                    assert!(v.matched_obligations.is_empty());
                }
                _ => {}
            }
        }
    }

    #[test]
    fn test_v5_plan_changed_roundtrip() {
        use crate::provenance::types::{
            VerificationCommandMatcher, VerificationKind, VerificationObligation,
        };
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(dir.path().join("s"));
        let ob = VerificationObligation {
            id: "vo-1".to_string(),
            description: "desc".to_string(),
            kind: VerificationKind::Test,
            command: Some(VerificationCommandMatcher {
                program: "cargo".to_string(),
                args_prefix: vec!["test".to_string()],
            }),
        };
        let event = ProvenanceEvent::PlanChanged(PlanChangedEvent {
            directive_id: None,
            changes: vec![PlanItemTransition {
                plan_item_id: "step-1".to_string(),
                parent_id: None,
                content: "a".to_string(),
                before_status: None,
                after_status: Some("pending".to_string()),
                before_requirement_ids: Vec::new(),
                before_verification_obligations: Vec::new(),
                after_requirement_ids: Vec::new(),
                after_verification_obligations: vec![ob.clone()],
            }],
        });
        let env = store.append("s", event).unwrap();
        assert_eq!(env.schema_version, PROVENANCE_SCHEMA_VERSION);
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 1);
        match &loaded.events[0].event {
            ProvenanceEvent::PlanChanged(p) => {
                assert_eq!(p.changes[0].after_verification_obligations, vec![ob]);
            }
            _ => panic!("expected plan"),
        }
        // Raw file is under v4.
        let v4_dir = store.current_events_path();
        assert!(v4_dir.to_string_lossy().ends_with("provenance/v5/events"));
        let raw = std::fs::read_to_string(v4_dir.join(format!("{}.json", env.event_id))).unwrap();
        assert!(raw.contains("\"schema_version\": 5") || raw.contains("\"schema_version\":5"));
    }

    #[test]
    fn test_v5_verification_roundtrip() {
        use crate::provenance::types::VerificationObligationRef;
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(dir.path().join("s"));
        let event = ProvenanceEvent::VerificationObserved(
            crate::provenance::types::VerificationObservedEvent {
                execution_workspace: None,
                directive_id: None,
                plan_item_id: Some("step-1".to_string()),
                requirement_ids: vec![],
                verification_kind: crate::provenance::types::VerificationKind::Test,
                source: crate::provenance::types::VerificationSource::ExecuteProcess,
                command: crate::provenance::types::CommandEvidence {
                    program: "cargo".to_string(),
                    args: vec!["test".to_string()],
                    cwd: None,
                },
                outcome: crate::provenance::types::VerificationOutcome {
                    success: true,
                    status: "completed".to_string(),
                    exit_code: Some(0),
                    timed_out: false,
                },
                observed_change_ids: vec!["c1".to_string()],
                matched_obligations: vec![VerificationObligationRef {
                    id: "vo-1".to_string(),
                    binding_hash: "blake3:abc".to_string(),
                }],
                stdout_excerpt: String::new(),
                stderr_excerpt: String::new(),
                output_digest: String::new(),
                output_truncated: false,
                warnings: vec![],
            },
        );
        let env = store.append("s", event).unwrap();
        let loaded = store.load_all().unwrap();
        match &loaded.events[0].event {
            ProvenanceEvent::VerificationObserved(v) => {
                assert_eq!(v.matched_obligations.len(), 1);
                assert_eq!(v.matched_obligations[0].id, "vo-1");
            }
            _ => panic!("expected verification"),
        }
        let _ = env;
    }

    #[test]
    fn test_mixed_v1_v2_v3_v4_load() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join("s");
        // v1
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
        // v2
        let v2_dir = session_dir.join("provenance/v2/events");
        std::fs::create_dir_all(&v2_dir).unwrap();
        let v2_payload = serde_json::json!({
            "schema_version": 2,
            "event_id": "m-id",
            "session_id": "s",
            "timestamp": "2026-01-02T00:00:00+00:00",
            "event": {"type": "plan_changed", "changes": []}
        });
        std::fs::write(
            v2_dir.join("m-id.json"),
            serde_json::to_string_pretty(&v2_payload).unwrap(),
        )
        .unwrap();
        // v3
        let v3_dir = session_dir.join("provenance/v3/events");
        std::fs::create_dir_all(&v3_dir).unwrap();
        let v3_payload = serde_json::json!({
            "schema_version": 3,
            "event_id": "v3-id",
            "session_id": "s",
            "timestamp": "2026-01-03T00:00:00+00:00",
            "event": {"type": "plan_changed", "changes": []}
        });
        std::fs::write(
            v3_dir.join("v3-id.json"),
            serde_json::to_string_pretty(&v3_payload).unwrap(),
        )
        .unwrap();
        let store = ProvenanceStore::new(session_dir.clone());
        store.append("s", change_event(None)).unwrap();
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 4);
        assert_eq!(loaded.events[0].event_id, "a-id");
        assert_eq!(loaded.events[1].event_id, "m-id");
        assert_eq!(loaded.events[2].event_id, "v3-id");
    }

    #[test]
    fn test_store_writes_only_under_v5() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join("s");
        let store = ProvenanceStore::new(session_dir.clone());
        store.append("s", change_event(None)).unwrap();
        assert!(
            !session_dir.join("provenance/v1/events").exists()
                || std::fs::read_dir(session_dir.join("provenance/v1/events"))
                    .map(|mut d| d.next().is_none())
                    .unwrap_or(true)
        );
        let v4_files: Vec<_> = std::fs::read_dir(session_dir.join("provenance/v5/events"))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(v4_files.len(), 1);
    }

    #[test]
    fn test_duplicate_id_v4_wins_over_v3_v2_v1() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join("s");
        let store = ProvenanceStore::new(session_dir.clone());
        let env = store.append("s", change_event(None)).unwrap();
        // Copy same id into v1/v2/v3.
        for (subdir, version) in [("v1", 1), ("v2", 2), ("v3", 3)] {
            let legacy_dir = session_dir.join(format!("provenance/{subdir}/events"));
            std::fs::create_dir_all(&legacy_dir).unwrap();
            let payload = if version == 1 {
                serde_json::json!({
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
                })
            } else if version == 2 {
                serde_json::json!({
                    "schema_version": 2,
                    "event_id": env.event_id,
                    "session_id": "s",
                    "timestamp": "2026-01-01T00:00:00+00:00",
                    "event": {
                        "type": "change_committed",
                        "transaction_id": "",
                        "plan_item_id": "step-1",
                        "change_kind": "text_edit",
                        "file": "a.txt",
                        "target": {"scope": "file"},
                        "before": {"exists": true},
                        "after": {"exists": true},
                        "diff": "d",
                        "diff_hash": "blake3:x",
                        "lines_added": 1,
                        "lines_removed": 0
                    }
                })
            } else {
                serde_json::json!({
                    "schema_version": 3,
                    "event_id": env.event_id,
                    "session_id": "s",
                    "timestamp": "2026-01-01T00:00:00+00:00",
                    "event": {
                        "type": "change_committed",
                        "transaction_id": "",
                        "plan_item_id": "step-1",
                        "change_kind": "text_edit",
                        "file": "a.txt",
                        "target": {"scope": "file"},
                        "before": {"exists": true},
                        "after": {"exists": true},
                        "diff": "d",
                        "diff_hash": "blake3:x",
                        "lines_added": 1,
                        "lines_removed": 0
                    }
                })
            };
            std::fs::write(
                legacy_dir.join(format!("{}.json", env.event_id)),
                serde_json::to_string_pretty(&payload).unwrap(),
            )
            .unwrap();
        }
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 1);
        assert!(
            loaded
                .warnings
                .iter()
                .any(|w| w.contains("v4 wins") || w.contains("wins"))
        );
    }

    #[test]
    fn test_malformed_v4_skipped_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(dir.path().join("s"));
        store.append("s", change_event(None)).unwrap();
        std::fs::write(current_dir(&store).join("bad-v4.json"), "{not json").unwrap();
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 1);
        assert!(loaded.warnings.iter().any(|w| w.contains("malformed")));
    }

    #[test]
    fn test_unsupported_future_schema_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::from_events_dir(dir.path().to_path_buf());
        std::fs::create_dir_all(store.current_events_path()).unwrap();
        let payload = serde_json::json!({
            "schema_version": 99,
            "event_id": "future2",
            "session_id": "s",
            "timestamp": "2026-01-01T00:00:00+00:00",
            "event": {"type": "plan_changed", "changes": []}
        });
        std::fs::write(
            store.current_events_path().join("future2.json"),
            serde_json::to_string_pretty(&payload).unwrap(),
        )
        .unwrap();
        let loaded = store.load_all().unwrap();
        assert!(loaded.events.is_empty());
        assert!(loaded.warnings[0].contains("Unsupported provenance schema version 99"));
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
                    directive_id: None,
                    plan_item_id: None,
                    requirement_ids: Vec::new(),
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

#[cfg(test)]
mod execution_snapshot_tests {
    use super::*;
    use crate::provenance::*;
    #[tokio::test]
    async fn v5_snapshot_roundtrip_and_frozen_v4_reader_preserve_history() {
        let root = tempfile::tempdir().expect("project");
        std::fs::write(root.path().join("input"), "PRIVATE-SOURCE").expect("input");
        let record = crate::features::verification_snapshot::finish(
            root.path(),
            crate::features::verification_snapshot::begin(
                root.path(),
                std::collections::BTreeSet::from(["input".into()]),
                None,
            )
            .await,
            None,
        )
        .await;
        let store = ProvenanceStore::new(root.path().join("session"));
        let event = build_verification_event(VerificationRecordInput {
            kind: VerificationKind::Test,
            source: VerificationSource::ExecuteProcess,
            program: "cargo",
            args: &["test".into()],
            cwd_relative: None,
            success: true,
            status: "completed",
            exit_code: Some(0),
            timed_out: false,
            stdout: "",
            stderr: "",
            capture_truncated: false,
            context: VerificationContext {
                execution_workspace: Some(record),
                ..Default::default()
            },
            extra_warnings: vec![],
        });
        let env = store
            .append("s", ProvenanceEvent::VerificationObserved(event))
            .expect("append v5");
        let v5_file = store.events_dir().join(format!("{}.json", env.event_id));
        let v5_before = std::fs::read(&v5_file).expect("bytes");
        assert!(!String::from_utf8_lossy(&v5_before).contains("PRIVATE-SOURCE"));
        let loaded = store.load_all().expect("load v5");
        match &loaded.events[0].event {
            ProvenanceEvent::VerificationObserved(v) => assert!(v.execution_workspace.is_some()),
            _ => panic!("verification"),
        }
        let frozen = crate::provenance::types::to_v4_wire(&env);
        assert_eq!(frozen.schema_version, 4);
        let v4_bytes = serde_json::to_vec(&frozen).expect("v4 wire");
        assert!(!String::from_utf8_lossy(&v4_bytes).contains("execution_workspace"));
        let legacy = store.v4_events_dir();
        std::fs::create_dir_all(&legacy).expect("legacy dir");
        let legacy_file = legacy.join(format!("{}.json", env.event_id));
        std::fs::write(&legacy_file, &v4_bytes).expect("v4 fixture");
        let both = store.load_all().expect("mixed versions");
        assert_eq!(both.events.len(), 1);
        assert!(both.warnings.iter().any(|w| w.contains("v5 wins")));
        assert_eq!(std::fs::read(&v5_file).expect("unchanged v5"), v5_before);
        std::fs::remove_file(v5_file).expect("remove test v5");
        let old = store.load_all().expect("legacy only");
        match &old.events[0].event {
            ProvenanceEvent::VerificationObserved(v) => {
                assert!(v.execution_workspace.is_none());
                assert!(v.outcome.success);
            }
            _ => panic!("verification"),
        }
        assert_eq!(std::fs::read(legacy_file).expect("unchanged v4"), v4_bytes);
        assert_eq!(
            std::fs::read_dir(store.events_dir())
                .expect("v5 directory")
                .count(),
            0
        );
    }
}
