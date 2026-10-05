#!/usr/bin/env python3
"""Reject false-green HTTP fixture patterns in Rust tests.

Required HTTP fixtures must fail loudly when unavailable. These patterns
turn missing fixtures into vacuous green tests and are rejected:

* ``DOGE_SKIP_HTTPTEST`` in ``src/**/*.rs`` (env-gated test skip).
* ``Skipping httptest-based test`` in ``src/**/*.rs`` (silent-skip marker).
* ``#[ignore]`` on critical HTTP/LLM transport regression tests in the
  scoped modules below (ignored contract coverage).

Only the scoped HTTP/LLM regression contract is checked. Unrelated
``#[ignore]`` uses (slow manual tests, platform-specific tests, ...) are
allowed.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

SKIP_ENV = "DOGE_SKIP_HTTPTEST"
SKIP_MESSAGE = "Skipping httptest-based test"

# Modules whose HTTP/LLM regression contract must never be ignored.
SCOPED_MODULES = [
    Path("src/llm/client_core.rs"),
    Path("src/llm/client_core/network.rs"),
    Path("src/llm/tool_execution/requests.rs"),
]

# Test-name prefixes covered by the critical HTTP/LLM transport contract.
CRITICAL_PREFIXES = (
    "chat_",
    "chat_once_",
    "tool_request_",
    "test_reasoning_",
    "test_first_payload_",
    "test_post_activation_",
)

IGNORE_RE = re.compile(r"^\s*#\[ignore\]")
FN_RE = re.compile(r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*[\(<]")


def rust_sources(root: Path):
    return sorted((root / "src").rglob("*.rs"))


def check_skip_env(root: Path):
    """Reject the fixture-skip environment variable in Rust tests."""
    errors = []
    for path in rust_sources(root):
        try:
            text = path.read_text()
        except OSError:
            continue
        if SKIP_ENV in text:
            errors.append(
                f"{path.relative_to(root)}: contains {SKIP_ENV}; "
                "HTTP fixtures must fail loudly instead of skipping"
            )
    return errors


def check_skip_message(root: Path):
    """Reject the legacy silent-skip log marker in Rust tests."""
    errors = []
    for path in rust_sources(root):
        try:
            text = path.read_text()
        except OSError:
            continue
        if SKIP_MESSAGE in text:
            errors.append(
                f"{path.relative_to(root)}: contains {SKIP_MESSAGE!r}; "
                "silent fixture skips are not allowed"
            )
    return errors


def ignored_critical_test_names(text: str):
    """Return critical test names gated by ``#[ignore]`` in one module."""
    names = []
    lines = text.splitlines()
    for index, line in enumerate(lines):
        if not IGNORE_RE.match(line):
            continue
        for candidate in lines[index + 1 : index + 6]:
            match = FN_RE.match(candidate)
            if match:
                name = match.group(1)
                if name.startswith(CRITICAL_PREFIXES):
                    names.append(name)
                break
            stripped = candidate.strip()
            if stripped and not stripped.startswith("#[") and not stripped.startswith("//"):
                break
    return names


def check_ignored_critical_tests(root: Path):
    """Reject ``#[ignore]`` on critical HTTP/LLM regression tests."""
    errors = []
    for module in SCOPED_MODULES:
        path = root / module
        if not path.is_file():
            continue
        try:
            names = ignored_critical_test_names(path.read_text())
        except OSError:
            continue
        for name in names:
            errors.append(
                f"{module}: critical HTTP test {name} must not be #[ignore]"
            )
    return errors


def check_root(root: Path):
    errors = []
    errors.extend(check_skip_env(root))
    errors.extend(check_skip_message(root))
    errors.extend(check_ignored_critical_tests(root))
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args()
    errors = check_root(args.root)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(
        f"PASS test integrity check "
        f"({len(rust_sources(args.root))} files, "
        f"{len(SCOPED_MODULES)} scoped modules)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
