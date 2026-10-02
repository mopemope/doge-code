#!/usr/bin/env python3
"""Run repository checks with retained logs, compact output, and zero-test detection."""

import argparse
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import time
import tomllib

ROOT = Path(__file__).resolve().parents[1]
TEST_RESULT = re.compile(r"test result: \w+\. (\d+) passed; (\d+) failed;")
UNITTEST_RESULT = re.compile(r"^Ran (\d+) tests? in ", re.M)


def executed_tests(output):
    """Count executed Rust tests across test binaries, excluding ignored tests."""
    return sum(int(passed) + int(failed) for passed, failed in TEST_RESULT.findall(output))


def run_check(argv, root, log_path, require_tests=False):
    started = time.monotonic()
    env = dict(os.environ, CARGO_TERM_COLOR="never")
    with log_path.open("w") as log:
        try:
            result = subprocess.run(argv, cwd=root, env=env, stdout=log, stderr=log, check=False)
            code = result.returncode if result.returncode >= 0 else 128 - result.returncode
        except OSError as error:
            log.write(f"Unable to start command: {error}\n")
            code = 127
    output = log_path.read_text(errors="replace")
    if require_tests == "unittest":
        count = sum(int(value) for value in UNITTEST_RESULT.findall(output))
    else:
        count = executed_tests(output) if require_tests else None
    if code == 0 and require_tests and count == 0:
        code = 2
        output += "\nNo executed test result was detected; a zero-match/ignored-only run is not verification.\n"
        with log_path.open("a") as log:
            log.write(output.splitlines()[-1] + "\n")
    label = shlex.join(argv)
    elapsed = time.monotonic() - started
    if code:
        print(f"FAIL ({code}) {label}; log: {log_path}", flush=True)
        print(output[-2500:], file=sys.stderr, flush=True)
    else:
        details = f", {count} executed tests" if count is not None else ""
        print(f"PASS {label} ({elapsed:.1f}s{details})", flush=True)
    return code


def commands(mode, test_filter, root):
    if mode == "guidance":
        return [([sys.executable, "scripts/check-agent-guidance.py"], False),
                ([sys.executable, "-m", "unittest", "discover", "-s", "scripts/tests",
                  "-p", "test_*.py"], "unittest")]
    if mode == "test":
        return [(["cargo", "test", "--locked", test_filter], True)]
    if mode == "rust":
        return [(["cargo", "fmt", "--all", "--check"], False),
                (["cargo", "clippy", "--locked", "--all-targets", "--all-features",
                  "--", "-D", "warnings"], False),
                (["cargo", "test", "--locked"], True)]
    if mode == "msrv":
        manifest = tomllib.loads((root / "Cargo.toml").read_text())
        version = manifest["package"]["rust-version"]
        if re.fullmatch(r"\d+\.\d+", version):
            version += ".0"
        return [(["rustup", "run", version, "cargo", "check", "--locked",
                  "--all-targets", "--all-features"], False)]
    if mode == "macos":
        if sys.platform != "darwin":
            raise ValueError("macos checks require macOS; this platform cannot supply macOS evidence")
        return [(["cargo", "test", "--locked", name], True)
                for name in ("tui::", "execution::", "jobs::")]
    if mode == "tui-deps":
        return [([sys.executable, "scripts/check-tui-deps.py"], False)]
    raise ValueError(f"Unknown mode: {mode}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("guidance", "test", "rust", "msrv", "macos", "tui-deps"))
    parser.add_argument("filter", nargs="?", help="required Cargo test-name filter in test mode")
    parser.add_argument("--log-dir", type=Path, help="default: a new directory under the system temp directory")
    args = parser.parse_args()
    if (args.mode == "test") != bool(args.filter):
        parser.error("provide a nonempty filter only in test mode")
    if args.filter and args.filter.startswith("-"):
        parser.error("the test filter must not be a Cargo option")
    try:
        checks = commands(args.mode, args.filter, ROOT)
    except (ValueError, KeyError, OSError, tomllib.TOMLDecodeError) as error:
        parser.error(str(error))
    log_dir = args.log_dir or Path(tempfile.mkdtemp(prefix="dgc-verify-"))
    log_dir.mkdir(parents=True, exist_ok=True)
    print(f"Logs: {log_dir.resolve()}", flush=True)
    for index, (argv, require_tests) in enumerate(checks, 1):
        code = run_check(argv, ROOT, log_dir / f"{index:02d}-{args.mode}.log", require_tests)
        if code:
            return code
    return 0


if __name__ == "__main__":
    sys.exit(main())
