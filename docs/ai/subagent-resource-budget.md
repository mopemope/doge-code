# Subagent resource budget v1

## Acceptance and boundaries

The ephemeral `task` worker remains read-only and its LLM-facing input schema
remains `description` + `prompt`. Administrator/user policy lives in `[subagent]`,
with defaults <- user <- project field-wise precedence. Explicit zero budgets
are rejected during config loading without modifying either source file.

The loop bounds started research requests, dispatched tools, monotonic elapsed
time, cumulative conservative token charges, and each request's context footprint.
The selected model's existing effective compaction limit resolves automatic token
budgets and the request context ceiling; no model tables are duplicated. Main
context optimization being off does not disable worker safety.

No dependency, worker persistence, Observation Store, LLM compaction, pricing,
write capability, or main-agent budget is introduced.

## Implementation boundaries

- `src/config/subagent.rs`: policy/defaults/merge/validation.
- `src/llm/client_core.rs`: provider usage totals + monotonic usage-record count.
- `src/llm/tool_execution/subagent.rs`: governor wiring, safe-boundary orchestration,
  tools-free finalization, typed cancellation/provider errors, telemetry restoration.
- `subagent/budget.rs`: independent policy decisions and saturating counters.
- `subagent/evidence.rs`: bounded tool summaries and file reports, deterministic fallback.
- `dispatch/tools.rs`: completed/partial success output, stable stop reason.

Usage attribution requires exactly one new provider usage record. Charge each
successful request once using max(reported total, local prompt estimate), or use
only the estimate if telemetry is missing/ambiguous. Calibrate the worker-local
governor only from attributable prompt usage. Never invent provider usage or
subtract cached tokens; reasoning already in total is not added twice. Session
usage continues to accumulate; main last-request counters/cache are restored.

Tool-cap overflow skips the whole batch. Skipped calls have synthetic paired
outputs, including Responses projection. Elapsed expiry is checked before new
operations and never drops an active tool solely because its budget expired.
Finalization is at most one request, has no tools, is itself budget-checked and
charged, and does not increment research iterations. Failure, empty output,
unsolicited finalization tool calls, or insufficient remaining resources use the
local ledger. Cancellation remains an error at every phase. Non-context research
provider errors remain errors.

## Reproduction and verification

Before replacing the research loop, HTTP characterization tests reproduced:
context overflow returning an error; 40 started requests reported as 41 iterations.
The original focused tests also passed. These fixtures now assert partial recovery
and exact request counts. The previous tool dispatcher had no batch budget; pure
tracker and HTTP tests now prove that oversized batches execute no prefix.

Focused checks cover subagent requests/Responses pairing, exact iteration and
batch boundaries, provider usage/estimate attribution, token/context preflight,
main telemetry restoration, cancellation, finalization fallback, elapsed safe
boundaries, bounded evidence/files, and config loading/precedence/zero rejection.
Required completion checks are `cargo fmt --all`, `bash scripts/verify.sh rust`,
and `bash scripts/verify.sh guidance`. Verification results are reported with
completion rather than treated as durable proof for later revisions.

## Deferred work

- Runtime matched token/quality evaluation harness.
- Main-agent run-wide resource budget.
- Subagent ephemeral Observation Store / compaction, only if measurements justify it.

No specific token reduction is claimed without matched runtime measurements.

The final macOS full-suite run passed 1,426 tests and failed three existing feature tests:
`frozen_saved_evidence_is_not_reopened_from_mutable_storage` (NotADirectory),
`git_helpers_are_disabled_and_non_utf8_is_not_aliased`, and
`submodules_and_non_utf8_paths_cannot_establish_complete_match` (Illegal byte
sequence). The same failures were reproduced in an unchanged `origin/develop`
archive, with 148 other feature tests passing. These are completion blockers for
the full-suite gate; this change does not alter those modules or test expectations.

## Added regression tests

