#!/usr/bin/env python3
"""Shared manifest/case/record helpers for the runtime matched eval harness.

Stdlib only. This module never launches agents, never reads secrets, and
never performs live model calls. Measurement values recorded here are either
provider-measured, locally measured (wall clock, exit codes, file digests),
or human-reviewed; estimates are never mixed into provider usage.
"""

import hashlib
import json
import math
import re
from pathlib import Path, PurePosixPath

MANIFEST_SCHEMA_VERSION = 1
MEASUREMENT_SCHEMA_VERSION = 2

SAFE_NAME_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*\Z")

# Substrings (normalized: lowercase, "_" -> "-") that mark an explicit
# secret-bearing CLI argument. Only applied to dash-prefixed arguments or
# KEY=value assignments so paths like "/secrets/dgc" are not rejected.
SECRET_ARG_SUBSTRINGS = (
    "api-key",
    "api-token",
    "access-token",
    "auth-token",
    "bearer",
    "authorization",
    "client-secret",
    "secret",
    "password",
    "private-key",
)

ENV_ASSIGNMENT_RE = re.compile(
    r"(?i)^[A-Z0-9_]*?(API_KEY|SECRET|PASSWORD|TOKEN|AUTHORIZATION)\s*="
)

ALLOWED_CASE_KEYS = frozenset({
    "case_id",
    "prompt",
    "expected_skills",
    "acceptance",
    "post_checks",
    "workspace_policy",
})

ALLOWED_WORKSPACE_POLICIES = ("read_write", "read_only")

RUN_STATUSES = (
    "completed",
    "partial",
    "failed",
    "timed_out",
    "cancelled",
    "harness_error",
)


def check_safe_name(value, kind):
    """Return an error string when *value* is not a path-safe artifact name."""
    if not isinstance(value, str) or not SAFE_NAME_RE.match(value):
        return (
            f"{kind} must match [A-Za-z0-9][A-Za-z0-9._-]*, "
            f"got {value!r}"
        )
    return None


def normalize_arg(arg):
    return str(arg).lower().replace("_", "-")


def find_secret_arg(agent_command):
    """Return the first secret-bearing argument, or None.

    Only flags (``-``/``--`` prefixed) and KEY=value assignments are
    inspected so ordinary paths are accepted.
    """
    for arg in agent_command:
        text = str(arg)
        if text.startswith("-"):
            normalized = normalize_arg(text)
            # Bare --token / --token=... is a canonical secret flag; longer
            # names such as --token-budget are deliberately not matched.
            flag = normalized.split("=", 1)[0]
            if flag in ("-token", "--token"):
                return text
            for marker in SECRET_ARG_SUBSTRINGS:
                if marker in normalized:
                    return text
        elif ENV_ASSIGNMENT_RE.match(text.strip()):
            return text
    return None


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 64), b""):
            digest.update(chunk)
    return digest.hexdigest()


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def fingerprint_command(command):
    return sha256_bytes(
        json.dumps(list(command), sort_keys=True, allow_nan=False).encode("utf-8")
    )


def _fail(errors, message):
    errors.append(message)


