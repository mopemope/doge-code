# Harness efficiency investigation, 2026-11-06

## Intent and boundaries

Improve token efficiency while preserving the ability to finish a tested Go/Rust
repair. This is an authorized runtime improvement, not a provider migration.
Existing untracked October audit/plan files are outside this change.
Baseline: `a9abf28`. Reviewed the main request/dispatch loop, request validation,
context accounting, history/offload contracts, tool catalog, progress observers,
loop/stall detectors, verification feedback and evaluation protocol.

## Investigation and alternatives

| Boundary inspected | Finding | Decision |
|---|---|---|
| Context governor / history | Counts schemas and cached tokens; protects unseen results; recoverable offload precedes compaction | Keep these safety boundaries |
| Read-result deduplication | Exact result matching exists, but argument strings must be byte-identical | Compare parsed JSON arguments, preserving exact result equality |
| Request recovery / agent loop | Tool argument correction has a two-correction limit; malformed response recovery only has the general run budget | Bound both with a shared run-local correction allowance |
| Recovery history | Generated correction feedback uses user role, so compaction preserves it as an observed directive | Store explicitly labelled assistant observations |
| Loop detector / task sentinel | Existing repetition/stall detection and plan no-op blocking | Do not add overlapping heuristics without task evidence |
| Verification / evaluation | Structured test observations distinguish zero/unknown tests; practical model comparisons remain unmeasured | Use real CLI regressions; do not claim model acceptance or token savings |

More aggressive compaction risks losing task evidence; longer prompts add recurring
cost without a demonstrated failure-specific benefit. Selected approach: remove
avoidable repetition and bound failed recovery, with no new dependencies or tools.

Official references consulted on 2026-11-06:

- [Effective context engineering](https://www.anthropic.com/research/effective-context-engineering-for-ai-agents): keep high-value context with bounded tools.
- [Effective harnesses for long-running agents](https://www.anthropic.com/research/effective-harnesses-for-long-running-agents): incremental work and verification beyond completion claims.

## Acceptance and implementation sequence

1. Reproduce deduplication misses for reordered/whitespace JSON. Compare parsed
   values only; different values/types, array order and changed output do not match.
   Do not normalize paths, supply defaults, skip tool execution or mutate history.
2. Reproduce malformed-response retry growth using the real CLI and a loopback
   provider fixture. Allow at most two correction follow-ups across argument and
   response errors per run, including alternating errors. Preserve usage charging,
   unseen results and successful correction followed by an actual workspace write.
3. Generated feedback is not a user directive and contains no raw provider body.
4. Run focused history and CLI tests, then fmt, full Rust gate and guidance gate.

## Verification record

- Before implementation: `bash scripts/verify.sh test dedupe_matches_equivalent_json_arguments`
  failed because equivalent JSON produced no pointer.
- Before implementation: `bash scripts/verify.sh test response_correction_real_cli`
  failed because malformed responses reached a fourth request and generated
  feedback had user role.
- After implementation: `bash scripts/verify.sh test correction`: 9 passed.
- After implementation: `bash scripts/verify.sh test llm::tool_execution::history::`:
  72 passed, including offload/compaction/unseen-result preservation.
- `bash scripts/verify.sh test equivalent_read_arguments_real_cli`: passed. Two
  real reads still execute; the next provider request retains the original
  result and uses a pointer below one fifth of its byte length for the repeat.
  The initial fixture assertion incorrectly included a final newline; corrected
  the expectation to the existing ranged-read contract, without a runtime change.
- `bash scripts/verify.sh rust`: fmt and warning-free Clippy passed; full tests
  initially had 2,394 passes and two failures in
  `utils::tests::test_get_git_repository_root_without_git` and
  `utils::tests::test_is_git_repository_without_git_dir`. The sandbox exposes
  `/tmp/.git`, contradicting those existing tests' assumption; the first failure
  reproduced alone. No unrelated test or production code was changed.
- `env TMPDIR=/var/tmp bash scripts/verify.sh rust` (approved outside sandbox):
  fmt, warning-free Clippy and all 2,442 tests passed, including 30 real CLI tests.
  Logs: `/var/tmp/dgc-verify-h0lcwl1p`.
- `bash scripts/verify.sh guidance`: all three static checks and 191 Python
  tests passed. Logs: `/tmp/dgc-verify-k4900w68`.

No paid/live model trials are included; local byte/request measurements are
mechanism evidence, not measured end-to-end token savings or repair success.

## Result and next evidence needed

The changes are confined to `src/llm/tool_execution/history.rs` (JSON equality
for repeated reads), `src/llm/tool_execution/agent_loop.rs` (bounded correction
and observation roles), regression tests and documentation. Error-kind changes
cannot replenish the shared allowance. Unknown-tool/identity failures still do
not enter correction; no invalid batch prefix runs. Failed requests still count
toward the existing budget. A recoverable response error preserves the preceding
read result and permits a subsequent real write, covered through CLI requests.

Practical effectiveness remains an open measurement:

1. Freeze a matched Go/Rust repair cohort using the existing evaluation protocol.
   Compare accepted patches first, then all-trial tokens per accepted patch and
   human interventions, keeping failed/partial trials in the denominator.
2. Use existing `AgentProgressUsage` and request-usage accounting to distinguish
   repeated reads, delayed first edits, delayed verification and failed requests.
   A compact pointer saves bytes but neither avoids a tool execution nor proves a
   provider token/cache improvement.
3. Inspect task evidence before tuning stall/repetition thresholds or increasing
   compression. Current stall heuristics count read/search activity without
   semantic knowledge of whether it advanced a difficult investigation.

Live trials need an explicit usage budget as required by `evaluation.md`. No TUI,
dependency or MSRV changes were made; this Linux run does not supply macOS evidence.

## Review correction: provider prefill compatibility

The original change stored correction feedback as assistant observations and
also sent them unchanged. Review identified a protocol regression: OpenRouter
treats a trailing assistant message as prefill, which Claude 4.6 and later reject
with HTTP 400. Confirmed against the official
[OpenRouter prefill contract](https://openrouter.ai/docs/api_reference/overview#assistant-prefill)
and [Claude validation errors](https://platform.claude.com/docs/en/api/errors#prefill-not-supported).

The CLI fixtures now reject assistant tails with HTTP 400. Before the fix, both
argument and response correction failed at that request (`test correction_real_cli`,
two failures and one pass). `RequestMessages` now projects labelled, plain
correction observations to user role in the provider-bound copy. Canonical history
retains assistant role for persistence, directive extraction and compaction.
Measurement and all request paths already share this projection, including
tools-free budget finalization; repeated projection does not alter it further.
Messages containing tool calls, reasoning or provider state are excluded.

Regression coverage includes the actual OpenRouter request adapter and decoder
against a loopback endpoint, requests with/without tools, transient runtime
context, untouched canonical history, tool pairing, persisted observation roles,
unseen evidence and a real CLI write after correction. Production endpoint
validation remains unchanged. No live API calls are used.

Review-fix verification:

- `bash scripts/verify.sh test correction`: 12 passed, including both CLI
  recovery paths and the actual OpenRouter adapter/decoder fixture.
- `env TMPDIR=/var/tmp bash scripts/verify.sh rust`: fmt, warning-free Clippy
  and all 2,445 tests passed. Logs: `/var/tmp/dgc-verify-2twahgxg`.
- `bash scripts/verify.sh guidance`: all three static checks and 191 Python
  tests passed. Logs: `/tmp/dgc-verify-5_e720nu`.
