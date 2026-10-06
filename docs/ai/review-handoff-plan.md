# Reviewable handoff implementation plan

Purpose: let a reviewer start from the current changes, find the recorded intent
and command observations, and see missing or stale evidence before approval.
Passing commands never prove correctness or requirement satisfaction. This plan
tracks repository behavior, not a fixed commit; recheck the relevant code before
each follow-up PR.

## Existing behavior and boundary

`src/features/evidence_report/` already exports bounded, read-only Markdown/JSON
from frozen session/provenance inputs and selected workspace files. Requirements,
plan obligations, change lifecycle, command outcomes and execution/current-code
comparisons already exist. `src/diff_review.rs` presents provenance in the TUI.
`src/features/verification_snapshot/` records selected-file endpoints; equality
cannot prove unchanged inputs throughout a run. Legacy commands have no endpoints.
Keep these distinctions and all existing historical records. Do not rerun tests
on export or introduce a ready/approved boolean.

## P0 / PR 1: Join changes and observations for review

Gap: separate change and command inventories require reviewers to reconstruct
explicit links, and historical successful counts can be mistaken for current
confirmation. Add a derived `review_handoff` array to the schema-v2 report and a
Markdown matrix at the beginning of the report. For each recorded change, display
recorded requirement/plan links, lifecycle/file correspondence and explicit
verification IDs in three groups: matching successes, all failures, other
successes. Matching requires active lifecycle, matching recorded file, stable
execution endpoints and current selected files matching the execution start.
No timestamp-based supersession, inferred file ownership, or inferred requirement
satisfaction. Preserve failures even when a later success exists. Unlinked
commands, unknowns, warnings and unattributed files retain their original sections.

Files: evidence-report model, collection, a derived join module, renderer and
regression tests; README explains the review interpretation. Index existing IDs
rather than scanning all commands for every change. Keep input/output limits and
Markdown escaping. No new content capture, probes, persistence writes or wire
migration. JSON is additive within report schema v2; existing fields and durable
provenance wire shapes remain unchanged. Consumers must tolerate additive fields.

Acceptance: explicit links survive; legacy, stale, changed-during-run and inactive
changes cannot yield matching successes; failures remain beside successes;
empty/unlinked histories do not imply success; duplicate observed IDs do not
duplicate a command in a row. Regression tests cover these cases and escaped
Markdown. Run focused evidence tests and the required full Rust/guidance checks.
Independent review precedes a dedicated-branch draft PR. This task has no other
implementation dependency.

## P1 / PR 2: Bring the handoff into the review workflow

Gap: reviewers must invoke the export themselves and find the relevant session.
Investigate CLI completion and Diff Review entry points; propose a session-specific
summary and discoverable export action using the P0 derived view. Preserve
read-only behavior, explicit destination selection, cancellation and existing
completion output. No automatic overwrite or external messaging. Files: CLI/TUI
completion handlers, diff review, report API and README. No event migration.
Acceptance: correct session selected, missing/incomplete evidence visible,
consistent matrix semantics, no export on unrelated operations. Tests cover
session selection, unavailable evidence and cancellation; a terminal recording
is required for TUI changes. Depends on P0. Keep UI integration in its own PR and
resolve concrete visible-behavior decisions before implementation.

## P2 / PR 3: Record bounded execution context

Gap: current reports explicitly mark toolchain, test counts and environment as
unrecorded. Design an allowlisted execution-context shape for managed checks
before changing the durable wire. Consider OS family/architecture and explicit
tool versions only; exclude host/user names, home paths, environment variables,
credentials and remote URLs. Bounded probes must use the managed runner and its
policy/deadline/cancellation; no paid calls. Files: execution observation writer,
provenance wire adapters, snapshot/report models and documentation. Requires an
explicit versioned adapter; legacy context remains unknown rather than being
reconstructed from the exporting machine. Acceptance: bounds and cancellation,
privacy fixtures, old/new wire loading, no fabricated environment/test counts.
Independent wire/privacy review and focused/full Rust checks required. Separate
PR after P0; context scope remains a product decision.

## P3 / separate future PRs: Structured results and reviewer decisions

Gap: exit status is not an authoritative test count, coverage measurement or
review acceptance. Investigate format-specific result adapters and a separate
explicit human review decision record. Never parse arbitrary stdout into a
correctness claim or treat a command success as human approval. Plan wire
migration, bounded artifact paths and trust boundaries before implementation.
Acceptance includes malformed/truncated results, legacy unknowns, stale decisions
and distinct observed outcome versus human verdict. Dependencies: P0; P2 only
where environment identity is necessary. Split adapters and review decisions;
this plan does not authorize a broad verifier or autonomous approval system.