def load_cases(cases_path):
    """Load and validate the eval case file. Returns a list of case dicts."""
    path = Path(cases_path)
    try:
        raw = path.read_text(encoding="utf-8")
    except OSError as error:
        raise ValueError(f"cannot read cases file {path}: {error}")
    try:
        data = json.loads(raw)
    except json.JSONDecodeError as error:
        raise ValueError(f"cases file {path} is not valid JSON: {error}")
    if not isinstance(data, list) or not data:
        raise ValueError(f"cases file {path} must be a nonempty top-level array")
    errors = []
    cases = []
    seen = set()
    for index, case in enumerate(data):
        where = f"{path} case index {index}"
        if not isinstance(case, dict):
            _fail(errors, f"{where}: each case must be a JSON object")
            continue
        unknown = sorted(set(case) - ALLOWED_CASE_KEYS)
        if unknown:
            _fail(errors, f"{where}: unknown case field(s): {', '.join(unknown)}")
        case_id = case.get("case_id")
        name_error = check_safe_name(case.get("case_id"), "case_id")
        if name_error:
            _fail(errors, f"{where}: {name_error}")
        elif case_id in seen:
            _fail(errors, f"{where}: duplicate case_id {case_id!r}")
        else:
            seen.add(case_id)
        if not isinstance(case.get("prompt"), str) or not case["prompt"].strip():
            _fail(errors, f"{where}: prompt must be a nonempty string")
        skills = case.get("expected_skills", [])
        if not isinstance(skills, list) or not all(
            isinstance(item, str) for item in skills
        ):
            _fail(errors, f"{where}: expected_skills must be an array of strings")
        acceptance = case.get("acceptance")
        if (
            not isinstance(acceptance, list)
            or not acceptance
            or not all(isinstance(item, str) and item.strip() for item in acceptance)
        ):
            _fail(errors, f"{where}: acceptance must be a nonempty array of strings")
        policy = case.get("workspace_policy", "read_write")
        if policy not in ALLOWED_WORKSPACE_POLICIES:
            _fail(
                errors,
                f"{where}: workspace_policy must be one of "
                f"{list(ALLOWED_WORKSPACE_POLICIES)}, got {policy!r}",
            )
        checks = case.get("post_checks", [])
        if not isinstance(checks, list):
            _fail(errors, f"{where}: post_checks must be an array")
            checks = []
        else:
            check_names = set()
            for check_index, check in enumerate(checks):
                cwhere = f"{where} post_checks[{check_index}]"
                if not isinstance(check, dict):
                    _fail(errors, f"{cwhere}: must be an object")
                    continue
                if not isinstance(check.get("name"), str) or not check["name"].strip():
                    _fail(errors, f"{cwhere}: name must be a nonempty string")
                elif check_safe_name(check["name"], "post check name"):
                    _fail(errors, f"{cwhere}: {check_safe_name(check['name'], 'x')}")
                unknown_check_keys = set(check) - {"name", "argv", "timeout_seconds", "protected_paths"}
                if unknown_check_keys:
                    _fail(errors, f"{cwhere}: unknown keys: {sorted(unknown_check_keys)}")
                if isinstance(check.get("name"), str):
                    if check["name"] in check_names:
                        _fail(errors, f"{cwhere}: duplicate post check name {check['name']!r}")
                    check_names.add(check["name"])
                protected = check.get("protected_paths", [])
                if not isinstance(protected, list) or not all(
                    isinstance(name, str) and name and "\\" not in name
                    and not PurePosixPath(name).is_absolute()
                    and ".." not in PurePosixPath(name).parts
                    and PurePosixPath(name).parts
                    for name in protected
                ):
                    _fail(errors, f"{cwhere}: protected_paths must contain relative file paths without traversal")
                argv = check.get("argv")
                if (
                    not isinstance(argv, list)
                    or not argv
                    or not all(isinstance(a, str) and a for a in argv)
                ):
                    _fail(
                        errors,
                        f"{cwhere}: argv must be a nonempty array of "
                        "nonempty strings (no shell strings)",
                    )
                timeout = check.get("timeout_seconds", 120)
                if (
                    type(timeout) not in (int, float)
                    or not (timeout > 0)
                    or not math.isfinite(timeout)
                ):
                    _fail(errors, f"{cwhere}: timeout_seconds must be finite and > 0")
        normalized = {
            "case_id": case_id,
            "prompt": case.get("prompt"),
            "expected_skills": skills if isinstance(skills, list) else [],
            "acceptance": acceptance if isinstance(acceptance, list) else [],
            "post_checks": [
                {
                    "name": check.get("name"),
                    "argv": list(check.get("argv", [])),
                    "timeout_seconds": check.get("timeout_seconds", 120),
                    "protected_paths": list(check.get("protected_paths", [])) if isinstance(check.get("protected_paths", []), list) else [],
                }
                for check in checks
                if isinstance(check, dict) and isinstance(check.get("argv"), list)
            ],
            "workspace_policy": policy,
        }
        cases.append(normalized)
    if errors:
        raise ValueError("invalid cases file:\n- " + "\n- ".join(errors))
    return cases


