# Evaluate agent, runtime, and guidance changes

Use [representative cases](eval-cases.json) to compare variants of the agent,
runtime implementation, prompt/cache behavior, budgets, Skills/guidance, or
model configuration on real development tasks. This evaluates agents
developing doge-code; it is separate from unit tests of the dgc agent
runtime. Cases specify observable acceptance criteria and expected Skills for
reviewing selection. Existing coverage may justify no code change; reward
evidence and task completion, not patch size.

## Matched runs

The [runtime harness](../../scripts/run-agent-evals.py) automates matched
execution:

1. Write an eval manifest (see [example](eval-run.example.json)): fixed
   `base_ref`, case file, trial count, exact model id, provider,
   environment id, timeouts, seed, and one entry per variant with its agent
   command and optional eval config. Relative paths resolve against the
   manifest directory; the example's `/absolute/path/to/...` placeholders
   must be replaced with real binaries and configs before any run.
   Never store API keys, bearer tokens, or other
   credentials in the manifest; authentication is inherited from the
   normal environment. The runner refuses secret-bearing CLI arguments.
2. Validate without spending usage:
   `python3 scripts/run-agent-evals.py --manifest <manifest> --output /tmp/dgc-eval --dry-run`.
   This resolves the base commit once, validates the manifest, cases, and
   variant commands, and prints the deterministic execution matrix.
3. Run live trials explicitly (never from CI):
   `python3 scripts/run-agent-evals.py --manifest <manifest> --output /tmp/dgc-eval`.
   Every variant x case x trial runs serially in a fresh disposable git
   worktree pinned to the same resolved base commit, with a fresh session,
   the same model/provider/environment/timeout, and no `--resume`.
   Variants interleave per case and trial using the manifest seed. Timeouts,
   failures, partial runs, and harness errors are kept as measurements;
   the harness never retries a trial and never discards a run.
4. Inspect artifacts under `<output>/<variant>/runs/<case>/trial-NNN/`:
   `run.json`, `exec.json`, `stdout.txt`, `stderr.txt`, redacted
   `evidence.json`, `git-status.txt`, `diff-stat.txt`, `agent.patch`
   (tracked plus non-ignored untracked files), `case.json`, and `checks/` outputs.
   `resolved-manifest.json` records the pinned base commit and variant
   hashes (never secrets or config bodies).
5. Review each case's acceptance criteria against the resulting diff, tests,
   and transcript. Copy `review-template.jsonl` to `reviewed.jsonl` and fill
   in `accepted`, `rework_count`, `contract_violations`, and `review_notes`.
   Runner output leaves these as null/empty: null means not yet reviewed,
   never rejected. Mark environment-blocked required checks as incomplete,
   not passes. Track misselected/missing Skills, unnecessary edits, and
   contract violations.
6. Compare matched reviewed files with
   `python3 scripts/compare-agent-evals.py /tmp/dgc-eval/baseline/reviewed.jsonl /tmp/dgc-eval/candidate/reviewed.jsonl`.
   The comparison command only reads existing records and never launches
   agents, calls an API, or writes repository state.
7. Interpret in this order: acceptance rate first, then contract
   violations, then failed/timeout/partial rates, then tokens per accepted
   run, seconds per accepted run, and finally tool/rework metrics. A variant
   that only reduces tokens while lowering quality is not an improvement.

Running an external agent may consume paid API usage. Live evaluation is a
manual local command; CI runs only unit tests, guidance checks, and Rust
checks, never real model evals.

## Optional API request budget

For controlled, text-only OpenAI Chat Completions evaluations, add `api_budget`
to the manifest. Ordinary manifests keep their existing behavior. The guard
supports only `openai-compatible` with pinned `gpt-4o-mini-2024-07-18` or
`gpt-4.1-mini-2025-04-14`; subscription Responses, streaming, images, custom
endpoints, multiple choices and unrecognized request fields are refused.

```json
"api_budget": {
  "max_cost_micro_usd": 5000000,
  "max_requests": 128,
  "max_requests_per_run": 32,
  "max_output_tokens": 4096,
  "input_nano_usd_per_token": 150,
  "output_nano_usd_per_token": 600,
  "pricing_reviewed_on": "2026-10-07"
}
```

