#!/usr/bin/env python3
"""Offline repair preparation, independent checks and operator-reviewed accounting.

No agent/model launcher, provider adapter or credential management. Trusted local
fixtures/checks are executed; disposable directories are not an OS sandbox.
"""
import argparse
import difflib
import hashlib
import importlib.util
import io
import json
import math
import os
import platform
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
CASES = ROOT / "docs/ai/repair-cases"
# Reuse bounded output capture and process-group cleanup from the existing runner.
sys.path.insert(0, str(ROOT / "scripts"))
spec = importlib.util.spec_from_file_location("runtime_evals", ROOT / "scripts/run-agent-evals.py")
runtime = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runtime)


def read(path):
    def invalid(value):
        raise ValueError(f"nonfinite JSON value: {value}")
    return json.loads(Path(path).read_text(), parse_constant=invalid)


def write_new(path, value):
    with Path(path).open("x") as f:
        json.dump(value, f, indent=2, allow_nan=False)
        f.write("\n")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def checker_identity():
    return {name: digest((ROOT / "scripts" / name).read_bytes()) for name in ("repair_acceptance.py", "run-agent-evals.py")}


def signature(root):
    """Source bytes and symlink identity; build/runtime outputs are excluded."""
    result = {}
    for p in sorted(root.rglob("*")):
        relative = p.relative_to(root)
        if relative.parts[0] in (".git", "target", ".doge"):
            continue
        if p.is_symlink():
            if not p.resolve().is_relative_to(root.resolve()):
                raise ValueError(f"escaping symlink: {relative}")
            result[str(relative)] = "symlink:" + os.readlink(p)
        elif p.is_file():
            result[str(relative)] = digest(p.read_bytes())
    return result


