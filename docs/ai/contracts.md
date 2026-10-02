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

## Job Lifecycle

- Do not spawn new user-visible long-running TUI work directly with
  `tokio::spawn` or `std::thread::spawn`. Register it through `JobManager`
  (`src/jobs/`) so cancellation, shutdown and job inspection remain
  consistent.

## Runtime Context

Runtime hints are request-scoped context, not conversation state.
Do not push recent-file/memory hints into durable HistoryManager messages.

## Context Budget / Observation Safety

Preflight context reductions must preserve unseen tool results.
Seen historical results should be offloaded through the Observation Store
before conversation compaction. Never replace an unseen tool result with a
non-recoverable clearing stub merely to satisfy a local token estimate.

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