def load_manifest(manifest_path):
    """Load and validate the eval manifest.

    Relative paths resolve against the manifest file's directory.
    Returns a normalized manifest dict with absolute paths.
    """
    path = Path(manifest_path)
    if not path.is_file():
        raise ValueError(f"manifest not found: {path}")
    base_dir = path.resolve().parent
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise ValueError(f"manifest {path} is not valid JSON: {error}")
    if not isinstance(data, dict):
        raise ValueError(f"manifest {path} must be a JSON object")
    errors = []

    if data.get("schema_version") != MANIFEST_SCHEMA_VERSION:
        _fail(
            errors,
            f"schema_version must be {MANIFEST_SCHEMA_VERSION}, "
            f"got {data.get('schema_version')!r}",
        )
    base_ref = data.get("base_ref")
    if not isinstance(base_ref, str) or not base_ref.strip():
        _fail(errors, "base_ref must be a nonempty string")
    trials = data.get("trials")
    if type(trials) is not int or trials < 1:
        _fail(errors, f"trials must be an integer >= 1, got {trials!r}")
    model = data.get("model")
    if not isinstance(model, str) or not model.strip():
        _fail(errors, "model must be a nonempty exact model id string")
    provider = data.get("provider")
    if provider is not None and (
        not isinstance(provider, str) or not provider.strip()
    ):
        _fail(errors, "provider must be a nonempty string when present")
    environment_id = data.get("environment_id")
    if not isinstance(environment_id, str) or not environment_id.strip():
        _fail(errors, "environment_id must be a nonempty string")
    timeout = data.get("timeout_seconds")
    if type(timeout) not in (int, float) or not (timeout > 0) or timeout != timeout:
        _fail(errors, f"timeout_seconds must be > 0, got {timeout!r}")
    grace = data.get("termination_grace_seconds", 10)
    if type(grace) not in (int, float) or not (grace > 0) or grace != grace:
        _fail(errors, f"termination_grace_seconds must be > 0, got {grace!r}")
    seed = data.get("seed", 0)
    if type(seed) is not int:
        _fail(errors, f"seed must be an integer, got {seed!r}")

    cases_rel = data.get("cases")
    if not isinstance(cases_rel, str) or not cases_rel.strip():
        _fail(errors, "cases must be a path string to the eval cases file")
        cases_path = None
    else:
        cases_path = (base_dir / cases_rel).resolve()
        if not cases_path.is_file():
            _fail(errors, f"cases file not found: {cases_path}")

    variants_raw = data.get("variants")
    variants = []
    if not isinstance(variants_raw, list) or not variants_raw:
        _fail(errors, "variants must be a nonempty array")
    else:
        seen_names = set()
        for index, variant in enumerate(variants_raw):
            where = f"variants[{index}]"
            if not isinstance(variant, dict):
                _fail(errors, f"{where}: must be an object")
                continue
            name = variant.get("name")
            name_error = check_safe_name(name, "variant name")
            if name_error:
                _fail(errors, f"{where}: {name_error}")
            elif name in seen_names:
                _fail(errors, f"{where}: duplicate variant name {name!r}")
            else:
                seen_names.add(name)
            command = variant.get("agent_command")
            if (
                not isinstance(command, list)
                or not command
                or not all(isinstance(a, str) and a for a in command)
            ):
                _fail(errors, f"{where}: agent_command must be a nonempty string array")
                command = []
            else:
                secret = find_secret_arg(command)
                if secret is not None:
                    _fail(errors, f"{where}: agent_command contains secret-bearing argument {secret!r}")
                for arg in command:
                    if arg.strip() == "--resume" or arg.strip().startswith("--resume"):
                        _fail(errors, f"{where}: agent_command must not contain --resume")
                # The runner appends --model/--provider/exec/--json itself;
                # pre-existing copies would duplicate or reorder the real
                # invocation, so fail fast instead of producing failed runs.
                for arg in command[1:]:
                    if arg.strip() in ("--model", "--provider", "exec", "--json"):
                        _fail(errors, f"{where}: agent_command must not contain {arg.strip()!r} (added by the runner)")
            config_path = None
            if "config" in variant and variant["config"] is not None:
                if not isinstance(variant["config"], str) or not variant["config"].strip():
                    _fail(errors, f"{where}: config must be a path string when present")
                else:
                    config_path = (base_dir / variant["config"]).resolve()
                    if not config_path.is_file():
                        _fail(errors, f"{where}: config file not found: {config_path}")
            variants.append(
                {"name": name, "agent_command": list(command), "config": config_path}
            )

    api_budget = None
    if data.get("api_budget") is not None and not errors:
        from agent_eval_api_budget import validate_budget
        try:
            api_budget = validate_budget(data["api_budget"], model, provider, variants)
        except (ValueError, OSError) as error:
            _fail(errors, str(error))

    if errors:
        raise ValueError("invalid manifest:\n- " + "\n- ".join(errors))
    return {
        "schema_version": MANIFEST_SCHEMA_VERSION,
        "api_budget": api_budget,
        "manifest_path": str(path.resolve()),
        "manifest_dir": str(base_dir),
        "base_ref": base_ref.strip(),
        "cases_path": str(cases_path),
        "trials": trials,
        "model": model.strip(),
        "provider": provider.strip() if isinstance(provider, str) else None,
        "environment_id": environment_id.strip(),
        "timeout_seconds": timeout,
        "termination_grace_seconds": grace,
        "seed": seed,
        "variants": variants,
    }


