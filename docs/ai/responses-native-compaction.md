# Responses Native Compaction v1

Server-side compaction for the `openai` provider via `POST /responses`
`context_management`.

## Why server-side

- Keep the Sign in with ChatGPT route unchanged (`store:false`, `stream:true`).
- Stay compatible with stateless input-array replay (no `previous_response_id`,
  no `conversation` storage, no WebSocket).
- Avoid converting Responses `provider_state` into Chat Completions text for a
  local summarizer.
- Treat compaction usage as ordinary response usage (single charge).

## Request contract

```json
{
  "model": "...",
  "input": [...],
  "tools": [...],
  "store": false,
  "stream": true,
  "context_management": [
    { "type": "compaction", "compact_threshold": 102400 }
  ]
}
```

- `compact_threshold` reuses `cfg.get_effective_compaction_limit()`
  (`min(auto_compact_prompt_token_threshold, context_window_size * 0.8)`).
- Minimum is 1000 tokens; below-minimum configuration fails startup without
  silent clamping (`RESPONSES_MIN_COMPACT_THRESHOLD`).
- Never send `previous_response_id`, `conversation`, `background`,
  `truncation`.

## Canonical boundary

- `validate_output()` accepts `type == "compaction"` with non-empty
  `encrypted_content` (optional string `id`). Ciphertext is never inspected.
- `ProviderState` stays at `version: 1`; compaction items are ordinary output
  items.
- `latest_compaction_index()` / `canonical_output()` define the boundary:
  from the latest compaction item onward. `completed()` validates, extracts,
  and persists only the canonical slice, so pre-boundary tool calls never
  dispatch.
- `ProviderState::contains_compaction()` /
  `latest_compaction_index()` centralize the scan.

## Session persistence

- `HistoryManager::apply_native_provider_compaction_boundary()` prunes to
  `[authority system?, latest compaction assistant ..]` after the response is
  pushed and before checkpoint. Authority system prompt survives; other old
  messages are subsumed server-side.
- `durable_conversation_messages()` is unchanged (system messages never
  persist); saved sessions resume from compact state.
- Fail-closed: unseen tool results in the discarded prefix refuse pruning.
- Tool pairing validated with `validate_tool_blocks(..., true)` before
  mutation; pending batches remain valid tails.

## Observation Store interaction

- Opaque `encrypted_content` may semantically reference `obs-*` ids that local
  code cannot parse. While canonical history contains a compaction item,
  `gc_unreferenced_observations()` skips the sweep (`observation_gc_skipped =
  "opaque_provider_compaction"`).
- Capacity policy unchanged: bounded store, no eviction, fallback elision.
- Finer-grained pin sets are deferred; correctness first.

## Token accounting

- Single `run_budget.charge_request(...)` per provider response; no
  `charge_internal(...)` for native detection (no double charge).
- Provider `usage` (`input/output/total/cached/reasoning`) recorded as-is;
  no estimated compaction usage is invented.
- Context governor: `reset_after_native_compaction()` (alias
  `reset_calibration()`) drops the pre-compaction sample; the next normal
  response recalibrates from the smaller canonical context.

## Context governor interaction

- `ContextBudgetMode::Auto`: overlay drop and Observation offload still run;
  the local textual compaction step is skipped for subscriptions and the
  request itself carries server-side compaction.
- `Observe`/`Off`: native compaction remains enabled as provider-state
  lifecycle, not local optimization.
- Reactive overflow: no local fallback, no unbounded retry. Explicit error:
  `Responses server-side compaction was enabled but the request still
  exceeded the provider context window. Verify [llm] context_window_size and
  the configured compaction threshold.`

## `/compact` behavior

- `openai`: informational message, no job, no provider request:
  `[INFO] ChatGPT Responses uses automatic native compaction; /compact does
  not run the local text summarizer for this provider.`
- `openai-compatible`: existing local summarization unchanged.

## Failure/recovery

- Cancellation stays `LlmErrorKind::Cancelled`.
- Subscription usage limits, auth failures, unsupported capabilities, network
  errors, and incomplete streams keep existing retry semantics.
- Stream stays bounded by the existing 32 MiB limit; no new unbounded
  buffering.
- Logs carry `responses_native_compaction_enabled`,
  `responses_compact_threshold`, `responses_native_compaction_observed`,
  `history_messages_pruned`, `observation_gc_skipped` — never
  `encrypted_content`, raw output, prompts, tool contents, or credentials.

## Out of scope for v1

- The read-only `task` subagent (`src/llm/tool_execution/subagent.rs`)
  measures with the same threshold but never prunes its ephemeral message
  vector. It is bounded by its own research budget and stops (rather than
  compacts) on provider context overflow. Main-loop pruning covers the
  long-session case; subagent pruning can be revisited if worker transcripts
  grow large in practice.
