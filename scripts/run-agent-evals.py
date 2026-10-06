#!/usr/bin/env python3
"""Runtime matched evaluation harness: run real `dgc exec --json` trials.

Takes an eval manifest, pins one base commit for every run, executes each
variant x case x trial in a fresh disposable git worktree with a fresh
session, and writes machine measurements plus a human review template.
This command performs live model calls unless --dry-run is given; it never
runs in CI and never fabricates human review (accepted stays null).
"""

import argparse
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import agent_eval_common as common

MAX_PARSE_BYTES = 5 * 1024 * 1024
MAX_STREAM_BYTES = 200 * 1024 * 1024
EVIDENCE_TIMEOUT_SECONDS = 60
VERSION_PROBE_TIMEOUT_SECONDS = 15


def log(message):
    print(f"[run-agent-evals] {message}", file=sys.stderr, flush=True)


def run_git(args, cwd, timeout=60):
    return subprocess.run(
        ["git"] + args,
        cwd=str(cwd),
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )


def repo_toplevel(start):
    result = run_git(["rev-parse", "--show-toplevel"], start)
    if result.returncode != 0:
        raise ValueError(
            f"not inside a git repository (starting at {start}): "
            f"{result.stderr.strip()}"
        )
    return Path(result.stdout.strip())


def resolve_base_commit(toplevel, base_ref):
    result = run_git(["rev-parse", "--verify", f"{base_ref}^{{commit}}"], toplevel)
    if result.returncode != 0:
        raise ValueError(
            f"cannot resolve base ref {base_ref!r} to a commit: "
            f"{result.stderr.strip()}"
        )
    return result.stdout.strip()


def validate_agent_binary(variant):
    """Fail fast when the variant command cannot be executed at all."""
    command = variant["agent_command"]
    binary = command[0]
    binary_path = Path(binary)
    # Allow PATH lookup for bare names; require existence otherwise.
    if "/" in binary or "\\" in binary:
        if not binary_path.is_file():
            raise ValueError(
                f"variant {variant['name']!r}: agent binary not found: {binary}"
            )
        if not os.access(binary, os.X_OK):
            raise ValueError(
                f"variant {variant['name']!r}: agent binary not executable: {binary}"
            )
    elif shutil.which(binary) is None:
        raise ValueError(
            f"variant {variant['name']!r}: agent binary not on PATH: {binary}"
        )
    if variant.get("config") is not None:
        text = Path(variant["config"]).read_text(
            encoding="utf-8", errors="replace"
        )
        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith("#"):
                continue
            # Config `resume = true` resumes the latest session, which would
            # break fresh-session isolation. An explicit `false` is harmless
            # and allowed; anything else fail-closed.
            match = re.match(r"(?i)resume\s*=\s*(.+?)\s*$", stripped)
            if match and match.group(1).lower() != "false":
                raise ValueError(
                    f"variant {variant['name']!r}: config must not enable "
                    "resume for eval runs"
                )


def variant_metadata(variant):
    """Collect non-secret variant identity (hashes only, never file content)."""
    command = variant["agent_command"]
    fingerprint = common.fingerprint_command(command)
    binary_sha = None
    binary = command[0]
    binary_path = Path(binary)
    if binary_path.is_file():
        try:
            binary_sha = common.sha256_file(binary_path)
        except OSError:
            binary_sha = None
    config_sha = None
    if variant.get("config") is not None:
        try:
            config_sha = common.sha256_file(variant["config"])
        except OSError:
            config_sha = None
    version = None
    # Probe from a disposable empty directory, never from the repository
    # checkout: variant commands are arbitrary executables and even a
    # --version probe must not be able to touch repository state.
    try:
        with tempfile.TemporaryDirectory(prefix="dgc-eval-probe-") as probe_dir:
            probe = subprocess.run(
                [binary, "--version"],
                cwd=probe_dir,
                capture_output=True,
                text=True,
                timeout=VERSION_PROBE_TIMEOUT_SECONDS,
                check=False,
            )
        text = (probe.stdout or probe.stderr or "").strip()
        if probe.returncode == 0 and text:
            version = text[:500]
    except (OSError, subprocess.SubprocessError):
        version = None
    metadata = {
        "agent_command_fingerprint": fingerprint,
        "agent_binary_sha256": binary_sha,
        "agent_version": version,
    }
    if variant.get("config") is not None:
        metadata["config_sha256"] = config_sha
    return metadata


