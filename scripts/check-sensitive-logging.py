#!/usr/bin/env python3
"""Reject raw-content identifiers in diagnostic logging of prompt and agent text.

Scope (the code that carries prompts, agent output, and permission requests):
  crates/dgc-daemon/src/provider/**
  crates/dgc-daemon/src/agent.rs and crates/dgc-daemon/src/agent/**
  crates/dgc-cli/src/tui/**

Macros:
  trace!, debug!, info!, warn!, error!

The check extracts tracing macro invocations with parenthesis matching
(handling string literals and comments), strips literals/comments from
the invocation, then looks for exact forbidden identifiers. Safe
metadata such as `message_count`, `prompt_tokens`, `content_chars`,
`tool_result_count` never matches because matching is exact-token.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# Paths relative to the repository root; each is a file or a directory.
SENSITIVE_SCOPE = [
    Path("crates/dgc-daemon/src/provider"),
    Path("crates/dgc-daemon/src/agent.rs"),
    Path("crates/dgc-daemon/src/agent"),
    Path("crates/dgc-cli/src/tui"),
]

MACROS = ("trace", "debug", "info", "warn", "error")

# Exact forbidden identifiers. Safe suffixed metadata (`message_count`,
# `prompt_tokens`, `content_chars`, `content_bytes`, `content_present`,
# `tool_result_count`, `body_bytes`, ...) never matches.
FORBIDDEN = {
    "payload",
    "response_chunk",
    "response_content",
    "appended_content",
    "provided_content",
    "raw_response",
    "encrypted_content",
    "tool_result",
    "tool_output",
    "arguments",
    "prompt",
    "messages",
    "content",
    "endpoint",
    "url",
    "body",
    "detail",
    "text",
}

TOKEN_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
MACRO_RE = re.compile(r"\b(trace|debug|info|warn|error)!\s*")


def strip_literals_and_comments(source: str) -> str:
    """Replace string/char literals and comments with spaces (length-preserving)."""
    out = list(source)
    i = 0
    n = len(source)
    while i < n:
        c = source[i]
        # Line comment
        if c == "/" and i + 1 < n and source[i + 1] == "/":
            j = i
            while j < n and source[j] != "\n":
                out[j] = " "
                j += 1
            i = j
            continue
        # Block comment
        if c == "/" and i + 1 < n and source[i + 1] == "*":
            j = i
            out[j] = " "
            out[j + 1] = " "
            j += 2
            while j < n:
                if source[j] == "*" and j + 1 < n and source[j + 1] == "/":
                    out[j] = " "
                    out[j + 1] = " "
                    j += 2
                    break
                if source[j] != "\n":
                    out[j] = " "
                j += 1
            i = j
            continue
        # Raw string r"..." / r#"..."# / r##"..."##
        if c == "r" and i + 1 < n and (source[i + 1] == '"' or source[i + 1] == "#"):
            j = i + 1
            hashes = 0
            while j < n and source[j] == "#":
                hashes += 1
                j += 1
            if j < n and source[j] == '"':
                out[i] = " "
                for k in range(i + 1, j + 1):
                    out[k] = " "
                j += 1
                closer = '"' + ("#" * hashes)
                while j < n:
                    if source.startswith(closer, j):
                        for k in range(j, j + len(closer)):
                            out[k] = " "
                        j += len(closer)
                        break
                    if source[j] != "\n":
                        out[j] = " "
                    j += 1
                i = j
                continue
        # Double-quoted string
        if c == '"':
            out[i] = " "
            j = i + 1
            while j < n:
                if source[j] == "\\" and j + 1 < n:
                    out[j] = " "
                    out[j + 1] = " "
                    j += 2
                    continue
                if source[j] == '"':
                    out[j] = " "
                    j += 1
                    break
                if source[j] != "\n":
                    out[j] = " "
                j += 1
            i = j
            continue
        # Char literal
        if c == "'":
            out[i] = " "
            j = i + 1
            while j < n:
                if source[j] == "\\" and j + 1 < n:
                    out[j] = " "
                    out[j + 1] = " "
                    j += 2
                    continue
                if source[j] == "'":
                    out[j] = " "
                    j += 1
                    break
                if source[j] != "\n":
                    out[j] = " "
                j += 1
                # Avoid consuming lifetimes like 'a as code; only treat
                # single-char-ish as literal. If we run too far, stop.
                if j - i > 8:
                    break
            i = j
            continue
        i += 1
    return "".join(out)


def extract_macro_invocations(source: str) -> list[tuple[str, str, int]]:
    """Return [(macro, invocation_inside_parens, line_number)]."""
    # Use a literal-stripped view for paren matching so parens inside
    # strings/comments do not break balance, but keep original for text.
    masked = strip_literals_and_comments(source)
    invocations: list[tuple[str, str, int]] = []
    for match in MACRO_RE.finditer(masked):
        macro = match.group(1)
        # Find opening paren after optional whitespace.
        j = match.end()
        while j < len(masked) and masked[j] in " \t\n\r":
            j += 1
        if j >= len(masked) or masked[j] != "(":
            continue
        depth = 0
        k = j
        while k < len(masked):
            if masked[k] == "(":
                depth += 1
            elif masked[k] == ")":
                depth -= 1
                if depth == 0:
                    break
            k += 1
        if depth != 0:
            continue
        inside = source[j + 1 : k]
        line_no = source.count("\n", 0, match.start()) + 1
        invocations.append((macro, inside, line_no))
    return invocations


def check_invocation(inside: str) -> list[str]:
    cleaned = strip_literals_and_comments(inside)
    tokens = set(TOKEN_RE.findall(cleaned))
    return sorted(tokens & FORBIDDEN)


def collect_scope_files(root: Path) -> list[Path]:
    files: list[Path] = []
    for scope in SENSITIVE_SCOPE:
        # Resolve against the provided root (tests use a fixture root).
        candidate = root / scope
        if candidate.is_file():
            files.append(candidate)
        elif candidate.is_dir():
            files.extend(sorted(candidate.rglob("*.rs")))
    return files


def check_root(root: Path) -> list[str]:
    errors: list[str] = []
    files = collect_scope_files(root)
    if not files:
        # A moved module must not turn the check into a silent pass.
        return ["sensitive logging scope matched no files; update SENSITIVE_SCOPE"]
    for path in files:
        try:
            source = path.read_text()
        except OSError as exc:
            errors.append(f"{path}: unreadable ({exc})")
            continue
        for macro, inside, line_no in extract_macro_invocations(source):
            hits = check_invocation(inside)
            for hit in hits:
                rel = path.relative_to(root) if path.is_relative_to(root) else path
                errors.append(
                    f"{rel}:{line_no}: {macro}! logs forbidden identifier `{hit}`"
                )
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
        f"PASS sensitive logging check "
        f"({len(collect_scope_files(args.root))} files, "
        f"{len(FORBIDDEN)} forbidden identifiers)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
