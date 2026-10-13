"""Pure validation of operator review/accounting payloads.

No filesystem, process, provider, persistence or historical-evidence access.
Validation never marks a trial accepted or writes a review on its own.
"""
import math


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


def validate_review(evidence, kind, machine_passed):
    if not isinstance(evidence, dict) or set(evidence) != {"accepted", "notes", "execution"}:
        raise ValueError("review fields must be accepted, notes, execution")
    if type(evidence.get("accepted")) is not bool or not isinstance(evidence.get("notes"), str) or not evidence["notes"].strip():
        raise ValueError("review needs an explicit accepted boolean and nonempty notes")
    execution = evidence["execution"]
    if not isinstance(execution, dict) or set(execution) != {"status", "elapsed_seconds", "review_seconds", "rework_seconds", "interventions", "cost"} | ({"identity"} if kind == "model-evaluation" else set()):
        raise ValueError("unexpected or missing execution fields")
    if execution["status"] not in ("completed", "failed", "timed_out", "blocked", "cancelled"):
        raise ValueError("invalid execution status")
    if evidence["accepted"] and (not machine_passed or execution["status"] != "completed"):
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
    if kind == "offline-replay":
        if any(execution["cost"][key] != 0 for key in ("estimated_usd", "confirmed_usd")):
            raise ValueError("offline replay requires explicit zero model cost")
    else:
        identity = execution.get("identity")
        if not isinstance(identity, dict) or any(not isinstance(identity.get(k), str) or not identity[k].strip() for k in ("model", "provider", "configuration", "permissions", "environment")):
            raise ValueError("model evaluation needs actual model/provider/configuration/permissions/environment identity")
