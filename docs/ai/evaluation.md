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
   (tracked plus non-ignored untracked files), and `checks/` outputs.
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

## Run record

One JSON object per line with the fields below. Schema v2 records carry
`"schema_version": 2`; records without a version are legacy v1 and still
load. This is a schema illustration, not a measured result; replace every
value with actual telemetry and review.

```json
{"schema_version":2,"case_id":"investigation-only-history","trial":1,"base_commit":"full-starting-commit-sha","model":"exact-model-id","settings":{"provider":"openai-chatgpt","environment_id":"fixed-environment-id","timeout_seconds":900,"cases_digest":"sha256-of-case-file","seed":1},"variant":"baseline","variant_metadata":{"agent_command_fingerprint":"sha256-of-command"},"run_status":"completed","stop_reason":null,"input_tokens":1000,"output_tokens":200,"cached_input_tokens":800,"reasoning_tokens":50,"cache_write_tokens":null,"elapsed_seconds":10,"agent_elapsed_seconds":9,"tool_calls":3,"iterations":2,"request_attempts":2,"usage_records":2,"unknown_usage_attempts":0,"budget_charged_tokens":1200,"budget_provider_reported_tokens":1200,"budget_estimated_tokens":0,"accepted":false,"rework_count":0,"contract_violations":0}
```

Field provenance:

- provider measured: `input_tokens`, `output_tokens`, `cached_input_tokens`,
  `reasoning_tokens`, `cache_write_tokens`, `usage_records`,
  `unknown_usage_attempts` (from `dgc exec --json` usage; null whenever
  provider coverage is incomplete, never zero).
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
and accepted runs with contract violations. Variant metadata (binary and
config hashes) may differ between files; matched settings must be equal.

The fixture set, harness, and comparison utility make evaluation repeatable
but do not demonstrate a savings percentage by themselves. With only a few
trials, report measured sample aggregates without generalizing to claims
such as "30% faster". Expand cases from actual failures and keep human
review aligned with acceptance criteria. See official [evaluation best
practices](https://developers.openai.com/api/docs/guides/evaluation-best-practices).
