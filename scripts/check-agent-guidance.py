#!/usr/bin/env python3
"""Check shared skill discovery, local links, CLAUDE.md, and CI/wrapper/guide agreement."""

import argparse
import importlib.util
import json
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tomllib
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]
HOSTS = (".agents/skills", ".claude/skills", ".opencode/skill")


def valid_description(value):
    """Accept the documented safe subset of single-line YAML string scalars."""
    value = value.strip()
    if value.startswith('"'):
        try:
            parsed = json.loads(value)
            return isinstance(parsed, str) and bool(parsed.strip())
        except ValueError:
            return False
    # Reject YAML collections, aliases/tags, implicit booleans/numbers, mapping
    # separators, and comments. Quoting such descriptions avoids host ambiguity.
    return bool(value) and value[0] not in "-?:,[]{}#&*!|>'%@`" and \
        not re.search(r":(?:\s|$)|(?:^|\s)#", value) and \
        value.lower() not in {"null", "true", "false", "yes", "no", "on", "off", "~"} and \
        not re.match(r"[-+]?\d|\.\d|[+-]?\.(?:inf|nan)$", value, re.I)


def check_links(path, root):
    text = re.sub(r"```.*?```", "", path.read_text(), flags=re.S)
    errors = []
    for target in re.findall(r"\[[^\]]*\]\(([^)]+)\)", text):
        target = target.strip().strip("<>")
        parsed = urlsplit(target)
        if parsed.scheme or not parsed.path:
            continue
        resolved = (path.parent / unquote(parsed.path)).resolve()
        if not resolved.is_relative_to(root.resolve()) or not resolved.exists():
            errors.append(f"{path.relative_to(root)}: broken/outside local link {target}")
    return errors


def tracked_files(root):
    """Return the repository-relative tracked paths when `root` is a git top level, else None."""
    try:
        top = subprocess.run(["git", "-C", str(root), "rev-parse", "--show-toplevel"],
                             capture_output=True, text=True, check=False)
        if top.returncode or Path(top.stdout.strip()).resolve() != root.resolve():
            return None
        listed = subprocess.run(["git", "-C", str(root), "ls-files", "-z"],
                                capture_output=True, text=True, check=False)
    except OSError:
        return None
    if listed.returncode:
        return None
    return set(filter(None, listed.stdout.split("\0")))


def guidance_documents(root):
    """Markdown guidance whose local links must resolve.

    In a git checkout only tracked files count: CI sees exactly those, and a
    developer's never-committed local notes are not repository guidance.
    """
    required = [root / "AGENTS.md", root / "README.md"]
    optional = [root / "docs/tool-output-contract.md"]
    optional.extend(sorted((root / "docs/ai").rglob("*.md")))
    optional = [path for path in optional if path.is_file()]
    tracked = tracked_files(root)
    if tracked is not None:
        optional = [path for path in optional if path.relative_to(root).as_posix() in tracked]
    return required + optional


def check_skills(root):
    """Check canonical skills (zero is fine), their host links, guidance links, and CLAUDE.md."""
    errors = []
    base = root / "docs/ai/skills"
    skills = sorted(base.glob("*/SKILL.md"))
    names = []
    for path in skills:
        text = path.read_text()
        # Repo skills intentionally use two single-line scalar metadata fields.
        header = re.match(r"\A---\nname: ([a-z0-9]+(?:-[a-z0-9]+)*)\n"
                          r"description: ([^\n]+)\n---\n", text)
        if not header:
            errors.append(f"{path.relative_to(root)}: expected name and description scalar frontmatter")
            continue
        name, description = header.groups()
        names.append(name)
        if name != path.parent.name or len(name) > 64 or not valid_description(description):
            errors.append(f"{path.relative_to(root)}: invalid name/description")
        for host in HOSTS:
            link = root / host / name
            if not link.is_symlink() or link.resolve() != path.parent.resolve():
                errors.append(f"{host}/{name}: must link to docs/ai/skills/{name}")
    for host in HOSTS:
        directory = root / host
        if directory.is_dir():
            for entry in directory.iterdir():
                if entry.name not in names:
                    errors.append(f"{host}/{entry.name}: stale or noncanonical skill entry")
    if len(set(names)) != len(names):
        errors.append("Duplicate canonical skill names")
    for path in guidance_documents(root):
        errors.extend(check_links(path, root))
    if (root / "CLAUDE.md").read_text().strip() != "@AGENTS.md":
        errors.append("CLAUDE.md must import the shared AGENTS.md")
    return errors