def child_env(variant):
    env = dict(os.environ)
    if variant.get("config") is not None:
        env["DOGE_CODE_CONFIG"] = str(variant["config"])
    # Suppress desktop notification spam during bulk eval runs. Unset means
    # notifications stay enabled for normal users.
    env["DGC_DISABLE_NOTIFICATIONS"] = "1"
    return env


def stream_child(argv, cwd, env, timeout_seconds, grace_seconds,
                 stdout_path, stderr_path):
    """Run a child with bounded, concurrently drained output files.

    Give the leader time to flush on cancellation, then kill its original
    process group even if the leader already exited. A completed trial must
    not leave same-group descendants writing into its workspace. Detached
    sessions are outside this group boundary; stop readers before returning
    so they cannot change captured artifacts after collection finishes.
    """
    start = time.monotonic()
    timed_out = False
    truncated = [False, False]
    readers = []
    reader_errors = []
    stop_readers = threading.Event()

    def drain(pipe, path, index):
        try:
            with pipe, open(path, "wb") as handle:
                os.set_blocking(pipe.fileno(), False)
                remaining = MAX_STREAM_BYTES
                while not stop_readers.is_set():
                    try:
                        chunk = os.read(pipe.fileno(), 64 * 1024)
                    except BlockingIOError:
                        stop_readers.wait(0.01)
                        continue
                    if not chunk:
                        break
                    kept = chunk[:remaining]
                    handle.write(kept)
                    remaining -= len(kept)
                    if len(kept) != len(chunk):
                        truncated[index] = True
        except OSError as error:
            reader_errors.append(f"output_capture_failed: {error}")

    def kill_group():
        try:
            if os.name == "posix":
                # start_new_session makes pid the group id. Do not query
                # getpgid after wait(): the leader may already be reaped.
                os.killpg(child.pid, signal.SIGKILL)
            elif child.poll() is None:
                child.kill()
        except ProcessLookupError:
            pass

    def stop_child():
        try:
            child.terminate()
        except ProcessLookupError:
            pass
        try:
            child.wait(timeout=grace_seconds)
        except subprocess.TimeoutExpired:
            pass
        finally:
            kill_group()
        return child.wait()

    # Check the sinks before starting a process that might mutate a workspace.
    try:
        stdout_path.write_bytes(b"")
        stderr_path.write_bytes(b"")
        child = subprocess.Popen(
            argv, cwd=str(cwd), env=env, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, start_new_session=True,
        )
    except OSError as error:
        try:
            stderr_path.write_bytes(
                f"harness: failed to spawn child: {error}\n".encode("utf-8")
            )
        except OSError:
            pass
        return 127, False, 0.0, f"spawn_failed: {error}", False

    try:
        for index, (pipe, path) in enumerate((
            (child.stdout, stdout_path), (child.stderr, stderr_path),
        )):
            reader = threading.Thread(target=drain, args=(pipe, path, index))
            readers.append(reader)
            reader.start()
        try:
            exit_code = child.wait(timeout=timeout_seconds)
        except subprocess.TimeoutExpired:
            timed_out = True
            exit_code = stop_child()
        except KeyboardInterrupt:
            stop_child()
            raise
    finally:
        kill_group()
        child.wait()
        for reader in readers:
            reader.join(timeout=grace_seconds)
        if any(reader.is_alive() for reader in readers):
            reader_errors.append("output_capture_incomplete: descendant retained output pipe")
            stop_readers.set()
            for reader in readers:
                reader.join()
    error = "; ".join(reader_errors) if reader_errors else None
    return exit_code, timed_out, time.monotonic() - start, error, any(truncated)


