#!/usr/bin/env python3
"""Compare measured agent runs on matched cases; this command never calls an LLM."""

import argparse
import json
import math
from pathlib import Path
import sys


def load_runs(path):
    runs = {}
    variants = set()
    for number, line in enumerate(path.read_text().splitlines(), 1):
        if not line.strip():
            continue
        run = json.loads(line)
        if not isinstance(run, dict):
            raise ValueError(f"{path}:{number}: a run must be a JSON object")
        for field in ("case_id", "base_commit", "model", "variant"):
            if not isinstance(run.get(field), str) or not run[field].strip():
                raise ValueError(f"{path}:{number}: missing {field}")
        if not isinstance(run.get("settings"), dict):
            raise ValueError(f"{path}:{number}: settings must describe the run configuration")
        for field in ("trial", "input_tokens", "output_tokens", "cached_input_tokens", "tool_calls", "rework_count", "contract_violations"):
            if type(run.get(field)) is not int or run[field] < 0:
                raise ValueError(f"{path}:{number}: {field} must be a nonnegative measured integer")
        elapsed = run.get("elapsed_seconds")
        if type(elapsed) not in (int, float) or not math.isfinite(elapsed) or elapsed < 0:
            raise ValueError(f"{path}:{number}: elapsed_seconds must be finite and nonnegative")
        if type(run.get("accepted")) is not bool:
            raise ValueError(f"{path}:{number}: accepted must be a reviewed boolean")
        if run["cached_input_tokens"] > run["input_tokens"]:
            raise ValueError(f"{path}:{number}: cached tokens exceed input tokens")
        if run["accepted"] and run["contract_violations"]:
            raise ValueError(f"{path}:{number}: a contract-violating run cannot be accepted")
        key = (run["case_id"], run["trial"], run["base_commit"], run["model"],
               json.dumps(run["settings"], sort_keys=True, allow_nan=False))
        if key in runs:
            raise ValueError(f"{path}:{number}: duplicate case/trial/configuration")
        runs[key] = run
        variants.add(run["variant"])
    if not runs or len(variants) != 1:
        raise ValueError(f"{path}: expected nonempty runs for one guidance variant")
    return runs


def summarize(runs):
    values = list(runs.values())
    accepted = sum(run["accepted"] for run in values)
    tokens = sum(run["input_tokens"] + run["output_tokens"] for run in values)
    seconds = sum(run["elapsed_seconds"] for run in values)
    return {
        "variant": values[0]["variant"], "runs": len(values), "accepted": accepted,
        "acceptance_rate": accepted / len(values), "total_tokens": tokens,
        "cached_input_tokens": sum(run["cached_input_tokens"] for run in values),
        "tokens_per_accepted_run": tokens / accepted if accepted else None,
        "seconds_per_accepted_run": seconds / accepted if accepted else None,
        "tool_calls": sum(run["tool_calls"] for run in values),
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