def check_ci(root):
    """Verify wrapper/CI command agreement without requiring a YAML dependency."""
    source = (root / ".github/workflows/ci.yml").read_text()
    errors = []
    # Validate the simple single-line run commands used by this repository.
    commands = set()
    for value in re.findall(r"^\s*(?:-\s+)?run: (.+)$", source, re.M):
        if value.startswith('"'):
            value = json.loads(value)
        commands.add(tuple(shlex.split(value)))
    # The root manifest is a virtual workspace; its package table holds the MSRV.
    version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["rust-version"]
    if re.fullmatch(r"\d+\.\d+", version):
        version += ".0"
    expected = {
        ("cargo", "fmt", "--all", "--check"),
        ("cargo", "clippy", "--locked", "--workspace", "--all-targets", "--all-features",
         "--", "-D", "warnings"),
        ("cargo", "test", "--locked", "--workspace"),
        ("rustup", "run", version, "cargo", "check", "--locked", "--workspace", "--all-targets",
         "--all-features"),
        ("bash", "scripts/verify.sh", "guidance"),
        ("bash", "scripts/verify.sh", "tui-deps"),
        # macOS focused tests run through the wrapper so zero-test filters fail.
        ("bash", "scripts/verify.sh", "macos"),
    }
    for command in sorted(expected - commands):
        errors.append(f"CI missing expected verification command: {shlex.join(command)}")
    if f"dtolnay/rust-toolchain@{version}" not in source:
        errors.append(f"CI MSRV toolchain must match Cargo.toml rust-version ({version})")
    toolchain = tomllib.loads((root / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    if not re.fullmatch(r"\d+\.\d+\.\d+", toolchain):
        errors.append("Development toolchain must pin a stable Rust release")
    ci_toolchains = re.findall(r"dtolnay/rust-toolchain@([^\s]+)", source)
    if any(value not in (version, toolchain) for value in ci_toolchains):
        errors.append("CI development toolchain must match rust-toolchain.toml")
    # Ensure the wrapper's Rust gate agrees with the commands checked above.
    module_spec = importlib.util.spec_from_file_location("dgc_verify", root / "scripts/verify.py")
    module = importlib.util.module_from_spec(module_spec)
    module_spec.loader.exec_module(module)
    for argv, _ in module.commands("rust", None, root):
        if tuple(argv) not in commands:
            errors.append(f"Wrapper/CI command mismatch: {shlex.join(argv)}")
    return errors


def check_workflow_doc(root):
    """Require the workflow guide to quote each command the wrapper's Rust gate runs."""
    text = (root / "docs/ai/workflow.md").read_text()
    module_spec = importlib.util.spec_from_file_location("dgc_verify_doc", root / "scripts/verify.py")
    module = importlib.util.module_from_spec(module_spec)
    module_spec.loader.exec_module(module)
    return [f"docs/ai/workflow.md does not list Rust gate command: {shlex.join(argv)}"
            for argv, _ in module.commands("rust", None, root)
            if f"`{shlex.join(argv)}`" not in text]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args()
    try:
        errors = check_skills(args.root)
        errors.extend(check_ci(args.root))
        errors.extend(check_workflow_doc(args.root))
    except (OSError, ValueError, IndexError, KeyError, tomllib.TOMLDecodeError) as error:
        print(f"Guidance check failed: {error}", file=sys.stderr)
        return 1
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    skills = len(list((args.root / "docs/ai/skills").glob("*/SKILL.md")))
    print(f"PASS {skills} shared Skills, local links, CLAUDE.md, and CI/wrapper/guide command agreement")
    return 0


if __name__ == "__main__":
    sys.exit(main())
