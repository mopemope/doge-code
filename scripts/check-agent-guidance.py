#!/usr/bin/env python3
"""Check shared skill discovery, local links, and static tool routing/documentation parity."""

import argparse
from collections import Counter
import importlib.util
import json
from pathlib import Path
import re
import shlex
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


def check_skills(root):
    errors = []
    base = root / "docs/ai/skills"
    skills = sorted(base.glob("*/SKILL.md"))
    if not skills:
        return ["No canonical skills found under docs/ai/skills"]
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
    docs = [root / "AGENTS.md", root / "README.md", root / "docs/tool-output-contract.md"]
    docs.extend((root / "docs/ai").rglob("*.md"))
    for path in docs:
        errors.extend(check_links(path, root))
    if (root / "CLAUDE.md").read_text().strip() != "@AGENTS.md":
        errors.append("CLAUDE.md must import the shared AGENTS.md")
    return errors


def resolve_tool_name(root, module, function):
    candidates = [root / f"src/tools/{module}.rs"]
    candidates.extend(sorted((root / f"src/tools/{module}").rglob("*.rs")))
    definitions = []
    pattern = re.compile(rf"^pub fn {re.escape(function)}\(\)\s*->\s*ToolDef\s*\{{", re.M)
    for path in candidates:
        if not path.is_file():
            continue
        source = path.read_text()
        match = pattern.search(source)
        if match:
            tail = source[match.end():]
            boundary = re.search(r"^pub (?:async )?fn |^#\[cfg\(test\)\]", tail, re.M)
            body = tail[:boundary.start()] if boundary else tail
            field = re.search(r'\bname:\s*("[a-z0-9_]+"|[A-Z][A-Z0-9_]*)\.to_string\(\)', body)
            if not field:
                raise ValueError(f"{module}::{function}: unsupported schema name expression")
            definitions.append(field.group(1))
    if len(definitions) != 1:
        raise ValueError(f"{module}::{function}: expected one schema definition, found {len(definitions)}")
    expression = definitions[0]
    if expression.startswith('"'):
        return json.loads(expression)
    pattern = re.compile(rf'\bconst {expression}:\s*&str\s*=\s*"([a-z0-9_]+)";')
    values = set()
    for path in (root / "src").rglob("*.rs"):
        values.update(pattern.findall(path.read_text()))
    if len(values) != 1:
        raise ValueError(f"{expression}: expected one constant tool name")
    return values.pop()


def check_catalog(root):
    # This deliberately checks the current literal registry/match format. Fail
    # closed on unsupported syntax; it is not a Rust parser or a behavior proof.
    registry = (root / "src/llm/tool_def.rs").read_text().split("#[cfg(test)]", 1)[0]
    inventory = re.search(r"vec!\[([^]]+)\]", registry, re.S)
    if not inventory:
        raise ValueError("default_tools_def inventory not found")
    entries = re.findall(r"tools::([a-z0-9_]+)::([a-z0-9_]+)\(\),?", inventory.group(1))
    remainder = re.sub(r"tools::[a-z0-9_]+::[a-z0-9_]+\(\),?", "", inventory.group(1))
    if remainder.strip() or not entries:
        raise ValueError("unsupported default_tools_def inventory syntax")
    names = [resolve_tool_name(root, module, function) for module, function in entries]
    errors = [f"Duplicate schema name: {name}" for name, count in Counter(names).items() if count > 1]
    source = (root / "src/llm/tool_execution/dispatch.rs").read_text().split("#[cfg(test)]", 1)[0]
    arms = re.findall(r'^\s*"([a-z0-9_]+)"\s*=>', source, re.M)
    errors.extend(f"Duplicate dispatch arm: {name}" for name, count in Counter(arms).items() if count > 1)
    exceptions = json.loads((root / "docs/ai/tool-routing-exceptions.json").read_text())
    expected_keys = {"dispatch_only", "runtime_tools", "readme_commands"}
    if set(exceptions) != expected_keys:
        raise ValueError("unsupported routing exception structure")
    for group in exceptions.values():
        if not isinstance(group, dict) or not all(isinstance(reason, str) and reason.strip() for reason in group.values()):
            raise ValueError("routing exceptions must have nonempty reasons")
    defaults = set(names)
    runtime = set(exceptions["runtime_tools"])
    dispatch_only = set(exceptions["dispatch_only"])
    documented_commands = set(exceptions["readme_commands"])
    if defaults & (runtime | dispatch_only | documented_commands):
        errors.append("A routing exception became a default tool; update the exception classification")
    expected_dispatch = defaults | runtime | dispatch_only
    errors.extend(f"Missing dispatch arm: {name}" for name in sorted(expected_dispatch - set(arms)))
    errors.extend(f"Undeclared dispatch-only tool: {name}" for name in sorted(set(arms) - expected_dispatch))
    # Runtime-supplied schema factories must still exist even though not in defaults.
    for name in runtime:
        if resolve_tool_name(root, name, "tool_def") != name:
            errors.append(f"Runtime schema name mismatch: {name}")
    readme = (root / "README.md").read_text()
    section = readme.split("## 🛠️ Tools and Commands\n", 1)[1].split("\n## ", 1)[0]
    documented = set()
    for line in section.splitlines():
        if line.startswith("- ") and ": " in line:
            documented.update(name for name in re.findall(r"`([^`]+)`", line.split(": ", 1)[0])
                              if re.fullmatch(r"[a-z0-9_]+", name))
    expected_docs = defaults | runtime | documented_commands
    errors.extend(f"Missing README tool entry: {name}" for name in sorted(expected_docs - documented))
    errors.extend(f"Undeclared README tool/command: {name}" for name in sorted(documented - expected_docs))
    return errors, len(names)


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
    version = tomllib.loads((root / "Cargo.toml").read_text())["package"]["rust-version"]
    if re.fullmatch(r"\d+\.\d+", version):
        version += ".0"
    expected = {
        ("cargo", "fmt", "--all", "--check"),
        ("cargo", "clippy", "--locked", "--all-targets", "--all-features", "--", "-D", "warnings"),
        ("cargo", "test", "--locked"),
        ("rustup", "run", version, "cargo", "check", "--locked", "--all-targets", "--all-features"),
        ("bash", "scripts/verify.sh", "guidance"),
        ("bash", "scripts/verify.sh", "tui-deps"),
    }
    expected.update(("cargo", "test", "--locked", name) for name in ("tui::", "execution::", "jobs::"))
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args()
    try:
        errors = check_skills(args.root)
        catalog_errors, count = check_catalog(args.root)
        errors.extend(catalog_errors)
        errors.extend(check_ci(args.root))
    except (OSError, ValueError, IndexError) as error:
        print(f"Guidance check failed: {error}", file=sys.stderr)
        return 1
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"PASS shared Skills, local links, {count} tool routes/README entries, and CI command agreement")
    return 0


if __name__ == "__main__":
    sys.exit(main())