def build_agent_argv(variant, model, provider, prompt):
    """Build the child argv: <agent_command> --model M [--provider P] exec P --json."""
    argv = list(variant["agent_command"])
    argv += ["--model", model]
    if provider:
        argv += ["--provider", provider]
    argv += ["exec", prompt, "--json"]
    return argv


def execution_matrix(manifest, cases, selected=None):
    """Return the serial execution order for variant x case x trial.

    Variants are deterministically interleaved per (case, trial) with the
    manifest seed so provider/machine load bias is spread across variants.
    """
    import random

    wanted = list(cases)
    if selected:
        ids = {case["case_id"] for case in cases}
        for case_id in selected:
            if case_id not in ids:
                raise ValueError(f"selected case not found: {case_id!r}")
        wanted = [case for case in cases if case["case_id"] in selected]
    rng = random.Random(manifest["seed"])
    order = []
    for case in wanted:
        for trial in range(1, manifest["trials"] + 1):
            names = [variant["name"] for variant in manifest["variants"]]
            rng.shuffle(names)
            for name in names:
                order.append(
                    {"variant": name, "case_id": case["case_id"], "trial": trial}
                )
    return order


def _as_optional_u64(value):
    if value is None:
        return None
    return value if type(value) is int and value >= 0 else None


def usage_is_complete(usage):
    """True only when provider telemetry covers every tracked attempt."""
    if not isinstance(usage, dict):
        return False
    if usage.get("all_tracked_attempts_reported") is not True:
        return False
    unknown = usage.get("unknown_usage_attempts", 0)
    if type(unknown) is not int or unknown != 0:
        return False
    if usage.get("historical_usage_unknown") is True:
        return False
    return True


# (provider subtotal, evaluation total, item report count, known subtotal)
OPTIONAL_USAGE_METRICS = (
    ("cached_tokens", "cached_input_tokens", "cached_usage_records", "known_cached_input_tokens"),
    ("reasoning_tokens", "reasoning_tokens", "reasoning_usage_records", "known_reasoning_tokens"),
    ("cache_write_tokens", "cache_write_tokens", "cache_write_usage_records", "known_cache_write_tokens"),
)


def optional_usage_is_complete(usage, item_records):
    if not usage_is_complete(usage):
        return False
    attempts = _as_optional_u64(usage.get("attempts"))
    records = _as_optional_u64(usage.get("usage_records"))
    return attempts is not None and attempts == records and item_records == records