def parse_exec_json(stdout_path):
    """Parse `dgc exec --json` stdout with an explicit size limit."""
    try:
        size = stdout_path.stat().st_size
    except OSError as error:
        return None, f"stdout_unreadable: {error}"
    if size == 0:
        return None, "empty_stdout"
    if size > MAX_PARSE_BYTES:
        return None, (
            f"stdout_exceeds_parse_limit: {size} bytes > {MAX_PARSE_BYTES}"
        )
    try:
        raw = stdout_path.read_bytes()[: MAX_PARSE_BYTES + 1]
        parsed = json.loads(raw.decode("utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        return None, f"stdout_not_json: {error}"
    if not isinstance(parsed, dict):
        return None, "stdout_not_json_object"
    return parsed, None


def find_sessions(workspace):
    store = Path(workspace) / ".doge" / "sessions"
    if not store.is_dir():
        return []
    sessions = []
    try:
        entries = sorted(store.iterdir())
    except OSError:
        return []
    for entry in entries:
        if entry.name in (".locks", ".recovery"):
            continue
        if entry.name.startswith("."):
            continue
        if entry.is_dir() and (entry / "session.json").is_file():
            sessions.append(entry.name)
    return sessions


def collect_evidence(variant, workspace, run_dir, env):
    """Run `session evidence --format json` for exactly one fresh session."""
    sessions = find_sessions(workspace)
    evidence_path = run_dir / "evidence.json"
    if not sessions:
        evidence_path.write_text(
            json.dumps({"status": "no_sessions"}) + "\n", encoding="utf-8"
        )
        return "no_sessions"
    if len(sessions) != 1:
        evidence_path.write_text(
            json.dumps(
                {"status": "ambiguous_sessions", "sessions": sorted(sessions)}
            )
            + "\n",
            encoding="utf-8",
        )
        return f"ambiguous_sessions:{len(sessions)}"
    session_id = sessions[0]
    argv = [
        variant["agent_command"][0],
        "session",
        "evidence",
        session_id,
        "--format",
        "json",
    ]
    try:
        result = subprocess.run(
            argv,
            cwd=str(workspace),
            env=env,
            capture_output=True,
            timeout=EVIDENCE_TIMEOUT_SECONDS,
            check=False,
        )
    except subprocess.TimeoutExpired:
        evidence_path.write_bytes(b"")
        return "evidence_timeout"
    except OSError as error:
        evidence_path.write_bytes(b"")
        return f"evidence_command_failed: {error}"
    if result.returncode != 0:
        evidence_path.write_bytes(result.stdout or b"")
        (run_dir / "evidence.stderr.txt").write_bytes(result.stderr or b"")
        return "evidence_command_failed"
    try:
        json.loads((result.stdout or b"").decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        evidence_path.write_bytes(result.stdout or b"")
        return f"evidence_invalid_json: {error}"
    evidence_path.write_bytes(result.stdout or b"")
    return "collected"


def collect_workspace_diff(workspace, run_dir, base_commit):
    """Save status, diff stat, and a patch including untracked source files."""
    status = run_git(
        ["status", "--porcelain=v1", "--untracked-files=all"], workspace
    )
    (run_dir / "git-status.txt").write_text(
        status.stdout if status.returncode == 0 else f"git status failed: {status.stderr}",
        encoding="utf-8",
    )
    # Intent-to-add lets untracked source files appear in `git diff <base_commit>`
    # without committing anything. Session evidence was already collected,
    # so touching the index here is safe; the worktree is disposable.
    # NUL-separated listing keeps newline-containing filenames exact.
    untracked = run_git(
        ["ls-files", "--others", "--exclude-standard", "-z"], workspace
    )
    added = []
    if untracked.returncode == 0:
        for name in untracked.stdout.split("\0"):
            if not name:
                continue
            add = run_git(["add", "-N", "--", name], workspace)
            if add.returncode == 0:
                added.append(name)
    diff = run_git(["diff", base_commit, "--binary", "--no-ext-diff"], workspace)
    (run_dir / "agent.patch").write_text(
        diff.stdout if diff.returncode == 0 else f"git diff failed: {diff.stderr}",
        encoding="utf-8",
    )
    stat = run_git(["diff", base_commit, "--stat", "--no-ext-diff"], workspace)
    (run_dir / "diff-stat.txt").write_text(
        stat.stdout if stat.returncode == 0 else f"git diff --stat failed: {stat.stderr}",
        encoding="utf-8",
    )
    return {
        "status_output": status.stdout if status.returncode == 0 else "",
        "patch_bytes": (run_dir / "agent.patch").stat().st_size,
        "intent_to_add": added,
    }


def workspace_changed(diff_info):
    if diff_info["patch_bytes"] > 0:
        return True
    return bool(diff_info["status_output"].strip())


def run_post_checks(case, workspace, run_dir, env):
    checks_dir = run_dir / "checks"
    checks_dir.mkdir(parents=True, exist_ok=True)
    results = []
    for check in case.get("post_checks", []):
        name = check["name"]
        argv = check["argv"]
        timeout = check.get("timeout_seconds", 120)
        stdout_path = checks_dir / f"{name}.stdout.txt"
        stderr_path = checks_dir / f"{name}.stderr.txt"
        start = time.monotonic()
        timed_out = False
        try:
            completed = subprocess.run(
                argv,
                cwd=str(workspace),
                env=env,
                capture_output=True,
                timeout=timeout,
                check=False,
            )
            exit_code = completed.returncode
            stdout_path.write_bytes(completed.stdout or b"")
            stderr_path.write_bytes(completed.stderr or b"")
        except subprocess.TimeoutExpired as error:
            timed_out = True
            exit_code = None
            stdout_path.write_bytes(error.stdout or b"")
            stderr_path.write_bytes(error.stderr or b"")
        except OSError as error:
            exit_code = 127
            stdout_path.write_bytes(b"")
            stderr_path.write_bytes(f"harness: check spawn failed: {error}\n".encode())
        elapsed = time.monotonic() - start
        results.append(
            {
                "name": name,
                "argv": list(argv),
                "exit_code": exit_code,
                "timed_out": timed_out,
                "elapsed_seconds": elapsed,
                "stdout_artifact": f"checks/{name}.stdout.txt",
                "stderr_artifact": f"checks/{name}.stderr.txt",
            }
        )
    return results


def remove_worktree(toplevel, workspace):
    result = run_git(["worktree", "remove", "--force", str(workspace)], toplevel)
    if result.returncode != 0:
        # Fall back to pruning; report the failure to the caller.
        run_git(["worktree", "prune"], toplevel)
        return f"worktree remove failed: {result.stderr.strip()}"
    run_git(["worktree", "prune"], toplevel)
    return None


def run_single(toplevel, manifest, variant_by_name, variant_metas, case_by_id,
               item, base_commit, settings, out_root, keep_workspaces):
    variant_name = item["variant"]
    case = case_by_id[item["case_id"]]
    trial = item["trial"]
    variant = variant_by_name[variant_name]
    run_dir = (
        out_root
        / variant_name
        / "runs"
        / case["case_id"]
        / f"trial-{trial:03d}"
    )
    run_dir.mkdir(parents=True, exist_ok=True)
    measurement = common.base_measurement(
        case["case_id"],
        trial,
        base_commit,
        manifest["model"],
        settings,
        variant_name,
        variant_metas[variant_name],
    )
    measurement["artifacts"] = {"directory": str(run_dir.relative_to(out_root / variant_name))}
    env = child_env(variant)
    harness_error = None
    timed_out = False
    exit_code = None
    elapsed = 0.0
    workspace = None
    cancelled = False
    warnings = []
    try:
        workspace = Path(
            tempfile.mkdtemp(prefix=f"dgc-eval-{variant_name}-{case['case_id']}-t{trial}-")
        )
        # mkdtemp created the directory; worktree add needs a missing path.
        workspace.rmdir()
        created = run_git(
            ["worktree", "add", "--detach", str(workspace), base_commit], toplevel
        )
        if created.returncode != 0:
            harness_error = f"worktree_failed: {created.stderr.strip()}"
            workspace = None
        else:
            pre = run_git(
                ["status", "--porcelain=v1", "--untracked-files=all"], workspace
            )
            if pre.returncode != 0:
                harness_error = f"pre_run_status_failed: {pre.stderr.strip()}"
            elif pre.stdout.strip():
                harness_error = (
                    "worktree_not_clean: "
                    + pre.stdout.strip()[:2000]
                )
        parsed = None
        parse_diagnostic = None
        telemetry = common.extract_exec_telemetry(None)
        if harness_error is None:
            argv = common.build_agent_argv(
                variant, manifest["model"], manifest["provider"], case["prompt"]
            )
            stdout_path = run_dir / "stdout.txt"
            stderr_path = run_dir / "stderr.txt"
            try:
                exit_code, timed_out, elapsed, spawn_error, truncated = stream_child(
                    argv,
                    workspace,
                    env,
                    manifest["timeout_seconds"],
                    manifest["termination_grace_seconds"],
                    stdout_path,
                    stderr_path,
                )
            except KeyboardInterrupt:
                cancelled = True
                raise
            measurement["elapsed_seconds"] = elapsed
            if spawn_error is not None:
                harness_error = spawn_error
            else:
                if truncated:
                    (run_dir / "output-truncated.txt").write_text(
                        "stdout or stderr exceeded "
                        f"{MAX_STREAM_BYTES} bytes\n",
                        encoding="utf-8",
                    )
                parsed, parse_diagnostic = parse_exec_json(stdout_path)
                if parsed is not None:
                    (run_dir / "exec.json").write_text(
                        json.dumps(parsed, indent=2, allow_nan=False),
                        encoding="utf-8",
                    )
                telemetry = common.extract_exec_telemetry(parsed)
        else:
            measurement["elapsed_seconds"] = 0.0

        if harness_error is None:
            evidence_status = collect_evidence(variant, workspace, run_dir, env)
        else:
            evidence_status = "not_collected"
            (run_dir / "evidence.json").write_text(
                json.dumps({"status": "not_collected"}) + "\n", encoding="utf-8"
            )
        measurement["evidence_status"] = evidence_status

        if harness_error is None:
            diff_info = collect_workspace_diff(workspace, run_dir, base_commit)
        else:
            diff_info = {"status_output": "", "patch_bytes": 0, "intent_to_add": []}
            for name in ("git-status.txt", "diff-stat.txt", "agent.patch"):
                if not (run_dir / name).exists():
                    (run_dir / name).write_text("", encoding="utf-8")

        if harness_error is None:
            check_results = run_post_checks(case, workspace, run_dir, env)
        else:
            check_results = []
        measurement["post_checks"] = check_results
        if check_results:
            measurement["required_checks_passed"] = all(
                r["exit_code"] == 0 and not r["timed_out"] for r in check_results
            )
        else:
            measurement["required_checks_passed"] = None

        findings = []
        if (
            case.get("workspace_policy") == "read_only"
            and harness_error is None
            and workspace_changed(diff_info)
        ):
            findings.append("workspace_modified_in_read_only_case")
        if timed_out:
            findings.append("agent_timeout")
        if parse_diagnostic is not None and harness_error is None:
            findings.append(f"exec_output_unparseable:{parse_diagnostic.split(':')[0]}")
        measurement["machine_findings"] = findings

        if cancelled:
            run_status = "cancelled"
        else:
            run_status, stop_reason = common.map_run_status(
                telemetry, exit_code if exit_code is not None else -1,
                timed_out, harness_error,
            )
            if parse_diagnostic is not None and run_status == "completed":
                # Valid-looking status without parseable JSON is impossible;
                # unparseable output can never be a completion.
                run_status = "harness_error"
                harness_error = parse_diagnostic
        measurement["run_status"] = run_status
        measurement["stop_reason"] = telemetry.get("stop_reason")
        for key in (
            "input_tokens",
            "output_tokens",
            "cached_input_tokens",
            "reasoning_tokens",
            "cache_write_tokens",
            "known_cached_input_tokens",
            "known_reasoning_tokens",
            "known_cache_write_tokens",
            "cached_usage_records",
            "reasoning_usage_records",
            "cache_write_usage_records",
            "agent_elapsed_seconds",
            "tool_calls",
            "iterations",
            "request_attempts",
            "usage_records",
            "unknown_usage_attempts",
            "budget_charged_tokens",
            "budget_provider_reported_tokens",
            "budget_estimated_tokens",
            "conversation_length",
        ):
            measurement[key] = telemetry[key]
        measurement["harness_error"] = harness_error
        measurement["exit_code"] = exit_code
        measurement["timed_out"] = timed_out
        measurement["exec_output_diagnostic"] = parse_diagnostic
        measurement["artifacts"] = {
            "directory": str(run_dir.relative_to(out_root / variant_name)),
            "evidence": "evidence.json",
            "patch": "agent.patch",
            "stdout": "stdout.txt",
            "stderr": "stderr.txt",
        }
        if keep_workspaces and workspace is not None:
            measurement["workspace"] = str(workspace)
        (run_dir / "run.json").write_text(
            json.dumps(measurement, indent=2, allow_nan=False), encoding="utf-8"
        )
    except KeyboardInterrupt:
        measurement["run_status"] = "cancelled"
        measurement["harness_error"] = harness_error
        measurement["exit_code"] = exit_code
        measurement["timed_out"] = timed_out
        try:
            (run_dir / "run.json").write_text(
                json.dumps(measurement, indent=2, allow_nan=False), encoding="utf-8"
            )
        except OSError:
            pass
        raise
    finally:
        if workspace is not None and not keep_workspaces:
            warning = remove_worktree(toplevel, workspace)
            if warning is not None:
                warnings.append(warning)
                log(f"WARNING: {warning} ({workspace})")
    if warnings:
        measurement.setdefault("warnings", []).extend(warnings)
        try:
            (run_dir / "run.json").write_text(
                json.dumps(measurement, indent=2, allow_nan=False), encoding="utf-8"
            )
        except OSError:
            pass
    return measurement


def write_outputs(out_root, manifest, variant_names, records_by_variant,
                  resolved):
    for variant_name in variant_names:
        variant_dir = out_root / variant_name
        variant_dir.mkdir(parents=True, exist_ok=True)
        measurements = records_by_variant.get(variant_name, [])
        with open(variant_dir / "measurements.jsonl", "w", encoding="utf-8") as handle:
            for record in measurements:
                handle.write(
                    json.dumps(record, allow_nan=False) + "\n"
                )
        template = []
        for record in measurements:
            entry = dict(record)
            entry["accepted"] = None
            entry["rework_count"] = None
            entry["contract_violations"] = None
            if not entry.get("review_notes"):
                entry["review_notes"] = ""
            template.append(entry)
        with open(variant_dir / "review-template.jsonl", "w", encoding="utf-8") as handle:
            for entry in template:
                handle.write(json.dumps(entry, allow_nan=False) + "\n")
    with open(out_root / "resolved-manifest.json", "w", encoding="utf-8") as handle:
        handle.write(json.dumps(resolved, indent=2, allow_nan=False) + "\n")


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--case", dest="cases", action="append", default=[])
    parser.add_argument("--keep-workspaces", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--overwrite", action="store_true")
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    try:
        manifest = common.load_manifest(args.manifest)
    except ValueError as error:
        print(f"Manifest error: {error}", file=sys.stderr)
        return 2
    try:
        cases = common.load_cases(manifest["cases_path"])
    except ValueError as error:
        print(f"Cases error: {error}", file=sys.stderr)
        return 2
    try:
        matrix = common.execution_matrix(manifest, cases, args.cases or None)
    except ValueError as error:
        print(f"Selection error: {error}", file=sys.stderr)
        return 2
    case_by_id = {case["case_id"]: case for case in cases}
    variant_by_name = {v["name"]: v for v in manifest["variants"]}

    try:
        toplevel = repo_toplevel(Path.cwd())
    except ValueError as error:
        print(f"Git error: {error}", file=sys.stderr)
        return 2
    try:
        base_commit = resolve_base_commit(toplevel, manifest["base_ref"])
    except ValueError as error:
        print(f"Git error: {error}", file=sys.stderr)
        return 2
    try:
        for variant in manifest["variants"]:
            validate_agent_binary(variant)
    except ValueError as error:
        print(f"Variant error: {error}", file=sys.stderr)
        return 2

    cases_digest = common.sha256_file(manifest["cases_path"])
    settings = common.build_settings(manifest, cases_digest)
    variant_metas = {
        variant["name"]: variant_metadata(variant)
        for variant in manifest["variants"]
    }
    resolved = {
        "schema_version": manifest["schema_version"],
        "manifest": str(Path(args.manifest).resolve()),
        "base_ref": manifest["base_ref"],
        "base_commit": base_commit,
        "cases_file": manifest["cases_path"],
        "cases_digest": cases_digest,
        "model": manifest["model"],
        "provider": manifest["provider"],
        "environment_id": manifest["environment_id"],
        "timeout_seconds": manifest["timeout_seconds"],
        "termination_grace_seconds": manifest["termination_grace_seconds"],
        "seed": manifest["seed"],
        "variants": [
            {
                "name": variant["name"],
                "agent_command_fingerprint": variant_metas[variant["name"]][
                    "agent_command_fingerprint"
                ],
                "agent_binary_sha256": variant_metas[variant["name"]].get(
                    "agent_binary_sha256"
                ),
                "agent_version": variant_metas[variant["name"]].get("agent_version"),
                "config_sha256": variant_metas[variant["name"]].get("config_sha256"),
            }
            for variant in manifest["variants"]
        ],
    }

    out_root = args.output
    if args.dry_run:
        print(json.dumps({
            "dry_run": True,
            "base_commit": base_commit,
            "runs": len(matrix),
            "matrix": matrix,
            "resolved_manifest": resolved,
        }, indent=2))
        return 0

    if out_root.exists():
        for variant in manifest["variants"]:
            for leaf in ("measurements.jsonl", "review-template.jsonl"):
                existing = out_root / variant["name"] / leaf
                if existing.exists() and not args.overwrite:
                    print(
                        f"Refusing to overwrite {existing} without --overwrite "
                        "(human review must never be implicitly overwritten).",
                        file=sys.stderr,
                    )
                    return 2
    out_root.mkdir(parents=True, exist_ok=True)

    records = {variant["name"]: [] for variant in manifest["variants"]}
    interrupted = False
    try:
        for index, item in enumerate(matrix, 1):
            log(
                f"run {index}/{len(matrix)}: variant={item['variant']} "
                f"case={item['case_id']} trial={item['trial']}"
            )
            try:
                measurement = run_single(
                    toplevel,
                    manifest,
                    variant_by_name,
                    variant_metas,
                    case_by_id,
                    item,
                    base_commit,
                    settings,
                    out_root,
                    args.keep_workspaces,
                )
            except KeyboardInterrupt:
                # The interrupted run already flushed its own run.json as
                # cancelled; recover it into the records so no trial is lost.
                run_json = (
                    out_root
                    / item["variant"]
                    / "runs"
                    / item["case_id"]
                    / f"trial-{item['trial']:03d}"
                    / "run.json"
                )
                try:
                    measurement = json.loads(run_json.read_text(encoding="utf-8"))
                except (OSError, json.JSONDecodeError):
                    measurement = common.base_measurement(
                        item["case_id"],
                        item["trial"],
                        base_commit,
                        manifest["model"],
                        settings,
                        item["variant"],
                        variant_metas[item["variant"]],
                    )
                    measurement["run_status"] = "cancelled"
                records[item["variant"]].append(measurement)
                raise
            records[item["variant"]].append(measurement)
    except KeyboardInterrupt:
        log("interrupted: current run marked cancelled; flushing completed trials")
        interrupted = True
    finally:
        try:
            write_outputs(
                out_root, manifest,
                [v["name"] for v in manifest["variants"]], records, resolved,
            )
        except OSError as error:
            print(f"Failed to flush artifacts: {error}", file=sys.stderr)
            return 1
    return 130 if interrupted else 0


if __name__ == "__main__":
    sys.exit(main())
