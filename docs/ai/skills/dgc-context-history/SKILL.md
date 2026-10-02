---
name: dgc-context-history
description: Change doge-code (dgc) context budgeting, history compaction, Observation Store recovery, or request-scoped runtime hints.
---

# dgc context and history

Start with `src/llm/context_budget.rs` for estimates/pressure,
`tool_execution/history.rs` for durable conversation changes,
`runtime_context.rs` for bootstrap hints, and `agent_loop.rs` for request wiring.
Read [context contracts](../../../../docs/ai/contracts.md) when changing these boundaries.

- Preserve unseen tool results until a successful provider request has consumed
  them. Seen results may be offloaded only with recoverable observation handles.
- Keep the governor pure with respect to history mutation and keep runtime hints
  request-scoped; do not insert hints into durable HistoryManager messages.
- Include tool schemas and cached prompt tokens in context pressure. Cache
  telemetry is observational; schema ordering stays deterministic.
- Preserve recovery after provider failure, parallel batches, compaction, and
  observation pagination. Do not trade exact recoverability for a lower estimate.

Start with `bash scripts/verify.sh test llm::context_budget::` or
`llm::tool_execution::history::`. Add agent-loop/dispatch tests when wiring changes.
Use existing unseen-result, request-failure, runtime-overlay, and observation-read
regressions; add a new test only for a concrete missing behavior.
