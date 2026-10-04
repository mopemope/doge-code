# Development contracts

These contracts apply to dgc implementations. For LLM-facing tool output, use [the output contract](../tool-output-contract.md).

## Mutation tools

Workspace text mutations performed by dgc tools must produce a `MutationReceipt` via the shared
commit helper (`src/tools/mutation.rs`). Never update undo/session/provenance
independently from a write tool — use `FsTools::finalize_mutation` after a
successful commit.

New workspace mutation tools must implement: candidate generation, shared
commit helper, `MutationReceipt`, `finalize_mutation`, and regression tests
(success / no-op / failure / race / undo / provenance). This prevents future
tracking gaps.

Mutation targets retain both the requested spelling and the authorized canonical
path. Snapshot, commit, receipt and rollback use the canonical path. Revalidate
the original spelling, authorized root, object identity and contents immediately
before publication. Shared writers reject unresolved symlinks and non-regular
files; new files use no-clobber publication. Rollback must compare its saved
canonical path before restoring or deleting. These are optimistic checks, not
filesystem compare-and-swap: final check-to-rename/unlink races remain possible.

## Job Lifecycle

- Do not spawn new user-visible long-running TUI work directly with
  `tokio::spawn` or `std::thread::spawn`. Register it through `JobManager`
  (`src/jobs/`) so cancellation, shutdown and job inspection remain
  consistent.

## Runtime Context

Runtime hints are request-scoped context, not conversation state.
Do not push recent-file/memory hints into durable HistoryManager messages.

Each assistant tool-call batch has exactly one matching result per call before
the next non-tool message. Queue non-tool interventions until the batch closes.
Checkpoint real or skipped results individually; only the final interrupted
batch may receive unknown results in its durable projection. Validate per batch,
not by global ID lookup. Never reorder arbitrary user messages to repair legacy
history; reject ambiguous blocks before inference or new directive persistence.

## Context Budget / Observation Safety

Preflight context reductions must preserve unseen tool results.
Seen historical results should be offloaded through the Observation Store
before conversation compaction. Never replace an unseen tool result with a
non-recoverable clearing stub merely to satisfy a local token estimate.

Observation GC is reachability-based, never capacity eviction. After
successful compaction and after Observation Store restore, retain only the
observations still referenced by canonical messages (every role's content,
`tool_calls[*].function.arguments`, and `provider_state.output` string leaves,
matched against known store ids). Observation content is never a GC root,
unknown `obs-*` strings never enter the live set, and `unseen_tool_results`
is untouched. Never rewind `next_id`, never reuse removed ids, and never
evict reachable entries for capacity. Empty snapshots are valid state changes
and must be persisted so GC'd entries do not resurrect after restart.

## Prompt Cache Telemetry

Prompt-cache metrics are observational only.
Cached prompt tokens still count toward context-window pressure.
Do not subtract cached tokens from context-budget calculations.

Keep tool definition ordering deterministic; tool/schema changes are
cache-relevant request-prefix changes.

## Provenance rule

Never treat an inferred requirement as a verbatim user directive.
Observed directives and interpreted requirements are distinct provenance nodes.

## Execution and MCP boundaries

- Execution changes belong in `src/execution/` (`runner.rs` / `policy.rs` / `process.rs` / `lifecycle.rs` / `output.rs`); `runner.rs` is policy-free process mechanics, while `process.rs` is the ExecutionPolicy-aware LLM adapter. `FsTools` keeps only thin `execute_process` / `execute_bash` / `execute_shell` adapters. Never route `execute_process` through `bash -c`, join args into a shell string, or prefix-match `allowed_programs`.
- Do not create new ad-hoc `Command::output()` / `wait_with_output()` paths for finite background commands. Use `src/execution/runner.rs` unless the process is intentionally long-lived or interactive (PTY, MCP transport, daemon/service, or another documented exception).
- Do not infer remote MCP tool success from transport success. For a completed call, `CallToolResult.is_error` is authoritative: `Some(true)` maps to `ToolOutput.is_success = false`; `Some(false)` and `None` map to success. Protocol/transport errors remain typed errors.
- Never log remote MCP arguments, full results, environment values, or credentials. Structured stdio uses `command` + argv and never a shell parser.
