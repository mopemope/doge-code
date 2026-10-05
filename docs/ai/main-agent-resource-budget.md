# Main-agent run-wide resource budget v1

## Goal and non-goals

One run of `run_agent_loop()` enforces iteration, main-dispatch tool-call,
monotonic elapsed, and cumulative token budgets. Budget exhaustion returns
`status="partial"` with a stable `stop_reason`, never a runtime error.
Cancellation stays `LlmErrorKind::Cancelled`; provider/runtime errors stay
errors.

Out of scope: evaluation harness, Responses-native compaction, cache/routing/
reasoning policy changes, sandboxing, pricing/cost tables, session-wide quotas,
subagent policy redesign, and default token tuning (deferred to measured
evaluation). Manual `/compact` and nested `doc_generate` usage are integrated
into persisted session accounting; manual `/compact` stays outside the main-agent
run budget.

## Config semantics

`src/config/agent_budget.rs` owns `AgentBudgetConfig` and
`PartialAgentBudgetConfig` with `#[serde(deny_unknown_fields)]`.

Resolved defaults preserve existing behavior:

```toml
[agent_budget]
max_iterations = 256
# max_tool_calls / max_elapsed_ms / max_total_tokens are None until set.
```

Explicit `0` is a startup error for all four fields; unset and zero stay
distinct. Merge is field-wise default <- user/global <- project, so a partial
project table never erases unrelated user fields.

`FileConfig.agent_budget` and `AppConfig.agent_budget` are wired through
`loading.rs` validation and `app.rs::from_cli()`.

## Accounting semantics

- Baseline: `client.usage_snapshot()` at run start. Run-local deltas only;
  resumed history is never re-charged.
- `charged_tokens` drives policy; `UsageLedger` stays provider-reported only.
  Reasoning and cached subtotals are never added on top of `total_tokens`.
- Main request preflight uses `ContextBudgetGovernor` prompt estimates even
  when `[context_budget] mode="off"`.
- `charge_request(estimate, before, after)`: one record attributable =>
  `max(reported total, estimate)` plus one estimate per extra attempt without
  a record. Zero records => `estimate * max(attempts,1)`. Ambiguous multi-record
  => single estimate (serial contract violated; stay conservative without
  inventing provider usage).
- `charge_internal(estimate, before, after)`: compaction/task/finalization.
  One record => reported plus unknown-attempt estimates. Multi-record =>
  reported sum plus unknown estimates. Zero records with movement =>
  bounded estimate (effective compaction limit for compaction/task, governor
  estimate for finalization). Idle iterations with no ledger movement charge
  nothing.
- Retries without usage are never free. Missing internal usage is never zero;
  callers pass a bounded estimate and the tracker records it as `estimated_tokens`.
- Token budget is a next-operation gate, not a hard billing cap: one in-flight
  request may finish, then tools/further requests stop.

## Safe-boundary semantics

Checks occur only before starting work: loop top (iteration/elapsed/exhausted),
token preflight (estimate vs remaining), tool batch preflight (all-or-none),
per-tool start (elapsed/token for index>0), compaction start, finalization
start. In-flight mutations, processes, and compactions are never dropped for
budget reasons; existing cancellation cleanup is unchanged.

Tool-call budget counts main dispatches only (`task` itself is one; subagent
internals use `SubagentBudgetTracker`). Oversized batches dispatch no prefix.
Every skipped call gets a synthetic `tool` result:

```json
{
  "ok": false,
  "error": {
    "kind": "agent_budget_exhausted",
    "reason": "tool_call_budget",
    "message": "Agent run resource budget was reached before this tool call could be executed."
  },
  "warnings": []
}
```

IDs are preserved, so Chat and Responses (`function_call_output`) projections
both stay paired. Loop-detector skips keep their own error kind.

## Partial finalization

At most one tools-free request with the partial-report prompt. Iteration and
tool-call budgets never block it; token estimate vs remaining and elapsed do.
Cancellation during finalization stays cancelled. Provider errors, empty
content, tool violations, or serialization failures fall back to a bounded
deterministic local message (no new LLM calls or repo-wide scans). Both paths
checkpoint canonical history before return.

## Cancellation distinction

- Budget: `AgentRunStatus::Partial` + `AgentStopReason::*`.
- Cancelled: `LlmErrorKind::Cancelled` error path, never converted.
- Provider/runtime: existing error path, never converted.

## Subagent interaction

`task` counts once for the tool budget. Shared-client ledger deltas from
subagent LLM calls are reconciled via `charge_internal` with the effective
compaction limit as the bounded estimate. Any nested LLM tool using the shared
client is automatically reconciled into the run budget. `SubagentBudgetTracker`
is unchanged; main last-request telemetry restoration is preserved by the
existing `LastRequestGuard`. No budget telemetry is added to the model-visible `task`
result.

## Test matrix

Tracker: exact iteration, all-or-none batch, elapsed safe boundary, token
preflight exact boundary, reported beats estimate, missing uses estimate, retry
not free, saturating counters, disabled limits, internal multi-record.

Integration: iteration partial with finalization counting, batch overflow with
pairing, token-after-response with pairing, task single-count plus usage,
compaction usage preserved, cancellation/provider errors not partial, elapsed
during tool finishes in-flight, mutation preservation, finalization success/
violation/provider-failure/insufficient-tokens/elapsed/cancellation, exec JSON
shape, Responses pairing, accounting invariants, resume with fresh counters.

## Deferred work

- Runtime Matched Evaluation Harness and default token/tool/time tuning.
- Responses-native compaction.
- Full provider-aware cost calculation.

No token-reduction claim is made from this change alone.
