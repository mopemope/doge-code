# Offline repair acceptance foundation

The small [runner](../../scripts/repair_acceptance.py) prepares fixed tasks, checks
an externally produced candidate, records an independent operator decision, and
summarizes all planned trials. It never starts a model or agent, calls a provider,
configures credentials, fetches dependencies or adapts Codex event streams.
It complements the existing dgc exec comparison runner; it does not replace it.

Two cases are pinned in [cases.json](repair-cases/cases.json):

| Case | Provenance | Candidate scope | Fixed verification |
|---|---|---|---|
| Rust legacy argument prefix | Actual doge-code pre-fix commit `168db022d32cea702c8ebaee210207860c6f6ce8`; historical fix `a78ff91a5ce7c22fb1d66ecf25654e3ccc176085` | Policy implementation only; existing inline tests preserved | Six independent policy regressions and existing policy tests |
| Go block rounding | Explicitly synthetic, authored for this harness; no historical issue claim | `block.go` only | Ordinary test plus ten normal/boundary/invalid subcases |

The Rust task is a known historical defect in this product's own repository. It
is useful for reproduction, not a representative sample or an unbiased task for
an agent that has already seen the fix. The independent oracle is newly authored
from the known contract; it is not a pre-existing external evaluation. The Go
fixture is an overflow bug with ordinary tests initially passing. Neither case
establishes practical agent usefulness. Additional historical cases and an unseen
roughly six-case cohort still need selection before a live comparison.

## Prepare without model usage

Use installed compiler binaries on PATH, rather than a tool-manager shim that
requires the user's trusted config under a different HOME. Do not change trust or
credential settings to make a check pass. Tool versions/probe failures and host
versions are recorded in checks.json. The full historical Rust tree needs several
GB of build cache; prefer an on-disk output directory when /tmp is a small tmpfs.

Python 3.11+, Git and local objects for the Rust base commit are required. The
source checkout need not be clean: `git archive` exports the exact committed
pre-fix tree, excluding uncommitted work and Git history. No fetch is automatic;
if the object is absent, supply a checkout containing it. Candidate check tools
are Cargo with locally cached locked dependencies and Go 1.23+ with no external
dependencies. Missing tools/cache or compile failures are blocked checks, not
passing tests. `GOPROXY=off`, `GOSUMDB=off`, `GOTOOLCHAIN=local`, Cargo `--offline
--locked`, and an isolated HOME limit ordinary downloads and home-config inheritance.
Existing `CARGO_HOME` is retained for offline dependencies, so its Cargo config and
credentials files remain accessible; this is not credential isolation or network
enforcement. No provider secret environment variables are passed or exported.

```bash
python3 scripts/repair_acceptance.py prepare \
  --source-repo /path/to/doge-code \
  --output /tmp/repair-offline \
  --variant candidate --trials 1
```

Preparation creates one fresh workspace per variant × case × trial. The output
path must not exist; variant labels are path-safe and trial count is 1–10. The
plan, source digests, allowed files, acceptance rubric, commands, deadlines and
oracle and checker hashes are fixed before candidate edits. `task.json` and
`workspace/REPAIR_TASK.txt` contain only the task and permitted scope. Historical
future fix identity and the oracle stay in the operator directory, outside the
candidate workspace. Do not expose the parent evaluation directory or the
reference fix/history to a later evaluated agent. Isolation here is directory
separation, not an OS/network sandbox or authenticated evidence store; use trusted
fixtures/checks and suitable external access controls for untrusted candidates.

For an offline smoke check, edit the candidate workspace manually or apply a
known reference implementation there. Label such trials **offline-replay**, not
model evaluations. The runner does not distribute solution patches or invoke a
replay editor. Save intervention notes even for deterministic reference replays.

## Check and review exactly once

```bash
python3 scripts/repair_acceptance.py check \
  --run /tmp/repair-offline/candidate--go-block-rounding--001
```

The runner detects modified/deleted/new files outside the allowed scope. It copies
the candidate into a disposable verifier workspace and checks that copy against
the captured candidate signature before preserving its patch. It injects the
frozen oracle there, then runs fixed commands serially and rechecks the original
candidate after verification. A changed/mismatched copy is invalidated.
The original candidate is not rewritten by checks. Source changes during a check,
missing reported tests, truncated/incomplete capture, missing tools, nonzero exits
and timeouts cannot pass. Rust summaries and Go parent/subtest lines provide
**reported counts**, a stdout heuristic, not attestation that assertions ran. A
candidate can spoof runner output or exit early; even byte-identical successful
output is not independent proof. Records set `test_execution_attested: false`.
An independent reviewer must inspect the scoped implementation for such bypasses
and confirm the oracle contract, rather than accepting an exit code/count alone.
The Go minimum includes the ordinary test, oracle parent and ten subtests.
Only policy tests are run for the Rust case: full repository tests, Clippy and
other platforms are deliberately outside this task rubric.

