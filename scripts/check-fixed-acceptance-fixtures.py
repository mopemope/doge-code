#!/usr/bin/env python3
"""Offline native positive/negative controls. Never launches models or providers."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time

CASES = Path(__file__).resolve().parents[1] / "docs/ai/repair-cases"


def signature(workspace):
    return {str(p.relative_to(workspace)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in workspace.rglob("*") if p.is_file()}


def run_check(workspace, env, go, minimum, output, name):
    before = signature(workspace)
    started = time.monotonic()
    result = subprocess.run([go, "test", "-v", "-count=1", "./..."], cwd=workspace,
                            env=env, capture_output=True, text=True, timeout=120)
    (output / (name + ".stdout.txt")).write_text(result.stdout)
    (output / (name + ".stderr.txt")).write_text(result.stderr)
    count = len(re.findall(r"(?m)^\s*--- (?:PASS|FAIL):", result.stdout))
    unchanged = before == signature(workspace)
    return {"exit_code": result.returncode, "reported_tests": count,
            "minimum_tests": minimum, "source_unchanged": unchanged,
            "elapsed_seconds": time.monotonic() - started}


def controls(output, go):
    output.mkdir(parents=True)
    home = output / "home"
    home.mkdir()
    env = {key: os.environ[key] for key in ("PATH", "LANG", "LC_ALL") if key in os.environ}
    env.update(HOME=str(home), GOPROXY="off", GOSUMDB="off", GOTOOLCHAIN="local", GOWORK="off")
    version = subprocess.run([go, "version"], env=env, capture_output=True, text=True,
                             timeout=30, check=True).stdout.strip()
    results = []
    for case in json.loads((CASES / "cases.json").read_text()):
        if "reference_fixture" not in case:
            continue
        name = case["case_id"]
        directory = output / name
        directory.mkdir()
        rows = {}
        minimum = case["checks"][0]["min_tests"]
        for variant in ("baseline", "reference"):
            workspace = directory / variant
            shutil.copytree(CASES / case["fixture"], workspace)
            if variant == "reference":
                reference = CASES / case["reference_fixture"]
                files = {str(p.relative_to(reference)) for p in reference.rglob("*") if p.is_file()}
                if files != set(case["allowed_paths"]):
                    raise ValueError("reference must change exactly the declared implementation files")
                for path in case["allowed_paths"]:
                    shutil.copy2(reference / path, workspace / path)
            shutil.copy2(CASES / case["oracle"], workspace / "repair_acceptance_test.go")
            row = run_check(workspace, env, go, minimum, directory, variant)
            expected_exit = 0 if variant == "reference" else 1
            if row["exit_code"] != expected_exit or row["reported_tests"] < minimum or not row["source_unchanged"]:
                raise ValueError(f"{name} {variant} control failed: {row}; inspect {directory}")
            rows[variant] = row
        vet = subprocess.run([go, "vet", "./..."], cwd=directory / "reference", env=env,
                             capture_output=True, text=True, timeout=120)
        (directory / "vet.stderr.txt").write_text(vet.stderr)
        if vet.returncode != 0:
            raise ValueError(f"{name} reference vet failed")
        results.append({"case_id": name, "controls": rows, "vet": "passed",
                        "runtime_interaction": "not exercised" if "interaction" in case else "not applicable"})

    case = next(c for c in json.loads((CASES / "cases.json").read_text()) if c["case_id"] == "go-humanize-ftoa-limits")
    directory = output / "go-humanize-ftoa-limits"
    directory.mkdir()
    rows = {}
    for variant, source in (("reference", "reference.go"), ("rejected", "rejected.go")):
        workspace = directory / variant
        workspace.mkdir()
        (workspace / "go.mod").write_text("module humanize-control\n\ngo 1.23\n")
        shutil.copy2(CASES / "controls/go-humanize" / source, workspace / "ftoa.go")
        shutil.copy2(CASES / case["oracle"], workspace / "repair_acceptance_test.go")
        row = run_check(workspace, env, go, case["checks"][1]["min_tests"], directory, variant)
        expected_exit = 0 if variant == "reference" else 1
        if row["exit_code"] != expected_exit or row["reported_tests"] < row["minimum_tests"] or not row["source_unchanged"]:
            raise ValueError(f"humanize {variant} control failed: {row}")
        rows[variant] = row
    text = (directory / "rejected.stdout.txt").read_text()
    for name in ("all_six_positive", "all_six_negative", "zero_without_rounding_positive", "zero_without_rounding_negative"):
        if "--- FAIL: TestRepairAcceptanceFtoaLimits/" + name not in text:
            raise ValueError("rejected candidate must fail each new counterexample")
    results.append({"case_id": case["case_id"], "oracle_revision": case["oracle_revision"],
                    "controls": rows, "all_four_counterexamples_reject": True})
    report = {"kind": "offline_native_controls", "go": version, "cases_checked": len(results),
              "results": results, "models_started": 0, "api_calls": 0,
              "interpretation": "Fixture/oracle validation only; not model completion or session-resume evidence."}
    (output / "result.json").write_text(json.dumps(report, indent=2) + "\n")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--go", default=shutil.which("go"), help="installed Go binary, not a tool-manager shim")
    parser.add_argument("--output", type=Path, required=True, help="fresh output directory; existing evidence is never overwritten")
    args = parser.parse_args()
    if not args.go or args.output.exists():
        parser.error("Go is required and output must not exist")
    try:
        report = controls(args.output.resolve(), str(Path(args.go).resolve()))
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        parser.exit(2, f"offline fixture controls blocked/failed: {error}\n")
    print(json.dumps({key: report[key] for key in ("kind", "go", "cases_checked", "models_started", "api_calls")}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
