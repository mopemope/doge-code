#!/usr/bin/env python3
"""Reject an active legacy Crossterm backend using Cargo's resolved dependency graph."""

import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def main():
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
    command = ["cargo", "tree", "--locked", "--all-features", "--target", "all",
               "--package", manifest["package"]["name"], "--prefix", "none"]
    versions = []
    # Dependency requirements (for example ^0.29) are not package-ID versions.
    # Inspect the actual root dependency and then the full active graph instead.
    for options in (["--depth", "1"], []):
        result = subprocess.run(
            command + options, cwd=ROOT, text=True,
            env=dict(os.environ, CARGO_TERM_COLOR="never"),
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False,
        )
        print(result.stdout, end="")
        if result.returncode:
            return result.returncode
        versions.append(set(re.findall(r"^crossterm v(\S+)", result.stdout, re.M)))
    direct, active = versions
    if len(direct) != 1 or not direct <= active:
        print("Expected one active direct Crossterm dependency", file=sys.stderr)
        return 1
    if any(version.startswith("0.28.") for version in active):
        print("Legacy Crossterm 0.28 is active in the dependency graph", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