`checks.json` retains statuses/exit codes/deadlines/elapsed time and test counts;
`*.stdout.txt`, `*.stderr.txt` and `candidate.patch` retain evidence. A passing
check leaves `accepted` null. Check results cannot be overwritten or silently
rerun: prepare a new experiment/trial after correcting a harness or candidate.
Keep the original failed result and version any changed criteria explicitly.

An operator reviews the patch, check evidence and remaining uncertainty against
`criteria.json`, then writes an evidence file using this shape:

```json
{
  "accepted": false,
  "notes": "Offline replay: boundary regression still fails; no model was run.",
  "execution": {
    "status": "completed",
    "elapsed_seconds": 30,
    "review_seconds": 15,
    "rework_seconds": 0,
    "interventions": [
      {"kind": "manual_edit", "note": "Offline candidate patch application", "seconds": 5}
    ],
    "cost": {"estimated_usd": 0, "confirmed_usd": 0}
  }
}
```

```bash
python3 scripts/repair_acceptance.py review \
  --run /tmp/repair-offline/candidate--go-block-rounding--001 \
  --evidence /tmp/operator-review.json
python3 scripts/repair_acceptance.py summary --output /tmp/repair-offline
```

Execution status is `completed`, `failed`, `timed_out`, `blocked` or `cancelled`;
a completed agent can still yield a rejected patch. Intervention kinds are
`extra_instruction`, `manual_edit`, `restart`, `environment_workaround`. Ordinary
review time is separate from rescue interventions and rework time; record these
as distinct observations, avoid counting the same minutes twice, and include
rejected attempts. Times and intervention notes are operator-reported; unknown times must be null,
not invented or recorded as zero. Missing timing coverage prevents per-accepted
time metrics. Check wall
clock is machine-observed separately. Review requires an explicit boolean and
notes. Summary rechecks the reviewed candidate digest; later edits cannot be
reported as the previously accepted patch. Checker changes require a new plan. `accepted: true` additionally requires all fixed checks passing, completed
execution, and unchanged candidate state. This is an operator assertion, not a
mechanism proving that the reviewer is independent or preventing evidence edits.

Every planned trial remains in the denominator. Unreviewed trials remain visible;
failed/blocked/timed-out/cancelled rows are retained. Per-accepted elapsed/review/
rework values include all reviewed attempts and are null until all planned runs
are reviewed or when no task is accepted. Model estimated and confirmed costs
are distinct: null means unknown, zero must be explicit. Cost per acceptance sums
failed and successful costs and is undefined with no accepted task or incomplete
cost coverage. Offline replays require explicit zero model costs; compute/energy
and human labor are not claimed free. Historical and synthetic review counts are
shown separately; do not present a mixed total as historical task success.

## Later authorized model trials and comparisons

No paid trial is required for this PR. Before a live trial, select unseen cases,
freeze trial count, exact environment/model/provider/access limits, independent
criteria and operator review method; obtain a specific usage/budget approval.
Preparation with `--kind model-evaluation` only labels a new plan; it does not
launch or authorize an agent. Use an external, separately authorized workflow to
work in each prepared workspace, keep transcripts private and retain failures.
The operator evidence then requires an `execution.identity` object with nonempty
`model`, `provider`, `configuration`, `permissions`, and `environment` strings.
Record actual settings and observed/priced cost, not desired settings or a budget
ceiling. Never put credentials/config bodies in identity, notes or artifacts.

For matched-model foundation comparisons, separately verify that both foundations
support the same exact provider/model, task/environment/access/deadlines; otherwise
report unavailable. For ordinary Codex product comparisons, record its usual
settings and treat differing models as a whole-product comparison. Variant labels
alone establish neither kind; summary deliberately labels comparison unclassified.
This runner imports no agent event stream and makes no equivalence or performance
claim. A later adapter/launcher or budget policy needs a separately bounded scope.

Keep original live-pilot artifacts and subsequent review failure unchanged. New
fixtures, offline replays, deterministic harness tests and actual model evaluations
are distinct evidence classes. The current offline reproduction validates the
harness and known contracts only; it is not an agent acceptance result.