def extract_exec_telemetry(parsed):
    """Map `dgc exec --json` output to nullable measurement telemetry.

    Provider-reported usage is never mixed with local estimates: when usage
    coverage is incomplete, token fields become None (never 0). Reasoning
    tokens are reported separately and never added to output_tokens twice;
    output_tokens is exactly usage.completion_tokens.
    """
    telemetry = {
        "exec_success": None,
        "exec_status": None,
        "stop_reason": None,
        "input_tokens": None,
        "output_tokens": None,
        "cached_input_tokens": None,
        "reasoning_tokens": None,
        "cache_write_tokens": None,
        "known_cached_input_tokens": None,
        "known_reasoning_tokens": None,
        "known_cache_write_tokens": None,
        "cached_usage_records": None,
        "reasoning_usage_records": None,
        "cache_write_usage_records": None,
        "agent_elapsed_seconds": None,
        "tool_calls": None,
        "iterations": None,
        "request_attempts": None,
        "usage_records": None,
        "unknown_usage_attempts": None,
        "budget_charged_tokens": None,
        "budget_provider_reported_tokens": None,
        "budget_estimated_tokens": None,
        "conversation_length": None,
        "progress": None,
    }
    if not isinstance(parsed, dict):
        return telemetry
    telemetry["exec_success"] = parsed.get("success")
    telemetry["exec_status"] = parsed.get("status")
    telemetry["stop_reason"] = parsed.get("stop_reason")
    if not isinstance(telemetry["stop_reason"], str):
        telemetry["stop_reason"] = None

    usage = parsed.get("usage")
    if usage_is_complete(usage):
        prompt_tokens = _as_optional_u64(usage.get("prompt_tokens"))
        completion_tokens = _as_optional_u64(usage.get("completion_tokens"))
        # Both counters are required for a complete token pair; a missing
        # counter invalidates the pair instead of defaulting to zero.
        if prompt_tokens is not None and completion_tokens is not None:
            telemetry["input_tokens"] = prompt_tokens
            telemetry["output_tokens"] = completion_tokens
        telemetry["unknown_usage_attempts"] = _as_optional_u64(
            usage.get("unknown_usage_attempts")
        )
        telemetry["usage_records"] = _as_optional_u64(usage.get("usage_records"))
    elif isinstance(usage, dict):
        # Preserve coverage counters even when token totals are unknown so
        # aggregates can distinguish "no telemetry" from "zero tokens".
        telemetry["unknown_usage_attempts"] = _as_optional_u64(
            usage.get("unknown_usage_attempts")
        )
        telemetry["usage_records"] = _as_optional_u64(usage.get("usage_records"))

    if isinstance(usage, dict):
        for raw, field, counter, known in OPTIONAL_USAGE_METRICS:
            subtotal = _as_optional_u64(usage.get(raw))
            count = _as_optional_u64(usage.get(counter))
            telemetry[known] = subtotal
            telemetry[counter] = count
            if count is not None and optional_usage_is_complete(usage, count):
                telemetry[field] = subtotal

    budget = parsed.get("budget")
    if isinstance(budget, dict):
        telemetry["tool_calls"] = _as_optional_u64(budget.get("tool_calls"))
        telemetry["iterations"] = _as_optional_u64(budget.get("iterations"))
        telemetry["request_attempts"] = _as_optional_u64(
            budget.get("request_attempts")
        )
        if telemetry["usage_records"] is None:
            telemetry["usage_records"] = _as_optional_u64(
                budget.get("usage_records")
            )
        telemetry["budget_charged_tokens"] = _as_optional_u64(
            budget.get("charged_tokens")
        )
        telemetry["budget_provider_reported_tokens"] = _as_optional_u64(
            budget.get("provider_reported_tokens")
        )
        telemetry["budget_estimated_tokens"] = _as_optional_u64(
            budget.get("estimated_tokens")
        )
        progress = budget.get("progress")
        counters = ("read_tool_calls", "successful_read_tool_calls", "search_tool_calls",
                    "repeated_read_ranges", "verification_tool_calls",
                    "successful_verification_tool_calls")
        firsts = ("first_mutation_tool_call", "first_verification_tool_call")
        if (isinstance(progress, dict)
                and all(_as_optional_u64(progress.get(key)) is not None for key in counters)
                and all(key in progress and (progress[key] is None
                        or (_as_optional_u64(progress[key]) is not None and progress[key] > 0))
                        for key in firsts)):
            # Preserve only content-free contract fields, never arbitrary provider data.
            telemetry["progress"] = {key: progress[key] for key in counters + firsts}
        elapsed_ms = budget.get("elapsed_ms")
        if type(elapsed_ms) in (int, float) and elapsed_ms >= 0 and elapsed_ms == elapsed_ms:
            telemetry["agent_elapsed_seconds"] = float(elapsed_ms) / 1000.0

    conversation_length = parsed.get("conversation_length")
    telemetry["conversation_length"] = _as_optional_u64(conversation_length)
    return telemetry


