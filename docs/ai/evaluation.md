# Evaluate agent guidance changes

Use [representative cases](eval-cases.json) to compare guidance variants on real
development tasks. This evaluates agents developing doge-code; it is separate
from unit tests of the dgc agent runtime. Cases specify observable acceptance
criteria and expected Skills for reviewing selection. Existing coverage may
justify no code change; reward evidence and task completion, not patch size.

## Matched runs

1. Choose a fixed repository commit and create disposable checkouts outside the
   working repository. Apply only the intended guidance variant to each checkout.
   Keep compiler/dependency state, environment, enabled tools, permissions, model,
   reasoning settings, and starting conversation equivalent.
2. Run the same selected cases and trial numbers for both variants. Reset the
   workspace and conversation between runs. Record all failures and timeouts;
   do not discard unsuccessful trials. Use multiple trials for stochastic agents.
3. Review each case's acceptance criteria against the resulting diff, tests, and
   transcript. Mark environment-blocked required checks as incomplete, not passes.
   Track misselected/missing Skills, unnecessary edits, and contract violations.
4. Record measured telemetry from the provider/agent, including all workers and
   compaction calls. Missing metrics are unknown, not zero. Output tokens include
   reasoning when the provider reports it in output; do not count it twice.
   Protect credentials and redact sensitive transcripts; retain only needed evidence.
5. Compare matched run JSONL files with
   `python3 scripts/compare-agent-evals.py /tmp/baseline.jsonl /tmp/candidate.jsonl`.

Running an external agent may consume paid API usage. The comparison command
only reads existing records and never launches agents, calls an API, or writes
repository state. Select and authorize real runs through the host's normal
execution permissions; a guidance check is not a live evaluation.

## Run record

One JSON object per line with the fields below. This is a schema illustration,
not a measured result; replace every value with actual telemetry and review.

```json
{"case_id":"investigation-only-history","trial":1,"base_commit":"full-starting-commit-sha","model":"exact-model-id","settings":{"reasoning":"chosen-effort","tools":"fixed-tool-profile","environment":"fixed-environment-id"},"variant":"baseline","accepted":false,"input_tokens":1000,"output_tokens":200,"cached_input_tokens":0,"elapsed_seconds":10,"tool_calls":3,"rework_count":0,"contract_violations":0}
```

Optional `notes` or evidence paths can explain review decisions. Never invent
usage values to satisfy the record format. The comparer rejects duplicate runs,
different case/trial/configuration sets, nonfinite metrics, invalid cached-token
counts, and accepted runs with contract violations.

`tokens_per_accepted_run` and `seconds_per_accepted_run` include effort spent on
failed runs in the numerator. When no run is accepted, those metrics are null.
Review acceptance rate and violations before interpreting efficiency gains.
Cached tokens remain in total input tokens; caching can reduce billed cost, but
does not remove context pressure. Cost comparison requires separately recorded
provider pricing/charges; this script does not infer prices from token counts.

The fixture set and comparison utility make evaluation repeatable but do not
demonstrate a savings percentage by themselves. Expand cases from actual failures
and keep human review aligned with acceptance criteria. See official [evaluation
best practices](https://developers.openai.com/api/docs/guides/evaluation-best-practices).