This example uses the pinned 4o-mini model. Review current provider prices before
running and set `pricing_reviewed_on` to today's UTC date. Rates are explicit
billing ceilings in nano-USD per token, including any applicable uplift; values
below the known Standard floors are rejected. The date is an operator assertion,
not an automatic pricing lookup. The known floors and model limits come from
[4o-mini](https://developers.openai.com/api/docs/models/gpt-4o-mini) and
[4.1-mini](https://developers.openai.com/api/docs/models/gpt-4.1-mini).

The guard forces `service_tier: "default"` upstream and refuses any response
with a missing or different tier, following the [Chat API contract](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create).

Before every upstream call, the guard permanently reserves the model's full
published context times the input rate, plus the output cap times the output
rate. It never discounts cached input or refunds retries, failed requests,
timeouts or small reported usage. For the example, each reservation is
$0.0216576; 128 calls reserve at most $2.7721728. With 4.1-mini's 1,047,576-token
context and 400/1600 nano-USD rates, the same output cap reserves $0.425584 per
call: a $5 ceiling permits at most 11 calls, without promising task completion.
The entire planned request count must fit the ceiling before startup.

Every variant needs an explicit credential-free config with
`llm.max_retries = 0`, no MCP servers and the official OpenAI base URL. Fresh
worktrees containing `.doge/config.toml` are refused. The original
`OPENAI_API_KEY` stays in the parent guard; git hooks and version probes receive
no key, and agents, evidence collection and independent checks receive a random
local token. The harness forces a loopback base URL, so main, worker and
compaction calls share the same global reservation counter. It inserts Chat
Completions `max_completion_tokens` when absent and preserves a valid smaller
explicit output cap. Checks and evidence collection cannot forward API calls.

Any provider error, timeout, malformed or missing usage, unknown response model,
concurrent call or output-limit finish stops subsequent forwarding across the
entire matrix. Already reserved calls retain their upper bound; unreached trials
have explicit `not_run` checks. Existing independent post-checks still run for
available worktrees. `api-budget.json`, resolved manifest and each measurement
record the policy, permanent reservation and stop reason. `actual_cost` stays
null; observed tokens and reserved upper bounds are separate. Budget findings do
not determine human `accepted`, even if the agent reports completion.

This bounds only requests forwarded by this guard at the reviewed billing rates
and provider context/output contract. Use controlled fixtures and configs: it is
not an account-wide cap or a sandbox for arbitrary programs, other credentials,
or other API clients. Dry-run starts no guard and makes no provider request.
CI uses injected fake upstreams and loopback HTTP only.

## Independent post-check evidence

The agent's `run_status`, post-check `verification_status`, and human `accepted`
are separate. A completed agent can fail verification; a Partial or timed-out
agent can leave a change that passes a check. Neither result automatically fills
`accepted`. Required checks still run after agent Partial, failure or timeout
when the worktree is available. Spawn/setup failures leave explicit `not_run`
records with reasons; no declared checks means `not_configured`, not a pass.
On interruption, completed results stay recorded, the interrupted check is
`not_run` with `check_interrupted`, and only later checks remain unreached.

Each `post_checks` entry contains a unique name, an argv array (never a shell
string), and a finite positive timeout. It can also declare `protected_paths`,
a list of project-relative verifier files that must remain unchanged:

```json
{"name":"oracle","argv":["python3","accept.py"],"timeout_seconds":30,"protected_paths":["accept.py","oracle_helpers.py"]}
```

The runner records file hashes and modes before the agent runs and checks them
before and after verification. Changed verifiers invalidate the result even if
the command would exit zero. Missing, unreadable or symlink baselines are
`not_run`, never trusted as a pass. Declare all verifier dependencies that must
stay fixed; leave the implementation under evaluation editable. This is a
local file-integrity guard, not an immutable or hidden test environment. It does
not protect undeclared imports, the interpreter/toolchain, or transient changes
and races between signature checks. Use separately controlled verification for
stronger isolation. Existing checks without `protected_paths` remain supported
and do not gain this integrity guarantee.

Per-check statuses are `passed`, `failed`, `timed_out`, `error`, `not_run`, and
`invalidated`. Exit codes, reasons, timeouts, output truncation and bounded output
artifacts remain available individually when results are mixed. Verification
uses the existing process-group runner and the parent environment, not the
variant's `DOGE_CODE_CONFIG`. `required_checks_passed` is true only when all
checks pass, false for observed failures/timeouts/errors/invalidation, and null
when checks are absent or incomplete without an observed failure. The aggregate
`verification_status` prioritizes invalidated/error/failed/timed_out/not_run;
inspect per-check records for all outcomes.

Each run retains `case.json` (normalized original prompt, acceptance criteria,
commands and protected paths), its `case_sha256`, the pinned base commit,
`agent.patch`, and check outputs. These link the original task to the candidate
change and independent command evidence without asking the agent to grade itself.
The existing seven repository cases are retained; no model benchmarks are added.
Unit regressions use a disposable external Git repository with a small calculation
bug: the reference implementation fails then passes the same oracle, while
weakening that oracle invalidates the result. These fake-agent runs spend no model
usage. Other agents still require the existing dgc-style CLI/telemetry contract;
real Unity projects and external toolchains are not validated by these fixtures.

## Run record

One JSON object per line with the fields below. Schema v2 records carry
`"schema_version": 2`; records without a version are legacy v1 and still
load. This is a schema illustration, not a measured result; replace every
value with actual telemetry and review.

```json
{"schema_version":2,"case_id":"investigation-only-history","trial":1,"base_commit":"full-starting-commit-sha","model":"exact-model-id","settings":{"provider":"openai","environment_id":"fixed-environment-id","timeout_seconds":900,"cases_digest":"sha256-of-case-file","seed":1},"variant":"baseline","variant_metadata":{"agent_command_fingerprint":"sha256-of-command"},"run_status":"completed","stop_reason":null,"input_tokens":1000,"output_tokens":200,"cached_input_tokens":800,"reasoning_tokens":50,"cache_write_tokens":null,"elapsed_seconds":10,"agent_elapsed_seconds":9,"tool_calls":3,"iterations":2,"request_attempts":2,"usage_records":2,"unknown_usage_attempts":0,"cached_usage_records":2,"reasoning_usage_records":2,"cache_write_usage_records":0,"known_cached_input_tokens":800,"known_reasoning_tokens":50,"known_cache_write_tokens":null,"budget_charged_tokens":1200,"budget_provider_reported_tokens":1200,"budget_estimated_tokens":0,"accepted":false,"rework_count":0,"contract_violations":0}
```

Field provenance:

- provider measured: `input_tokens`, `output_tokens`, `cached_input_tokens`,
  `reasoning_tokens`, `cache_write_tokens`, `usage_records`,
  `unknown_usage_attempts` (from `dgc exec --json` usage). Optional totals
  additionally require every usage report to include that item; otherwise they
  are null, while the reported subtotals stay in `known_cached_input_tokens`,
  `known_reasoning_tokens`, and `known_cache_write_tokens`. Per-item
  `cached_usage_records`, `reasoning_usage_records`, and `cache_write_usage_records`
  preserve how many reports contained each metric, including explicit zero.
  Missing optional metrics are unknown, not zero.
- locally measured: `elapsed_seconds` (runner wall clock),
  `agent_elapsed_seconds` (main loop `budget.elapsed_ms`), `tool_calls`
  (main-agent dispatches), `iterations`, `request_attempts`, budget charge
  breakdown, exit codes, digests, diffs, check outputs.
- human reviewed: `accepted`, `rework_count`, `contract_violations`,
  `review_notes`. The runner always emits null/empty here.
- estimated: nothing. Local token estimates are never mixed into provider
  usage; budget estimates stay in `budget_estimated_tokens` only.

Missing metrics are unknown, not zero. Output tokens are exactly the
provider's `completion_tokens`; reasoning tokens are reported separately
and never added twice, and cached tokens stay inside total input tokens.
`tokens_per_accepted_run` and `seconds_per_accepted_run` include effort
spent on failed runs in the numerator. When no run is accepted, or when any
run's token telemetry is unknown, those efficiency metrics are null and the
summary reports `known_total_tokens` instead. Cached tokens remain in total
input tokens; caching can reduce billed cost, but does not remove context
pressure. Cost comparison requires separately recorded provider
pricing/charges; this harness does not infer prices from token counts.

The comparer rejects duplicate runs, different case/trial/commit/model/
settings sets, unreviewed records (null `accepted`/`rework_count`/
`contract_violations`), nonfinite metrics, invalid cached-token counts,
and accepted runs with contract violations. For schema v2, complete token and
cached-token summaries additionally require explicit nonnegative integer
`request_attempts`, `usage_records`, and `unknown_usage_attempts`, with equal
attempt/report counts and zero unknown attempts. Missing, null, or inconsistent
coverage prevents complete totals and token efficiency even when token fields
contain numbers; supplied numeric subtotals remain in `known_total_tokens` and
`known_cached_input_tokens`. These subtotals are not complete-run totals. Invalid
counter types or negative values are rejected. Complete schema-v2 cache totals
also require `cached_usage_records == usage_records`. Older schema-v2 records
without item coverage remain readable: their numeric cache value contributes
only to the known subtotal. New records store item counters and known subtotals
as additive schema-v2 fields; the comparer uses the new known subtotal once,
falling back to the old numeric cache value when the known field is absent/null.
Ordinary input/output totals do not require optional cache/reasoning fields.
Legacy v1 measured records retain
their original comparison behavior. Variant metadata (binary and
config hashes) may differ between files; matched settings must be equal.

The fixture set, harness, and comparison utility make evaluation repeatable
but do not demonstrate a savings percentage by themselves. With only a few
trials, report measured sample aggregates without generalizing to claims
such as "30% faster". Expand cases from actual failures and keep human
review aligned with acceptance criteria. See official [evaluation best
practices](https://developers.openai.com/api/docs/guides/evaluation-best-practices).
