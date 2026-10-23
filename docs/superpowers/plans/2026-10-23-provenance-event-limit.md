# Provenance event write limit implementation plan

> Execute natively with systematic-debugging and test-driven-development; request independent whole-branch review before publication.

**Goal:** Never publish a new provenance event that exceeds the existing authoritative reader's 8 MiB per-event limit.

**Architecture:** Share one serialized event byte limit between append and complete reads. Serialize current v8 wire data through a bounded writer before creating/publishing any event file. Preserve existing append failure propagation and FsTools finalization/undo semantics.

**Spec:** User authorized next highest priority follow-up through PR creation after merging the macOS fixture PR. Refresh develop/PR/CI at execution; do not tie future work to a fixed commit.

## Acceptance / safety constraints

- Limit is serialized UTF-8 JSON bytes including envelope, pretty formatting and escaping; exactly8 MiB is allowed, larger is rejected.
- Do not truncate directives, diffs, verification observations or rewrite legacy evidence.
- Reject oversize before event directory/temp file creation; existing bytes/ids remain unchanged and reloadable after restart.
- Bounded serialization avoids creating a complete oversized JSON allocation; errors contain no event content.
- A committed source change is never rolled back due to provenance failure. Existing warnings, provenance_incomplete count, receipts and conflict-safe undo remain intact.
- Preserve v1-v8 adapters, no-clobber persistence, complete-reader corruption rejection and historical display limits.
- Aggregate128 MiB/65,536-entry read limits and display256 KiB per-event budget remain separate, fail-closed policies; no cross-process append quota/locking design in this slice.
- No real credentials, inference, dependency/MSRV/config changes, direct develop updates, force push or merge. Commit identity mopemope <yutaka.matsubara@gmail.com>.

## Task 1: Reproduce and protect store boundary

Files: src/provenance/store.rs tests and append/read boundary.

- [x] Add append oversized ASCII/escaped-control event regressions: reject without adding files, old event bytes preserved, fresh store load_complete still returns old envelope.
- [x] Observe RED against existing writer, documenting that append succeeds while fresh load_complete rejects oversized stored evidence.
- [x] Introduce shared8 MiB constant and private serialize_event(V8Envelope) bounded-byte serializer; use it before create_dir_all/persist_sibling_noclobber.
- [x] Verify exact-limit and one-byte-over serialized wire data; roundtrip large valid Unicode/directive or change contents without altering hashes/links. Error Display/Debug excludes synthetic sentinel.
- [x] Run provenance:: store/legacy/traceability tests and retain reader oversize/corruption checks.

## Task 2: Preserve real mutation failure semantics

Files: src/tools/provenance.rs tests, README.md.

- [x] Reproduce real fs_write of an escaped-control payload whose raw bytes fit but full diff JSON exceeds8 MiB; observe missing warning/false-complete behavior RED.
- [x] Assert write succeeds and bytes remain, warning visible, session marked incomplete, no unreadable event published; undo restores original source and remains honest about its own tracking failure.
- [x] No production caller changes unless regression proves needed. Document8 MiB serialized cap and explicit tracking failure, not truncated evidence.

## Verification / publication

- [x] Self-review scope and failure paths before edits; every regression exercises real store/tool behavior.
- [x] Focused provenance::, tools::provenance:: and tools::mutation::; fmt, full locked Rust gate, guidance and required applicable checks.
- [x] Independent review; reproduce/fix material findings and rerun affected tests.
- [ ] Inspect effective URL/refspecs/upstream/push.default/pushRemote without secrets, dry-run full explicit feature refspec, inspect single destination, then identical push; verify remote head/develop unchanged.
- [ ] Create/attach PR against develop as mopemope; verify head/base and exact-head CI to terminal result. No merge.

## Plan self-review

The existing read limit provides the fixed behavior, avoiding a new product setting. Size is checked after JSON escaping through bounded serialization, not raw content length. All event variants share append. Tests include untouched existing evidence and real post-commit failure/undo behavior so rejection cannot silently become false rollback or lost source state. Aggregate acquisition is deliberately not represented as a writer transaction guarantee; history display budgets are not tightened. User has authorized implementation through PR; proceed without another approval gate.

## Validation and review checkpoint

Actual RED: oversized store append succeeded while load_complete failed with the
existing input limit; real fs_write succeeded without tracking warning for a1.5MB
control-character source whose escaped diff JSON exceeded8MiB. Both regressions
passed after bounded serialization. Exact8MiB reader/writer boundary, onebyteover,
large Unicode/hash/link roundtrip, fresh rejection with no directories, existing
bytes preserved/reloaded, persisted session failure count and real undo all pass.
Focused provenance157 and mutation19 passed. Independent whole-branch reviewer
found no concrete issues after checking production caller failure paths and budgets.

Initial full native macOS gate:2384unit passed/2failed, unchanged browser launcher
fixture returned TimedOut instead of Failed and oversized MCP HTTP fixture saw
connection reset. Both passed individually. No assertions/timeouts were changed.
Repeated identical Rust gate with RUST_TEST_THREADS=2 passed2386unit+43integration,
fmt and all-target/all-feature Clippy -D warnings. Initial failure logs are retained;
the precise scheduling/transport cause was not established. No related fix is claimed.
Guidance/Python191 tests also passed before publication. Existing oversized historical evidence is
not rewritten; aggregate/display budgets retain their separate limitations.
