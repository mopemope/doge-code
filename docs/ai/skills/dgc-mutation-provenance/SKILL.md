---
name: dgc-mutation-provenance
description: Change doge-code (dgc) workspace edit tools, undo, durable provenance, requirement attribution, or their persistence formats.
---

# dgc mutation and provenance

Read the affected writer and [mutation/provenance contracts](../../../../docs/ai/contracts.md).
Entry points are `src/tools/mutation.rs`, `src/tools/common.rs::finalize_mutation`,
`src/features/semantic_edit.rs`, and the relevant `src/provenance/` module.

- Generate a candidate from a snapshot, enforce preconditions, commit through the
  shared helper, produce a MutationReceipt, and finalize through FsTools.
  Never update undo, session, or provenance independently from a write tool.
- Retain no-op detection, race rejection, conflict-safe undo, and observable
  failure behavior. A tracking failure must not masquerade as a successful rollback.
- Separate observed directives from interpreted requirements. Attribution is
  per-turn and frozen with the event; later plan changes do not rewrite history.
- Read legacy wire versions through their adapters and write the current version.
  Keep durable state out of rebuildable RepoMap SQLite and legacy action_log.

Cover success, no-op, failure, race, undo, and provenance for new mutation tools.
Start with `bash scripts/verify.sh test tools::mutation::`; add semantic edit,
dispatch, and `provenance::` tests according to affected callers and wire formats.
Assertions should inspect receipts, file state, and event attribution, not just
the existence of a log message.
