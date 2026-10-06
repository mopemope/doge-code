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

Implementation slice: CLI exec completion adds only an exact-session navigation
link; text stdout remains the final answer. TUI `/evidence [id]` explicitly starts
a cancellable foreground read job using the P0 report collector/classifier.
Diff Review captures the session ID with its existing turn-owned capture; `e`
requests that ID and leaves the diff available. The log remains visible beside
the panel. Missing legacy IDs never fall back to the current session. Summary
counts deduplicate IDs per category, preserve failures/unknowns and expose
incomplete comparison. Full report export remains the existing CLI operation
with explicit user destination selection. No automatic report or session writes.
New runtime payload field defaults to absent for old payloads; no durable wire
migration. Sync bounded reads may finish before cancellation is observed; late
successful output is suppressed after cancellation. Saved data is inspected,
so unsaved session changes are not represented. Recheck current files on demand.


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

Implementation slice: v6 records frozen OS family/architecture and an optional
primary-tool numeric version. Probe only the selected root-owned native ELF
system executable, outside the project, with fixed arguments, cleared environment,
empty temporary cwd, 1 KiB retained per stream and 500 ms execution deadline
(or lower configured LLM cap), preserving managed cleanup/cancellation. Original
and additional LLM requests independently pass existing policy. Cargo/Python3/Node
are eligible; Go delegation and wrappers/home shims remain unknown. No raw output,
paths, host/user/env values are persisted. v1–5 stay frozen and are never filled
from the exporting host. JSON schema-v2 adds execution_context; the existing
execution_environment field remains reserved/null. Markdown shows context apart
from outcome. No test counts, dependency versions or correctness/reproducibility
claims. Focused policy/privacy/runner/wire/report tests and full Rust/guidance
checks plus independent review precede the dedicated draft PR.

Remaining boundary: complete toolchain/dependency identities, test counts and
format-specific result adapters are not recorded by P2. Metadata absence never
fabricates evidence, and future context changes require a new explicit format.
Structured test results and human decisions remain separate P3 work.

## P3 / PR 4: Bounded structured results, first format

Implementation slice: explicit `go test -json` stdout, parsed from the existing
managed capture before presentation truncation. Persist counts/status/reason only
in v7, composing frozen v6. No additional execution or artifact-file reads. Unknown
old v1–6 records stay unknown on export. Test/subtest terminal-event counts and
package outcomes are separate from process outcome; no result claims requirement
satisfaction or human approval. Keep frozen change/obligation IDs and selected-file
endpoint/current-code comparisons. Historical counts survive later edits while
comparison becomes stale. Complete zero requires terminal package events.

Bounds: 64 KiB input, 16 KiB lines, 4,096 events, 128 packages, 512 test keys,
256-byte names (transient only). Track interleaved start/run/pause/cont/terminal
lifecycles; unclosed/duplicate/malformed/oversized/truncated/timed-out streams and
outcome contradictions yield unknown, never zero. Go cached output is supported;
benchmark/fuzz/list/wrapper/repetition and other formats remain unsupported.
Acceptance: realistic current/cached Go fixtures, nonzero/failing/skip/zero,
pre-budget capture regression, bounds/malformed cases, explicit v7/old-wire reads,
privacy and unchanged frozen linkage/stale report. Independent review and focused/
full Rust/guidance checks precede dedicated draft PR and final-head CI completion.
No TUI command or layout change in this slice.

## P3 / PR 5: Explicit local reviewer decisions

Implementation slice: evidence exports issue a versioned snapshot token for all
saved session changes/observations and selected current working-tree comparison.
`session review <full-id> <accept|request-changes> --snapshot <token>` requires a
TTY and exact kind confirmation. No agent tool or noninteractive bypass; existing
Diff Review close/rollback never creates a judgment. Store append-only v1 decisions
separately from frozen provenance v1–7. Preserve command outcomes/unknowns. The
judgment is explicit operator input, not identity authentication, correctness,
GitHub approval or merge authority.

Target identity includes bounded saved input byte hashes, canonical change/command/
plan/requirement/obligation state, selected file hashes, Git comparison and current
execution-file hashes. Export time, content opt-in and review history are excluded.
No complete workspace/comparison or no recorded changes means no recording token;
--base exports are read-only. Different input is stale; unavailable target or
invalid/unsupported judgment data is unknown. Index contents are outside the
working-tree judgment scope.

Latest identical kind/target is idempotent; different kind appends with an explicit
supersedes ID. Derive history from the chain, not clock order. Session lease guards
publication; close the owning CLI/TUI before recording. Held Unix directories,
no-clobber publication and file/directory sync protect writes. Failures after
publication report durability uncertainty. Every export rechecks target identity;
this is not an atomic workspace approval. No identities/comments/external sends.

Acceptance: cancellation/EOF/noninteractive rejection, repeated operation and
accept→request-changes→accept, alternate session, saved-payload/plan/staging/file
changes, stale/unknown/old forms, clock reversal, unsafe storage, publication and
post-publication sync failures. Independent review, full Rust/guidance checks,
manual recorded CLI operation, dedicated draft PR and exact-head CI completion.

## P3 / later separate PRs: Other formats

Exit status and reported test events are not coverage measurements or review
acceptance. Further adapters require format-specific trust/bounds/compatibility
design. Human reviewer decisions are covered by the explicit local judgment slice above. Never parse arbitrary output into correctness or treat command success
as human approval. These follow-ups are not included in the first adapter PR.

## Follow-up: Connect the Go adapter to the regular test workflow

The first adapter was reached only by explicit execute_process requests; the
regular Go `/test` still selected verbose text. Run Go `/test` with `-json`, parse
its bounded raw capture before display budgets, and carry the typed result to
the shared observation writer. Preserve raw-capture digest/content semantics.
Decode Output events for human diagnostics and match failed-test information by
(Package, Test), including messages emitted before terminal failure. Counts may
be unknown while compiler/test diagnostics remain usable. Keep legacy/custom Go
verbose parsing, other languages, frozen links, timeout/cancel and deferred
failure handoff behavior. No new result format, schema migration or judgment.
Acceptance: readable pass/fail/build failure, parallel/subtest attribution,
known zero versus package failure, cache, pre-display count retention, capture
warnings/timeout unknown, cancelled runs unrecorded, writer failure visibility.
Focused/full Rust/guidance, independent review and TUI terminal recording.
