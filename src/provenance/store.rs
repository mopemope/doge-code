use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

use super::types::{
    PROVENANCE_SCHEMA_VERSION, PlanChangedEvent, ProvenanceEvent, ProvenanceEventEnvelope,
};

/// Durable provenance store for one session.
///
/// Layout: `<session_dir>/provenance/v1/events/<event-id>.json`.
/// Deleting the session directory removes provenance with it; no DB
/// migration is ever required.
#[derive(Debug, Clone)]
pub struct ProvenanceStore {
    events_dir: PathBuf,
}

impl ProvenanceStore {
    pub fn new(session_dir: PathBuf) -> Self {
        Self {
            events_dir: session_dir.join("provenance").join("v1").join("events"),
        }
    }

    /// Test/session helper: build directly from a known events directory.
    #[cfg(test)]
    pub(crate) fn from_events_dir(events_dir: PathBuf) -> Self {
        Self { events_dir }
    }

    pub fn events_dir(&self) -> &Path {
        &self.events_dir
    }

    /// Atomically persist one event and return its envelope.
    ///
    /// The payload is written to a sibling temp file in the events directory
    /// and moved with `persist_noclobber` (fail if the destination already
    /// exists). Never check-then-write.
    pub fn append(
        &self,
        session_id: &str,
        event: ProvenanceEvent,
    ) -> Result<ProvenanceEventEnvelope> {
        std::fs::create_dir_all(&self.events_dir).with_context(|| {
            format!(
                "failed to create provenance events dir {}",
                self.events_dir.display()
            )
        })?;

        let mut event = event;
        let event_id = Uuid::now_v7().to_string();
        // Keep a single identity for semantic changes: when the caller leaves
        // `transaction_id` empty, backfill it with the envelope id so
        // `observed_change_ids` (envelope ids) and `transaction_id` agree.
        if let ProvenanceEvent::ChangeCommitted(ref mut committed) = event
            && committed.transaction_id.is_empty()
        {
            committed.transaction_id = event_id.clone();
        }

        let envelope = ProvenanceEventEnvelope {
            schema_version: PROVENANCE_SCHEMA_VERSION,
            event_id: event_id.clone(),
            session_id: session_id.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            event,
        };
        let payload = serde_json::to_string_pretty(&envelope)
            .context("failed to serialize provenance event")?;
        let dest = self.events_dir.join(format!("{event_id}.json"));

        atomic_write_new(&self.events_dir, &dest, payload.as_bytes())
            .with_context(|| format!("failed to persist provenance event {}", dest.display()))?;

        tracing::info!(
            event_id = %event_id,
            event_type = envelope.event_type().as_str(),
            session_id = %session_id,
            "provenance.event_recorded"
        );
        Ok(envelope)
    }

    /// Load all events, sorted by `(timestamp, event_id)`.
    ///
    /// One corrupt or future-schema file never fails the whole query; it is
    /// skipped with a warning entry.
    pub fn load_all(&self) -> Result<ProvenanceLoadResult> {
        let mut events = Vec::new();
        let mut warnings = Vec::new();
        if !self.events_dir.exists() {
            return Ok(ProvenanceLoadResult { events, warnings });
        }
        let read_dir = std::fs::read_dir(&self.events_dir).with_context(|| {
            format!(
                "failed to list provenance events dir {}",
                self.events_dir.display()
            )
        })?;
        for entry in read_dir {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warnings.push(format!(
                        "Skipping unreadable provenance directory entry in {}: {e}",
                        self.events_dir.display()
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
            let envelope: ProvenanceEventEnvelope = match serde_json::from_str(&raw) {
                Ok(env) => env,
                Err(e) => {
                    warnings.push(format!(
                        "Skipping malformed provenance event {}: {e}",
                        path.display()
                    ));
                    continue;
                }
            };
            if envelope.schema_version != PROVENANCE_SCHEMA_VERSION {
                warnings.push(format!(
                    "Unsupported provenance schema version {} in {}",
                    envelope.schema_version,
                    path.display()
                ));
                continue;
            }
            events.push(envelope);
        }
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

fn atomic_write_new(dir: &Path, dest: &Path, bytes: &[u8]) -> Result<()> {
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
    use crate::provenance::types::{ChangeCommittedEvent, ChangeKind, PlanItemTransition};

    fn change_event(plan_item_id: Option<&str>) -> ProvenanceEvent {
        ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
            transaction_id: String::new(),
            plan_item_id: plan_item_id.map(str::to_string),
            change_kind: ChangeKind::SemanticEdit,
            file: "src/lib.rs".to_string(),
            symbol_id: "sym-v1-abc".to_string(),
            before_fingerprint: "fp-v1-a".to_string(),
            after_fingerprint: "fp-v1-b".to_string(),
            diff: "diff".to_string(),
            diff_hash: "blake3:abc".to_string(),
            lines_added: 1,
            lines_removed: 0,
        })
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
        // Store round-trip with a fresh handle (session resume path).
        let store2 = ProvenanceStore::new(dir.path().join("sess-1"));
        let loaded = store2.load_all().unwrap();
        assert_eq!(loaded.events.len(), 1);
        assert!(loaded.warnings.is_empty());
        assert_eq!(loaded.events[0].event_id, env.event_id);
    }

    #[test]
    fn test_unique_event_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(dir.path().join("s"));
        for _ in 0..10 {
            store.append("s", change_event(None)).unwrap();
        }
        let files: Vec<_> = std::fs::read_dir(store.events_dir())
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
        std::fs::create_dir_all(store.events_dir()).unwrap();
        // Hand-write two events with explicit timestamps out of filename order.
        let mk = |id: &str, ts: &str| {
            let env = ProvenanceEventEnvelope {
                schema_version: PROVENANCE_SCHEMA_VERSION,
                event_id: id.to_string(),
                session_id: "s".to_string(),
                timestamp: ts.to_string(),
                event: change_event(None),
            };
            let payload = serde_json::to_string_pretty(&env).unwrap();
            std::fs::write(store.events_dir().join(format!("{id}.json")), payload).unwrap();
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
        std::fs::write(store.events_dir().join("broken.json"), "{not json").unwrap();
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 1);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("malformed"));
    }

    #[test]
    fn test_future_schema_skipped_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::from_events_dir(dir.path().to_path_buf());
        std::fs::create_dir_all(store.events_dir()).unwrap();
        let env = ProvenanceEventEnvelope {
            schema_version: 2,
            event_id: "future".to_string(),
            session_id: "s".to_string(),
            timestamp: "2026-01-01T00:00:00+00:00".to_string(),
            event: PlanChangedEvent { changes: vec![] }.into(),
        };
        // Serialize manually: envelope with version 2 must still parse as JSON.
        let payload = serde_json::to_string_pretty(&env).unwrap();
        std::fs::write(store.events_dir().join("future.json"), payload).unwrap();
        let loaded = store.load_all().unwrap();
        assert!(loaded.events.is_empty());
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("Unsupported provenance schema version 2"));
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
}
