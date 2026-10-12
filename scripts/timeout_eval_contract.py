#!/usr/bin/env python3
"""Version-2 black-box timeout contract. No agent, API, credentials or edits."""
import importlib.util
import math
from pathlib import Path
import sys


def evaluate(parse_timeout):
    accepted = [
        ("integer", 1, 1.0), ("small", 0.000001, 0.000001),
        ("padded_string", " 2.5 ", 2.5), ("exponent", "1e2", 100.0),
        ("negative_exponent", "1e-3", 0.001), ("large_finite", 1e20, 1e20),
        ("smallest_float", 5e-324, 5e-324), ("smallest_string", "5e-324", 5e-324),
        ("largest_float", sys.float_info.max, sys.float_info.max),
        ("large_integer", 2**53, float(2**53)),
    ]
    rejected = [
        ("true", True), ("false", False), ("none", None), ("object", {}),
        ("array", []), ("empty", ""), ("whitespace", " "), ("text", "text"),
        ("zero", 0), ("negative", -1), ("negative_zero", "-0.0"),
        ("nan_string", "nan"), ("NaN_string", "NaN"), ("nan_float", float("nan")),
        ("inf_string", "inf"), ("negative_inf", "-inf"), ("inf_float", float("inf")),
        ("string_overflow", "1e309"), ("positive_integer_overflow", 10**1000),
        ("negative_integer_overflow", -(10**1000)), ("underflow", "1e-9999"),
        ("complex", complex(1, 2)), ("bytes", b"2"),
    ]
    failures = []
    for label, value, expected in accepted:
        try:
            output = parse_timeout(value)
            if type(output) is not float or not math.isfinite(output) or output != expected:
                failures.append(f"{label}: result mismatch")
        except Exception as error:
            failures.append(f"{label}: unexpected {type(error).__name__}")
    for label, value in rejected:
        try:
            parse_timeout(value)
            failures.append(f"{label}: invalid value accepted")
        except ValueError:
            pass
        except Exception as error:
            failures.append(f"{label}: expected ValueError, observed {type(error).__name__}")
    return len(accepted) + len(rejected), failures


def main(argv):
    if len(argv) != 1:
        raise SystemExit("Usage: timeout_eval_contract.py <fixture-workspace>")
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("evaluated_settings", Path(argv[0]) / "settings.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    count, failures = evaluate(module.parse_timeout)
    print(f"timeout contract v2: {count} cases; {len(failures)} failures")
    for failure in failures:
        print(failure)
    return int(bool(failures))


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