| Test | Contract |
|---|---|
| `test_exact_iteration_limit` | Only the allowed number of research requests starts; count has no off-by-one. |
| `test_batch_is_all_or_none` | Insufficient remaining tool capacity denies the entire batch. |
| `test_estimate_fallback_and_exact_token_boundary` | Missing usage charges cumulative estimates and prevents further requests. |
| `test_reported_usage_max_without_double_counting` | Charge max(reported, estimated); ambiguous attribution uses estimates. |
| `test_context_boundary_and_saturating_charge` | Context ceiling is inclusive and cumulative charges saturate. |
| `test_elapsed_safe_boundary` | Monotonic elapsed expiry denies new requests and dispatch. |
| `test_provider_context_overflow_fallback` | Provider overflow attempts finalization once; repeated overflow returns partial fallback. |
| `test_exact_iteration_limit_with_finalization` | Finalization is excluded from research iterations and receives no schemas. |
| `test_skipped_batch_is_paired_and_not_dispatched` | No prefix dispatch; all IDs get outputs, including Responses projection. |
| `test_finalization_failure_preserves_evidence` | Failed, empty, or tool-calling finalization retains real file/tool evidence. |
| `test_context_preflight_enforced_with_mode_off` | Oversized requests are never sent, even with main governor optimization off. |
| `test_token_preflight_and_remaining_budget_blocks_next_request` | Insufficient initial token capacity sends no request. |
| `test_cancelled_at_boundary_and_during_research_is_error` | Cancellation is typed error, not partial success. |
| `test_real_provider_errors_remain_errors` | Authentication, permanent provider, network/server, and invalid-response errors propagate. |
| `test_main_telemetry_restoration_and_cached_charge` | Restore main last counters; retain cumulative provider/cache/cache-write/reasoning usage; no cache discount. |
| `test_task_output_completed_and_partial_shape` | Dispatch emits successful completed/partial output with stable fields. |
| `test_usage_missing_does_not_reuse_main_counters` | Missing usage uses request estimate, never stale main counters or fabricated telemetry. |
| `test_auto_token_budget_uses_configured_window_and_model_override` | Automatic budget reuses configured context window and model override. |
| `test_subscription_footprint_uses_existing_projection` | Subscription branch uses the existing Responses governor measurement. |
| `test_elapsed_expiry_after_response_starts_no_tools_or_finalization` | Expired response boundary starts no tools or summary request. |
| `test_cancellation_during_finalization_is_not_partial` | Cancellation during summary generation propagates. |
| `test_structured_file_tracking_all_read_tools` | Read, multi-read, find, text search, and RepoMap use current output paths. |
| `test_files_and_evidence_bounded_unicode_fallback` | Distinct-file count, first-N report, bounded Unicode ledger and fallback sections. |
| `test_defaults_and_auto_budget` | Finite default policy; omitted explicit token value means automatic. |
| `test_field_wise_precedence` | Global values survive unless project overrides that field. |
| `test_zero_config_rejected_even_when_overridden` | Each explicit source rejects all zero budget fields. |
| `test_subagent_zero_rejected_by_config_loaders_without_mutation` | Global/project startup loading rejects zeros without changing files. |
| `test_subagent_global_project_field_wise_loading` | Actual source loaders preserve field-wise global/project precedence. |
| `test_usage_totals_snapshot_counts_records_without_estimates` | Usage snapshots count provider records and preserve cumulative totals. |

## Review corrections

- Parent-loop cancellation can drop the worker future instead of letting it return.
  A Drop guard now restores main last-request counters/cache in either case,
  without rolling back any cumulative provider usage.
- Individual summary/path caps did not bound combined JSON after escaping.
  The task handler now applies the shared 6,000-character serialized budget at
  its source, preserving status, reason and counts. Omitted paths set the file
  truncation flag; summary shortening uses the existing truncation marker.

Both issues were reproduced by failing tests before correction. Added tests:
`test_review_dropped_future_restores_main_telemetry`,
`test_review_task_output_bounded_after_json_escaping`, and
`test_serialized_task_budget_preserves_partial_metadata`.

Review validation: `bash scripts/verify.sh test llm::tool_execution::` passed
160 tests, including the two reproduced failures after correction and the
serialized partial-output metadata regression. `bash scripts/verify.sh rust`
passed format and warning-free Clippy, then reported the same three baseline
failures (1,426 passed, 3 ignored). Guidance and its 28 Python tests passed.
The PR remains draft while the full-suite gate is unresolved.
