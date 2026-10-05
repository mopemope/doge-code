#!/usr/bin/env python3
"""Compare measured agent runs on matched cases; this command never calls an LLM."""

import argparse
import json
import math
from pathlib import Path
import sys

RUN_STATUSES = (
    "completed",
    "partial",
    "failed",
    "timed_out",
    "cancelled",
    "harness_error",
)


def _is_legacy(run):
    return run.get("schema_version") is None


def _optional_nonnegative_int(value):
    return value is None or (type(value) is int and value >= 0)


def load_runs(path):
    runs = {}
    variants = set()
    for number, line in enumerate(path.read_text().splitlines(), 1):
        if not line.strip():
            continue
        run = json.loads(line)
        where = f"{path}:{number}"
        if not isinstance(run, dict):
            raise ValueError(f"{where}: a run must be a JSON object")
        for field in ("case_id", "base_commit", "model", "variant"):
            if not isinstance(run.get(field), str) or not run[field].strip():
                raise ValueError(f"{where}: missing {field}")
        if not isinstance(run.get("settings"), dict):
            raise ValueError(f"{where}: settings must describe the run configuration")
        if run.get("accepted") is None:
            raise ValueError(f"{where}: run has not been reviewed (accepted is null)")
        if type(run.get("accepted")) is not bool:
            raise ValueError(f"{where}: accepted must be a reviewed boolean")
        for field in ("rework_count", "contract_violations"):
            if type(run.get(field)) is not int or run[field] < 0:
                raise ValueError(
                    f"{where}: {field} must be a reviewed nonnegative integer"
                )
        if run["accepted"] and run["contract_violations"]:
            raise ValueError(
                f"{where}: a contract-violating run cannot be accepted"
            )
        legacy = _is_legacy(run)
        if legacy:
            for field in ("trial", "input_tokens", "output_tokens",
                          "cached_input_tokens", "tool_calls"):
                if type(run.get(field)) is not int or run[field] < 0:
                    raise ValueError(
                        f"{where}: {field} must be a nonnegative measured integer"
                    )
            if run["cached_input_tokens"] > run["input_tokens"]:
                raise ValueError(f"{where}: cached tokens exceed input tokens")
        else:
            if run.get("schema_version") != 2:
                raise ValueError(
                    f"{where}: unsupported schema_version "
                    f"{run.get('schema_version')!r}"
                )
            if type(run.get("trial")) is not int or run["trial"] < 1:
                raise ValueError(f"{where}: trial must be a positive integer")
            for field in ("input_tokens", "output_tokens", "cached_input_tokens"):
                value = run.get(field)
                if value is not None and (type(value) is not int or value < 0):
                    raise ValueError(
                        f"{where}: {field} must be a nonnegative integer or null "
                        "(unknown telemetry is never zero)"
                    )
            if (
                run.get("input_tokens") is not None
                and run.get("cached_input_tokens") is not None
                and run["cached_input_tokens"] > run["input_tokens"]
            ):
                raise ValueError(f"{where}: cached tokens exceed input tokens")
            if not _optional_nonnegative_int(run.get("tool_calls")):
                raise ValueError(
                    f"{where}: tool_calls must be a nonnegative integer or null"
                )
            status = run.get("run_status")
            if status is None:
                raise ValueError(f"{where}: schema v2 run_status is required")
            if status not in RUN_STATUSES:
                raise ValueError(
                    f"{where}: run_status must be one of {list(RUN_STATUSES)}"
                )
        elapsed = run.get("elapsed_seconds")
        if type(elapsed) not in (int, float) or not math.isfinite(elapsed) or elapsed < 0:
            raise ValueError(f"{where}: elapsed_seconds must be finite and nonnegative")
        key = (run["case_id"], run["trial"], run["base_commit"], run["model"],
               json.dumps(run["settings"], sort_keys=True, allow_nan=False))
        if key in runs:
            raise ValueError(f"{where}: duplicate case/trial/configuration")
        runs[key] = run
        variants.add(run["variant"])
    if not runs or len(variants) != 1:
        raise ValueError(f"{path}: expected nonempty runs for one guidance variant")
    return runs


def summarize(runs):
    values = list(runs.values())
    accepted = sum(1 for run in values if run["accepted"])
    seconds = sum(run["elapsed_seconds"] for run in values)
    status_counts = {f"{status}_runs": 0 for status in RUN_STATUSES}
    unknown_status = 0
    for run in values:
        status = run.get("run_status")
        if status is None:
            # Legacy v1 records predate run_status; they described finished
            # runs, so they count as completed for status coverage.
            status_counts["completed_runs"] += 1
        elif f"{status}_runs" in status_counts:
            status_counts[f"{status}_runs"] += 1
        else:  # pragma: no cover - validated in load_runs
            unknown_status += 1

    token_complete = [
        run for run in values
        if type(run.get("input_tokens")) is int
        and type(run.get("output_tokens")) is int
    ]
    token_metrics_complete = len(token_complete) == len(values)
    known_total = sum(run["input_tokens"] + run["output_tokens"] for run in token_complete)
    total_tokens = known_total if token_metrics_complete else None

    cached_known = [
        run for run in values if type(run.get("cached_input_tokens")) is int
    ]
    cached_complete = len(cached_known) == len(values)
    known_cached = sum(run["cached_input_tokens"] for run in cached_known)

    tool_known = [run for run in values if type(run.get("tool_calls")) is int]
    tool_complete = len(tool_known) == len(values)
    known_tools = sum(run["tool_calls"] for run in tool_known)

    return {
        "variant": values[0]["variant"], "runs": len(values), "accepted": accepted,
        "acceptance_rate": accepted / len(values),
        **status_counts,
        "token_metrics_complete": token_metrics_complete,
        "token_complete_runs": len(token_complete),
        "total_tokens": total_tokens,
        "known_total_tokens": known_total,
        "tokens_per_accepted_run": total_tokens / accepted
        if (accepted and total_tokens is not None) else None,
        "cached_token_complete_runs": len(cached_known),
        "cached_input_tokens": known_cached if cached_complete else None,
        "known_cached_input_tokens": known_cached,
        "seconds_per_accepted_run": seconds / accepted if accepted else None,
        "tool_call_complete_runs": len(tool_known),
        "tool_calls": known_tools if tool_complete else None,
        "known_tool_calls": known_tools,
        "rework_count": sum(run["rework_count"] for run in values),
        "contract_violations": sum(run["contract_violations"] for run in values),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    args = parser.parse_args()
    try:
        baseline, candidate = load_runs(args.baseline), load_runs(args.candidate)
        if baseline.keys() != candidate.keys():
            raise ValueError("Runs must match case, trial, starting commit, model, and settings")
        if next(iter(baseline.values()))["variant"] == next(iter(candidate.values()))["variant"]:
            raise ValueError("Use distinct baseline/candidate variant names")
        print(json.dumps({"baseline": summarize(baseline), "candidate": summarize(candidate)}, indent=2, allow_nan=False))
    except (OSError, ValueError, TypeError) as error:
        print(f"Evaluation comparison failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