def environment(home, target):
    env = {key: os.environ[key] for key in ("PATH", "LANG", "LC_ALL", "CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "TMPDIR") if key in os.environ}
    # Reuse dependencies without inheriting provider secrets or user config.
    env.setdefault("CARGO_HOME", str(Path.home() / ".cargo"))
    env.setdefault("RUSTUP_HOME", str(Path.home() / ".rustup"))
    env.update(HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"),
               CARGO_TARGET_DIR=str(target.resolve()), GOPROXY="off", GOSUMDB="off",
               GOTOOLCHAIN="local", GOWORK="off", GOCACHE=str(target / "go"),
               GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
    home.mkdir(parents=True, exist_ok=True)
    return env


def tool_versions(language, env, cwd):
    commands = (["cargo", "--version"], ["rustc", "--version"]) if language == "rust" else (["go", "version"],)
    versions = {}
    for command in commands:
        try:
            result = subprocess.run(command, env=env, cwd=cwd, capture_output=True, text=True, timeout=10)
            versions[command[0]] = {"path": shutil.which(command[0], path=env.get("PATH")), "version": result.stdout.strip()[:500] if result.returncode == 0 else None, "probe_exit_code": result.returncode}
        except (OSError, subprocess.SubprocessError):
            versions[command[0]] = {"path": None, "version": None, "probe_exit_code": None}
    return versions


def archive(repo, commit, destination):
    data = subprocess.run(["git", "-C", str(repo), "archive", "--format=tar", commit], check=True, capture_output=True).stdout
    # Materialize tracked files only, with no Git history or future solution.
    with tarfile.open(fileobj=io.BytesIO(data)) as tar:
        for entry in tar:
            p = destination / entry.name
            if not p.resolve().is_relative_to(destination.resolve()):
                raise ValueError("unsafe archive path")
            if entry.isdir():
                p.mkdir(parents=True, exist_ok=True)
            elif entry.isfile():
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_bytes(tar.extractfile(entry).read())
                p.chmod(entry.mode & 0o777)
            elif entry.issym():
                p.parent.mkdir(parents=True, exist_ok=True)
                if not (p.parent / entry.linkname).resolve().is_relative_to(destination.resolve()):
                    raise ValueError("unsafe archive symlink")
                p.symlink_to(entry.linkname)
            else:
                raise ValueError("unsupported archive entry")
    return digest(data)


def prepare(output, repo, variants, trials, kind="offline-replay", case_ids=None):
    if not variants or len(set(variants)) != len(variants) or any(not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_-]*", v) for v in variants):
        raise ValueError("variant names must be unique path-safe labels")
    if type(trials) is not int or not 1 <= trials <= 10:
        raise ValueError("trials must be 1..10")
    if kind not in ("offline-replay", "model-evaluation"):
        raise ValueError("invalid experiment kind")
    output = output.resolve()
    if output.exists():
        raise ValueError("output already exists; never overwrite a prior experiment")
    cases = read(CASES / "cases.json")
    by_id = {case["case_id"]: case for case in cases}
    if case_ids is None:
        cases = [case for case in cases if case.get("default", True)]
    else:
        if not case_ids or len(set(case_ids)) != len(case_ids):
            raise ValueError("case selection must be nonempty and contain no duplicates")
        unknown = [case_id for case_id in case_ids if case_id not in by_id]
        if unknown:
            raise ValueError("unknown case selection: " + ", ".join(unknown))
        cases = [by_id[case_id] for case_id in case_ids]
    if not cases:
        raise ValueError("no cases selected")
    for case in cases:
        for command in case["checks"]:
            check_kind = command.get("kind", "test")
            if check_kind == "vet":
                if case["language"] != "go" or command["argv"] != ["go", "vet", "./..."]:
                    raise ValueError("vet kind is only for the fixed go vet ./... check")
            elif check_kind != "test" or type(command.get("min_tests")) is not int or command["min_tests"] < 1:
                raise ValueError("test checks require a positive minimum reported test count")
    # Resolve historical inputs before creating any planned workspace.
    for case in cases:
        if case["provenance"]["kind"] == "historical":
            commit = case["provenance"]["base_commit"]
            resolved = subprocess.run(["git", "-C", str(repo), "rev-parse", commit + "^{commit}"], check=True, capture_output=True, text=True).stdout.strip()
            if resolved != commit:
                raise ValueError("historical commit identity mismatch")
    output.mkdir(parents=True)
    runs = []
    for variant in variants:
        for case in cases:
            for trial in range(1, trials + 1):
                name = f"{variant}--{case['case_id']}--{trial:03d}"
                run = output / name
                workspace = run / "workspace"
                run.mkdir()
                if case["provenance"]["kind"] == "historical":
                    workspace.mkdir()
                    source_digest = archive(repo, case["provenance"]["base_commit"], workspace)
                else:
                    shutil.copytree(CASES / case["fixture"], workspace)
                    source_digest = digest(json.dumps(signature(workspace), sort_keys=True).encode())
                baseline = signature(workspace)
                baseline_sources = run / "baseline-source"
                baseline_sources.mkdir()
                for allowed in case["allowed_paths"]:
                    dest = baseline_sources / allowed
                    dest.parent.mkdir(parents=True, exist_ok=True)
                    dest.write_bytes((workspace / allowed).read_bytes())
                oracle = (CASES / case["oracle"]).read_bytes()
                (run / "oracle").write_bytes(oracle)
                # Future reference identity is operator-only, not the task file.
                task = {key: case[key] for key in ("case_id", "prompt", "allowed_paths")}
                write_new(run / "task.json", task)
                (workspace / "REPAIR_TASK.txt").write_text(case["prompt"] + "\n")
                baseline["REPAIR_TASK.txt"] = digest((workspace / "REPAIR_TASK.txt").read_bytes())
                criteria = {**case, "baseline": baseline, "source_digest": source_digest,
                            "output_binding": "sha256-v1",
                            "oracle_digest": digest(oracle), "variant": variant, "trial": trial, "kind": kind}
                write_new(run / "criteria.json", criteria)
                runs.append({"run": name, "criteria_digest": digest((run / "criteria.json").read_bytes())})
    plan = {"schema_version": 1, "kind": kind, "case_ids": [case["case_id"] for case in cases], "runs": runs, "checker": checker_identity(),
            "comparison": "unclassified; labels alone do not establish matched-model or product equivalence"}
    write_new(output / "plan.json", plan)
    return plan


def load_run(run, require_current_checker=True):
    run = run.resolve()
    plan = read(run.parent / "plan.json")
    if plan.get("schema_version") != 1:
        raise ValueError("unsupported saved experiment schema")
    if require_current_checker and plan.get("checker") != checker_identity():
        raise ValueError("checker changed after preparation; preserve this experiment and prepare a new one")
    entry = next((r for r in plan["runs"] if r["run"] == run.name), None)
    if entry is None or digest((run / "criteria.json").read_bytes()) != entry["criteria_digest"]:
        raise ValueError("criteria are not the frozen planned criteria")
    criteria = read(run / "criteria.json")
    if digest((run / "oracle").read_bytes()) != criteria["oracle_digest"]:
        raise ValueError("independent oracle changed after preparation")
    return criteria


def check(run):
    criteria = load_run(run)
    if criteria.get("output_binding") != "sha256-v1":
        raise ValueError("fresh checks require the prepared output-binding contract")
    if (run / "checks.json").exists():
        raise ValueError("checks already recorded; use a new planned trial for any rerun")
    workspace = run / "workspace"
    before = signature(workspace)
    baseline = criteria["baseline"]
    changed = sorted(p for p in set(before) | set(baseline) if before.get(p) != baseline.get(p))
    violations = [p for p in changed if p not in criteria["allowed_paths"]]
    results = []
    versions = {}
    snapshot_error = None
    (run / "candidate.patch").write_text("")
    if not violations:
        with tempfile.TemporaryDirectory(prefix="repair-check-", dir=run) as tmp:
            verifying = Path(tmp) / "workspace"
            shutil.copytree(workspace, verifying, symlinks=True, ignore=shutil.ignore_patterns(".git", "target", ".doge"))
            if signature(verifying) != before:
                snapshot_error = "candidate changed while creating verifier copy"
            else:
                # Inline historical tests are retained exactly, while implementation changes.
                if criteria["language"] == "rust":
                    original = (run / "baseline-source/src/execution/policy.rs").read_bytes()
                    if digest(original) != baseline["src/execution/policy.rs"]:
                        raise ValueError("baseline source changed after preparation")
                    marker = b"#[cfg(test)]"
                    old_tests = original[original.index(marker):]
                    if not (verifying / "src/execution/policy.rs").exists() or not (verifying / "src/execution/policy.rs").read_bytes().endswith(old_tests):
                        violations.append("historical inline tests changed")
                patch = []
                for path in changed:
                    if path in criteria["allowed_paths"]:
                        original = (run / "baseline-source" / path).read_bytes()
                        if digest(original) != baseline[path]:
                            raise ValueError("baseline source changed after preparation")
                        candidate = (verifying / path).read_text() if (verifying / path).is_file() else ""
                        for line in difflib.unified_diff(original.decode().splitlines(True), candidate.splitlines(True), fromfile="a/"+path, tofile="b/"+path):
                            patch.append(line if line.endswith("\n") else line + "\n\\ No newline at end of file\n")
                (run / "candidate.patch").write_text("".join(patch))
            if not violations and snapshot_error is None:
                if criteria["language"] == "rust":
                    oracle = verifying / "src/repair_acceptance.rs"
                    oracle.write_bytes((run / "oracle").read_bytes())
                    main = verifying / "src/main.rs"
                    main.write_bytes(main.read_bytes() + b'\n#[cfg(test)]\nmod repair_acceptance;\n')
                else:
                    (verifying / "repair_acceptance_test.go").write_bytes((run / "oracle").read_bytes())
                verification_source = signature(verifying)
                env = environment(run / "check-home", run.parent / "build-cache")
                versions = tool_versions(criteria["language"], env, run / "check-home")
                for command in criteria["checks"]:
                    name = command["name"]
                    check_kind = command.get("kind", "test")
                    stdout, stderr = run / (name + ".stdout.txt"), run / (name + ".stderr.txt")
                    code, timeout, elapsed, error, truncated = runtime.stream_child(command["argv"], verifying, env, command["timeout_seconds"], 1, stdout, stderr)
                    text = stdout.read_text(errors="replace")
                    if criteria["language"] == "rust":
                        counts = re.findall(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed;", text)
                        executed = sum(int(passed) + int(failed) for passed, failed in counts)
                    else:
                        executed = len(re.findall(r"(?m)^\s*--- (?:PASS|FAIL):", text))
                    source_changed = signature(verifying) != verification_source
                    count_ok = check_kind == "vet" or executed >= command["min_tests"]
                    passed = code == 0 and not timeout and not error and not truncated and count_ok and not source_changed
                    results.append({"name": name, "kind": check_kind, "argv": command["argv"], "exit_code": code,
                                    "status": "passed" if passed else "invalidated" if source_changed else "timed_out" if timeout else "blocked" if error or (check_kind == "test" and code != 0 and executed == 0) else "incomplete" if code == 0 else "failed",
                                    "timed_out": timeout, "elapsed_seconds": elapsed, "reported_tests": executed,
                                    "test_count_source": "not_applicable" if check_kind == "vet" else "stdout_heuristic", "test_execution_attested": None if check_kind == "vet" else False,
                                    "passed": passed, "error": error, "output_truncated": truncated,
                                    "stdout_sha256": digest(stdout.read_bytes()), "stderr_sha256": digest(stderr.read_bytes()),
                                    "verifier_source_changed": source_changed})
    if signature(workspace) != before:
        snapshot_error = "candidate changed during verification"
    if violations:
        results = [{"name": c["name"], "argv": c["argv"], "status": "scope_denied", "passed": False, "reported_tests": 0, "test_execution_attested": False} for c in criteria["checks"]]
    elif snapshot_error:
        if not results:
            results = [{"name": c["name"], "argv": c["argv"], "reported_tests": 0, "test_execution_attested": False} for c in criteria["checks"]]
        for row in results:
            row.update(status="invalidated", passed=False, candidate_snapshot_error=snapshot_error)
    result = {"schema_version": 1, "criteria_digest": digest((run / "criteria.json").read_bytes()),
              "output_binding": criteria["output_binding"],
              "candidate_digest": digest(json.dumps(before, sort_keys=True).encode()),
              "candidate_patch_digest": digest((run / "candidate.patch").read_bytes()),
              "candidate_snapshot_error": snapshot_error,
              "check_host": {"platform": platform.platform(), "python": platform.python_version()},
              "tool_versions": versions,
              "changed_paths": changed, "scope_violations": violations, "checks": results,
              "machine_passed": not violations and bool(results) and all(r["passed"] for r in results),
              "accepted": None}
    write_new(run / "checks.json", result)
    return result


def number(value, name, nullable=False):
    if value is None and nullable:
        return value
    if type(value) not in (int, float) or value < 0:
        raise ValueError(f"{name} must be finite and nonnegative")
    try:
        finite = math.isfinite(value)
    except OverflowError:
        finite = False
    if not finite:
        raise ValueError(f"{name} must be finite and nonnegative")
    return value


def validate_check_outputs(run, checks, require_hashes=False):
    for command in checks["checks"]:
        for stream in ("stdout", "stderr"):
            expected = command.get(stream + "_sha256")
            captured = "exit_code" in command or command.get("status") not in ("scope_denied", "invalidated")
            if require_hashes and captured and expected is None:
                raise ValueError("required check output hash binding is missing")
            if expected is not None:
                path = run / (command["name"] + "." + stream + ".txt")
                if not path.is_file() or digest(path.read_bytes()) != expected:
                    raise ValueError("recorded check output evidence changed or is missing")


def review(run, evidence):
    criteria = load_run(run)
    checks = read(run / "checks.json")
    if checks["criteria_digest"] != digest((run / "criteria.json").read_bytes()):
        raise ValueError("check evidence belongs to different criteria")
    if checks["candidate_patch_digest"] != digest((run / "candidate.patch").read_bytes()):
        raise ValueError("candidate patch evidence changed")
    current = digest(json.dumps(signature(run / "workspace"), sort_keys=True).encode())
    if current != checks["candidate_digest"]:
        raise ValueError("candidate changed after checks; record a new trial")
    if criteria.get("output_binding") != "sha256-v1" or checks.get("output_binding") != criteria["output_binding"]:
        raise ValueError("new review requires the prepared output-binding contract")
    validate_check_outputs(run, checks, require_hashes=True)
    if not isinstance(evidence, dict) or set(evidence) != {"accepted", "notes", "execution"}:
        raise ValueError("review fields must be accepted, notes, execution")
    if type(evidence.get("accepted")) is not bool or not isinstance(evidence.get("notes"), str) or not evidence["notes"].strip():
        raise ValueError("review needs an explicit accepted boolean and nonempty notes")
    execution = evidence["execution"]
    if not isinstance(execution, dict) or set(execution) != {"status", "elapsed_seconds", "review_seconds", "rework_seconds", "interventions", "cost"} | ({"identity"} if criteria["kind"] == "model-evaluation" else set()):
        raise ValueError("unexpected or missing execution fields")
    if execution["status"] not in ("completed", "failed", "timed_out", "blocked", "cancelled"):
        raise ValueError("invalid execution status")
    if evidence["accepted"] and (not checks["machine_passed"] or execution["status"] != "completed"):
        raise ValueError("acceptance requires all checks and completed execution; machine pass alone never accepts")
    for key in ("elapsed_seconds", "review_seconds", "rework_seconds"):
        number(execution[key], key, nullable=True)
    if not isinstance(execution["interventions"], list):
        raise ValueError("interventions must be a list")
    for event in execution["interventions"]:
        if not isinstance(event, dict) or set(event) != {"kind", "note", "seconds"}:
            raise ValueError("intervention fields must be kind, note, seconds")
        if event["kind"] not in ("extra_instruction", "manual_edit", "restart", "environment_workaround") or not isinstance(event.get("note"), str) or not event["note"].strip():
            raise ValueError("each intervention needs a recognized kind and a note")
        number(event["seconds"], "intervention seconds", nullable=True)
    if not isinstance(execution["cost"], dict) or set(execution["cost"]) != {"estimated_usd", "confirmed_usd"}:
        raise ValueError("cost fields must be estimated_usd, confirmed_usd")
    for key in ("estimated_usd", "confirmed_usd"):
        number(execution["cost"][key], key, nullable=True)
    if criteria["kind"] == "offline-replay":
        if any(execution["cost"][key] != 0 for key in ("estimated_usd", "confirmed_usd")):
            raise ValueError("offline replay requires explicit zero model cost")
    else:
        identity = execution.get("identity")
        if not isinstance(identity, dict) or any(not isinstance(identity.get(k), str) or not identity[k].strip() for k in ("model", "provider", "configuration", "permissions", "environment")):
            raise ValueError("model evaluation needs actual model/provider/configuration/permissions/environment identity")
    result = {"schema_version": 1, "case_id": criteria["case_id"], "variant": criteria["variant"],
              "kind": criteria["kind"], "provenance_kind": criteria["provenance"]["kind"],
              "checks_digest": digest((run / "checks.json").read_bytes()), **evidence}
    write_new(run / "review.json", result)
    return result


def saved_evidence(run, criteria):
    """Validate saved identities without rerunning or rescoring old checks."""
    checks = read(run / "checks.json")
    if checks.get("schema_version") != 1:
        raise ValueError("unsupported saved check schema")
    if checks["criteria_digest"] != digest((run / "criteria.json").read_bytes()):
        raise ValueError("check evidence belongs to different criteria")
    if checks["candidate_patch_digest"] != digest((run / "candidate.patch").read_bytes()):
        raise ValueError("candidate patch evidence changed")
    if checks["candidate_digest"] != digest(json.dumps(signature(run / "workspace"), sort_keys=True).encode()):
        raise ValueError("reviewed candidate changed; preserve original evidence and use a new trial")
    binding = criteria.get("output_binding")
    if binding is not None and (binding != "sha256-v1" or checks.get("output_binding") != binding):
        raise ValueError("saved output-binding contract changed or is unsupported")
    validate_check_outputs(run, checks, require_hashes=binding == "sha256-v1")
    row = read(run / "review.json") if (run / "review.json").exists() else None
    if row is not None:
        if row.get("schema_version") != 1 or row["checks_digest"] != digest((run / "checks.json").read_bytes()):
            raise ValueError("check evidence changed after review or review schema is unsupported")
        expected = {"case_id": criteria["case_id"], "variant": criteria["variant"], "kind": criteria["kind"], "provenance_kind": criteria["provenance"]["kind"]}
        if any(row.get(key) != value for key, value in expected.items()):
            raise ValueError("saved review belongs to a different trial identity")
        if type(row.get("accepted")) is not bool or (row["accepted"] and (not checks["machine_passed"] or row["execution"]["status"] != "completed")):
            raise ValueError("saved acceptance contradicts recorded checks or execution")
    return checks, row


def evidence_files(run):
    paths = [run.parent / "plan.json", run / "criteria.json", run / "oracle", run / "checks.json", run / "candidate.patch", run / "review.json"]
    paths.extend(run.glob("*.stdout.txt"))
    paths.extend(run.glob("*.stderr.txt"))
    return {str(path): digest(path.read_bytes()) if path.is_file() else None for path in paths}


def inspect_run(run, include_patch=False):
    """Read-only candidate handoff; an old checker is provenance, not a rescore."""
    before = evidence_files(run)
    criteria = load_run(run, require_current_checker=False)
    checks, row = saved_evidence(run, criteria)
    plan = read(run.parent / "plan.json")
    patch = (run / "candidate.patch").read_bytes()
    if include_patch and len(patch) > 65536:
        raise ValueError("inline patch exceeds 64 KiB; inspect candidate.patch separately")
    commands = []
    for command in checks["checks"]:
        item = dict(command)
        item["output_evidence"] = {stream: {"path": command["name"] + "." + stream + ".txt", "hash_binding": "verified" if command.get(stream + "_sha256") is not None else "unrecorded_legacy_or_not_captured"} for stream in ("stdout", "stderr")}
        commands.append(item)
    result = {"schema_version": 1, "case_id": criteria["case_id"], "variant": criteria["variant"], "trial": criteria["trial"], "experiment_kind": criteria["kind"],
              "source": {key: value for key, value in criteria["provenance"].items() if key in ("kind", "repository", "base_commit")},
              "source_digest": criteria["source_digest"], "criteria_digest": checks["criteria_digest"],
              "checker": {"recorded": plan["checker"], "reader": checker_identity(), "matches_current": plan["checker"] == checker_identity(), "interpretation": "saved_results_not_rescored"},
              "changed_paths": checks["changed_paths"], "scope_violations": checks["scope_violations"], "machine_passed": checks["machine_passed"],
              "accepted": row["accepted"] if row else None, "operator_review": row,
              "candidate_digest": checks["candidate_digest"], "patch": {"path": "candidate.patch", "sha256": checks["candidate_patch_digest"], "bytes": len(patch), "content": patch.decode() if include_patch else None},
              "check_host": checks.get("check_host"), "tool_versions": checks.get("tool_versions"), "checks": commands,
              "remaining_uncertainty": ["Reported stdout test counts are not assertion attestation; independent implementation review is required.", "Only saved checks/environment are represented; no additional architecture or model execution is inferred.", "Directory isolation and local hashes are not an OS sandbox or authenticated evidence store."]}
    saved_evidence(run, criteria)
    if evidence_files(run) != before:
        raise ValueError("evidence changed during inspection")
    return result


def summarize(output):
    plan = read(output / "plan.json")
    groups = {}
    for planned in plan["runs"]:
        run = output / planned["run"]
        criteria = load_run(run, require_current_checker=False)
        group = groups.setdefault(criteria["variant"], {"planned": 0, "reviews": []})
        group["planned"] += 1
        if (run / "review.json").exists():
            checks, row = saved_evidence(run, criteria)
            group["reviews"].append(row)
    summary = {"kind": plan["kind"], "comparison": plan["comparison"], "checker": {"recorded": plan["checker"], "reader": checker_identity(), "matches_current": plan["checker"] == checker_identity(), "interpretation": "saved_results_not_rescored"}, "variants": {}}
    for variant, group in groups.items():
        rows, total = group["reviews"], group["planned"]
        accepted = sum(row["accepted"] for row in rows)
        data = {"planned": total, "reviewed": len(rows), "unreviewed": total-len(rows), "accepted": accepted,
                "accepted_fraction_of_planned": accepted/total,
                "statuses": {status: sum(r["execution"]["status"] == status for r in rows) for status in ("completed", "failed", "timed_out", "blocked", "cancelled")},
                "provenance": {kind: sum(r["provenance_kind"] == kind for r in rows) for kind in ("historical", "synthetic")}}
        complete = len(rows) == total
        data["reviewed_interventions"] = sum(len(r["execution"]["interventions"]) for r in rows)
        intervention_times = [e["seconds"] for r in rows for e in r["execution"]["interventions"]]
        data["reviewed_intervention_seconds"] = sum(intervention_times) if all(t is not None for t in intervention_times) else None
        for key in ("elapsed_seconds", "review_seconds", "rework_seconds"):
            values = [r["execution"][key] for r in rows]
            known = all(v is not None for v in values)
            value = sum(values) if known else None
            data["reviewed_"+key] = value
            data[key+"_coverage"] = sum(v is not None for v in values)/total
            data[key+"_per_accepted"] = value/accepted if complete and known and accepted else None
        for key in ("estimated_usd", "confirmed_usd"):
            values = [r["execution"]["cost"][key] for r in rows]
            known = complete and all(v is not None for v in values)
            data["total_"+key] = sum(values) if known else None
            data[key+"_per_accepted"] = sum(values)/accepted if known and accepted else None
            data[key+"_coverage"] = sum(v is not None for v in values)/total
        summary["variants"][variant] = data
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    p = commands.add_parser("prepare")
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--source-repo", type=Path, default=ROOT)
    p.add_argument("--variant", action="append", required=True)
    p.add_argument("--trials", type=int, default=1)
    p.add_argument("--kind", choices=("offline-replay", "model-evaluation"), default="offline-replay")
    p.add_argument("--case", dest="case_ids", action="append", help="explicit case ID; repeat to select multiple cases (default: original two cases)")
    p = commands.add_parser("check"); p.add_argument("--run", type=Path, required=True)
    p = commands.add_parser("review"); p.add_argument("--run", type=Path, required=True); p.add_argument("--evidence", type=Path, required=True)
    p = commands.add_parser("inspect"); p.add_argument("--run", type=Path, required=True); p.add_argument("--include-patch", action="store_true", help="include the verified private patch, at most 64 KiB")
    p = commands.add_parser("summary"); p.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.command == "prepare":
            result = prepare(args.output, args.source_repo, args.variant, args.trials, args.kind, args.case_ids)
        elif args.command == "check":
            result = check(args.run.resolve())
        elif args.command == "review":
            result = review(args.run.resolve(), read(args.evidence))
        elif args.command == "inspect":
            result = inspect_run(args.run.resolve(), args.include_patch)
        else:
            result = summarize(args.output.resolve())
        print(json.dumps(result, indent=2, allow_nan=False))
        return int(args.command == "check" and not result["machine_passed"])
    except (ValueError, KeyError, OSError, subprocess.SubprocessError) as error:
        parser.exit(2, f"repair acceptance error: {type(error).__name__}: {error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