def map_run_status(telemetry, exit_code, timed_out, harness_error=None):
    """Map exec output plus runner outcome to a stable run_status."""
    if harness_error is not None:
        return "harness_error", telemetry.get("stop_reason")
    if timed_out:
        return "timed_out", telemetry.get("stop_reason")
    success = telemetry.get("exec_success")
    status = telemetry.get("exec_status")
    if success is True and status == "completed":
        return "completed", telemetry.get("stop_reason")
    # Both historical success:true/exit0 and corrected success:false/exit2
    # describe a resumable partial run, rather than an API/runtime failure.
    if status == "partial" and (success is True or success is False):
        return "partial", telemetry.get("stop_reason")
    if success is False:
        return "failed", telemetry.get("stop_reason")
    if success is None:
        # No parseable exec JSON: nonzero exit or empty output is failure,
        # otherwise the harness itself failed to interpret the run.
        if exit_code != 0:
            return "failed", telemetry.get("stop_reason")
        return "harness_error", telemetry.get("stop_reason")
    return "failed", telemetry.get("stop_reason")


def build_settings(manifest, cases_digest):
    settings = {
        "provider": manifest["provider"],
        "environment_id": manifest["environment_id"],
        "timeout_seconds": manifest["timeout_seconds"],
        "cases_digest": cases_digest,
        "seed": manifest["seed"],
    }
    if manifest.get("api_budget") is not None:
        settings["api_budget"] = dict(manifest["api_budget"])
    return settings


def base_measurement(case_id, trial, base_commit, model, settings, variant,
                     variant_metadata):
    """Create a schema-v2 measurement with review fields left unknown (null)."""
    return {
        "schema_version": MEASUREMENT_SCHEMA_VERSION,
        "case_id": case_id,
        "trial": trial,
        "base_commit": base_commit,
        "model": model,
        "settings": dict(settings),
        "variant": variant,
        "variant_metadata": dict(variant_metadata),
        "run_status": "harness_error",
        "stop_reason": None,
        "input_tokens": None,
        "output_tokens": None,
        "cached_input_tokens": None,
        "reasoning_tokens": None,
        "cache_write_tokens": None,
        "known_cached_input_tokens": None,
        "known_reasoning_tokens": None,
        "known_cache_write_tokens": None,
        "cached_usage_records": None,
        "reasoning_usage_records": None,
        "cache_write_usage_records": None,
        "elapsed_seconds": 0.0,
        "agent_elapsed_seconds": None,
        "tool_calls": None,
        "iterations": None,
        "request_attempts": None,
        "usage_records": None,
        "unknown_usage_attempts": None,
        "budget_charged_tokens": None,
        "budget_provider_reported_tokens": None,
        "budget_estimated_tokens": None,
        "conversation_length": None,
        "progress": None,
        # Human review is never fabricated by the runner: null means unknown,
        # not rejected.
        "accepted": None,
        "rework_count": None,
        "contract_violations": None,
        "review_notes": "",
        "machine_findings": [],
        "required_checks_passed": None,
        "evidence_status": "not_collected",
        "artifacts": {},
    }
