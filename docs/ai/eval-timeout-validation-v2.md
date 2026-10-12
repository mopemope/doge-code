# Timeout repair pilot, version 2

This is a controlled dgc workflow evaluation, not a product feature, benchmark
score or proof of daily-development readiness. Keep the original trial, oracle,
transcript, patch and acceptance decision unchanged. A changed oracle is a new
version, never a rewrite of the original passing post-check.

The first pilot executed regression edit → failing test → repair → passing test
→ tracked diff inspection. Its independent post-check passed, but later review
found that converting `10**1000` or `-(10**1000)` leaked `OverflowError` instead
of the required `ValueError`. Testing `1e309` alone does not exercise that path:
the literal is already infinity before conversion. The model's `git diff` also
omitted its untracked new test file; the retained full harness patch included it.

## Reproducible local checks (no inference)

`scripts/timeout_eval_contract.py` is a read-only black-box checker of 33 normal,
boundary and invalid inputs: finite numbers/numeric strings, smallest positive
float, largest finite float, signed integer overflow, underflow, booleans,
nonfinite values and unsupported types. It reports case labels and exception
types without a reference implementation. Unit tests reject the incomplete
repair matching the observed defect and other known-bad implementations:

```bash
python3 -m unittest discover -s scripts/tests -p 'test_timeout_eval_contract.py' -v
python3 scripts/timeout_eval_contract.py /path/to/retained-or-derived-fixture
```

The second command should fail against the incomplete first-trial repair; do
not fix or rerun the agent in that retained trial to turn its result into a pass.
A developer reference used only to validate the oracle is not a model result.

## Preparing a future fresh trial

Copy only `docs/ai/fixtures/timeout-validation-v2/` into a new disposable Git
repository outside the doge-code checkout. It contains the public contract,
AGENTS instructions, initially passing tests and buggy implementation. Do not
copy the oracle, its unit tests, reference fixes or prior trial output into the
agent workspace. Pin the initial fixture commit and the evaluated dgc binary
hash. Keep the controlled oracle outside the worktree; record and compare its
hash before and after the trial. This is file-integrity checking, not an OS
sandbox or a guarantee against an adversarial agent reading external files.

Use the existing [evaluation harness](evaluation.md) with one explicit case.
The prompt must require adding regression tests before the implementation fix,
observing the failing check with `execute_process`, repairing, rerunning the
same command, and reviewing the complete resulting change including new files.
Retain both the public test check and a separate operator-owned post-check:

```json
{
  "name": "timeout-v2-contract",
  "argv": ["python3", "/controlled/doge-code/scripts/timeout_eval_contract.py", "."],
  "timeout_seconds": 30,
  "protected_paths": ["AGENTS.md", "README.md", "test_settings.py", ".gitignore"]
}
```

The argv path is an operator-side verifier, not text supplied in the agent
prompt. Protect original tests/requirements, allow new regressions, retain
`agent.patch` (tracked + untracked), and independently check change scope.
Inspect actual tool-result order and failed/passed outputs; a final model claim
or `run_status=completed` does not establish acceptance. Review new/untracked
files from the full patch or dgc's captured diff, not `git diff` alone.

A future real-model trial needs its own explicit provider/model/trial/request
count and fee approval. Previous reservations/unused requests are not permission
for another trial. Use a fresh manifest/output directory, same-day reviewed
pricing, credential-free explicit config, no MCP, bounded run/tool/time limits,
no retry, the existing guarded supported model and `--keep-workspaces`.
Do not add this live run to CI. Keep measured usage, estimated cost, permanent
reservation upper bound and independently confirmed billing separate.

## Product failure handoff

The product change is `/verify feedback <id>`, which stages a freshly observed
failed selected check on a user-selected live review hunk. The command does not
infer a fix from output or call a model. Diagnostics remain generated evidence,
separate from the user's repair instruction. Existing batch confirmation starts
one repair run and asks for the literal failed command again. Full workspace
preconditions are checked after the write gate and before any model call.

An external operator finding does not magically become a dgc verification
observation. Add its regression to a new working state and explicitly run the
local check through `/verify`; only the new observed failure can be attached.
Historical/restored results and incomplete captures cannot authorize a repair.
Old failure evidence remains historical after edits; inspect a fresh result and
complete diff. The UI does not auto-resolve comments, plans or obligations, and
prompted rerun compliance is not guaranteed. No additional paid trial was run
for this change; local integration tests use real local processes and fixed
localhost provider responses.
