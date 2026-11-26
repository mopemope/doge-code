"""Behavior regressions for the verification wrapper and development checks."""

from contextlib import redirect_stderr, redirect_stdout
import importlib.util
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest.mock import patch


SCRIPTS = Path(__file__).resolve().parents[1]
VIRTUAL_MANIFEST = ('[workspace]\nmembers = ["crates/*"]\nresolver = "3"\n\n'
                    '[workspace.package]\nedition = "2024"\nrust-version = "{version}"\n'
                    'version = "0.1.0"\n')
# macOS focused tests: process groups, the unix socket server (permissions and
# peer credentials on every connection), daemon autostart, and the terminal UI.
MACOS_FILTERS = ("process_group", "terminate_", "socket", "hello", "autostart", "tui::")


def load_module(name, filename):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


verify = load_module("verify", "verify.py")
guidance = load_module("guidance", "check-agent-guidance.py")
tui_deps = load_module("tui_deps", "check-tui-deps.py")


class VerificationTests(unittest.TestCase):
    def run_fixture(self, output, exit_code=0, require_tests=True):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            log = root / "check.log"
            argv = [sys.executable, "-c",
                    f"import sys; print({output!r}); sys.exit({exit_code})"]
            with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                code = verify.run_check(argv, root, log, require_tests)
            return code, log.read_text()

    def test_zero_and_ignored_only_are_not_verification(self):
        for ignored in (0, 3):
            code, _ = self.run_fixture(
                f"test result: ok. 0 passed; 0 failed; {ignored} ignored; 0 measured; 12 filtered out;"
            )
            self.assertEqual(code, 2)

    def test_multiple_binaries_allow_zero_in_one_if_tests_execute(self):
        code, _ = self.run_fixture(
            "test result: ok. 0 passed; 0 failed; 0 ignored;\n"
            "test result: ok. 2 passed; 0 failed; 0 ignored;"
        )
        self.assertEqual(code, 0)

    def test_compile_failure_preserves_exit_and_log(self):
        code, output = self.run_fixture("compile/startup diagnostic", exit_code=101)
        self.assertEqual(code, 101)
        self.assertIn("compile/startup diagnostic", output)

    def test_failed_tests_preserve_exit(self):
        code, _ = self.run_fixture("test result: FAILED. 0 passed; 1 failed; 0 ignored;", 101)
        self.assertEqual(code, 101)

    def test_unrecognized_test_output_is_not_passed(self):
        code, _ = self.run_fixture("All checks allegedly passed")
        self.assertEqual(code, 2)

    def test_non_test_checks_do_not_require_rust_test_output(self):
        code, _ = self.run_fixture("documentation checks passed", require_tests=False)
        self.assertEqual(code, 0)

    def test_msrv_follows_virtual_workspace_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_text(VIRTUAL_MANIFEST.format(version="1.88"))
            command = verify.commands("msrv", None, root)[0][0]
            self.assertEqual(command[:3], ["rustup", "run", "1.88.0"])
            self.assertIn("--locked", command)
            self.assertIn("--workspace", command)

    def test_msrv_rejects_a_manifest_without_workspace_rust_version(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            # The legacy root package no longer exists; it is not a fallback.
            (root / "Cargo.toml").write_text('[package]\nrust-version = "1.88"\n')
            with self.assertRaises(KeyError):
                verify.commands("msrv", None, root)

    def test_macos_mode_runs_the_orchestrator_platform_tests(self):
        with patch.object(verify.sys, "platform", "darwin"):
            checks = verify.commands("macos", None, SCRIPTS.parent)
        self.assertEqual([argv[-1] for argv, _ in checks], list(MACOS_FILTERS))
        for argv, require_tests in checks:
            self.assertEqual(argv[:4], ["cargo", "test", "--locked", "--workspace"], argv)
            self.assertIs(require_tests, True)

    def test_macos_mode_refuses_other_platforms(self):
        with patch.object(verify.sys, "platform", "linux"), self.assertRaises(ValueError):
            verify.commands("macos", None, SCRIPTS.parent)

    def test_python_discovery_zero_tests_is_not_verification(self):
        commands = verify.commands("guidance", None, SCRIPTS.parent)
        requirement = next(value for _, value in commands if value == "unittest")
        code, _ = self.run_fixture("Ran 0 tests in 0.000s\n\nOK", require_tests=requirement)
        self.assertEqual(code, 2)

    def test_python_discovery_counts_actual_tests(self):
        code, _ = self.run_fixture("Ran 3 tests in 0.005s\n\nOK", require_tests="unittest")
        self.assertEqual(code, 0)

    def test_python_discovery_does_not_count_skipped_tests(self):
        code, _ = self.run_fixture("Ran 2 tests in 0.001s\n\nOK (skipped=2)", require_tests="unittest")
        self.assertEqual(code, 2)
        code, _ = self.run_fixture("Ran 3 tests in 0.001s\n\nOK (skipped=2)", require_tests="unittest")
        self.assertEqual(code, 0)

    def test_rust_and_test_modes_cover_workspace(self):
        for mode, test_filter in (("rust", None), ("test", "x")):
            argvs = [argv for argv, _ in verify.commands(mode, test_filter, SCRIPTS.parent)]
            cargo_tests = [argv for argv in argvs if argv[:2] == ["cargo", "test"]]
            self.assertTrue(cargo_tests)
            self.assertTrue(all("--workspace" in argv for argv in cargo_tests), cargo_tests)
            clippy = [argv for argv in argvs if argv[:2] == ["cargo", "clippy"]]
            self.assertTrue(all("--workspace" in argv for argv in clippy), clippy)
        msrv = verify.commands("msrv", None, SCRIPTS.parent)[0][0]
        self.assertIn("--workspace", msrv)

    def test_guidance_runs_checkers_before_script_discovery(self):
        commands = verify.commands("guidance", None, SCRIPTS.parent)
        labels = [" ".join(argv) for argv, _ in commands]
        positions = {}
        for marker in ("check-agent-guidance.py", "check-sensitive-logging.py"):
            matches = [index for index, label in enumerate(labels) if marker in label]
            self.assertEqual(len(matches), 1, marker)
            positions[marker] = matches[0]
        # The legacy-only test-integrity checker was removed with src/.
        self.assertFalse(any("check-test-integrity.py" in label for label in labels), labels)
        discovery = [
            index for index, (_, value) in enumerate(commands) if value == "unittest"
        ]
        self.assertEqual(len(discovery), 1)
        self.assertLess(max(positions.values()), discovery[0])
        for argv, _ in commands:
            script = next((arg for arg in argv if arg.startswith("scripts/")), None)
            if script and script.endswith(".py"):
                self.assertTrue((SCRIPTS.parent / script).is_file(), script)


class TuiDependencyTests(unittest.TestCase):
    def run_graph(self, direct, full, requirement="^0.29"):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            # A virtual workspace: there is no root package to inspect.
            (root / "Cargo.toml").write_text(
                VIRTUAL_MANIFEST.format(version="1.94")
                + f'[workspace.dependencies]\ncrossterm = "{requirement}"\n'
            )
            calls = []

            def tree(argv, **kwargs):
                calls.append(argv)
                if "--depth" in argv:
                    output = direct
                elif "--invert" in argv or "-i" in argv:
                    if any("^" in arg for arg in argv):
                        return subprocess.CompletedProcess(argv, 101, "invalid package ID specification")
                    output = full
                else:
                    output = full
                return subprocess.CompletedProcess(argv, 0, output)

            with patch.object(tui_deps, "ROOT", root, create=True), \
                 patch.object(tui_deps, "__file__", str(root / "scripts/check-tui-deps.py")), \
                 patch.object(tui_deps.subprocess, "run", side_effect=tree), \
                 redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                result = tui_deps.main()
            return result, calls

    def test_cargo_requirement_is_not_used_as_package_id(self):
        code, calls = self.run_graph("fixture v0.1.0\ncrossterm v0.29.0\n", "crossterm v0.29.0\n")
        self.assertEqual(code, 0)
        self.assertFalse(any("crossterm@^" in arg for call in calls for arg in call))

    def test_any_active_legacy_patch_version_is_rejected(self):
        code, _ = self.run_graph("crossterm v0.29.0\n", "crossterm v0.29.0\ncrossterm v0.28.2\n")
        self.assertEqual(code, 1)

    def test_graph_is_inspected_for_the_dgc_cli_package(self):
        code, calls = self.run_graph("dgc-cli v0.1.0\ncrossterm v0.29.0\n", "crossterm v0.29.0\n")
        self.assertEqual(code, 0)
        self.assertEqual(len(calls), 2)
        for call in calls:
            self.assertEqual(call[call.index("--package") + 1], "dgc-cli", call)
            self.assertIn("--locked", call)

    def test_missing_direct_crossterm_is_rejected(self):
        code, _ = self.run_graph("dgc-cli v0.1.0\n", "crossterm v0.29.0\n")
        self.assertEqual(code, 1)


class GuidanceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def write(self, path, content):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content)

    def test_broken_or_outside_link_is_detected(self):
        self.write("docs/guide.md", "[missing](gone.md)\n[outside](../../elsewhere.md)\n")
        self.assertEqual(len(guidance.check_links(self.root / "docs/guide.md", self.root)), 2)

    def test_local_agent_host_skills_are_not_guidance(self):
        self.write("AGENTS.md", "[Workflow](docs/ai/workflow.md)\n")
        self.write("CLAUDE.md", "@AGENTS.md\n")
        self.write("README.md", "dgc\n")
        self.write("docs/ai/workflow.md", "Workflow\n")
        self.assertEqual(guidance.check_guidance(self.root), [])
        # Personal skills under an agent host directory are local state, not
        # repository guidance: they must not fail the check.
        for host in (".claude/skills", ".opencode/skill", ".agents/skills"):
            self.write(f"{host}/foo/SKILL.md", "personal\n")
        self.write(".claude/skills/bar/SKILL.md", "---\nname: other\n---\n")
        self.assertEqual(guidance.check_guidance(self.root), [])

    def test_claude_md_must_import_agents_md(self):
        self.write("AGENTS.md", "Guidance\n")
        self.write("README.md", "dgc\n")
        self.write("CLAUDE.md", "Separate guidance\n")
        errors = guidance.check_guidance(self.root)
        self.assertTrue(any("CLAUDE.md" in error for error in errors), errors)

    def test_broken_links_in_guidance_documents_are_detected(self):
        self.write("AGENTS.md", "[Gone](src/main.rs)\n")
        self.write("CLAUDE.md", "@AGENTS.md\n")
        self.write("README.md", "[Gone](elisp/emacs-integration.md)\n")
        self.write("docs/ai/architecture.md", "[Gone](../../src/lib.rs)\n")
        errors = guidance.check_guidance(self.root)
        for name in ("AGENTS.md", "README.md", "docs/ai/architecture.md"):
            self.assertTrue(any(error.startswith(name) for error in errors), (name, errors))

    def test_untracked_notes_are_not_link_checked_in_a_git_checkout(self):
        self.write("AGENTS.md", "Guidance\n")
        self.write("CLAUDE.md", "@AGENTS.md\n")
        self.write("README.md", "dgc\n")
        self.write("docs/ai/tracked.md", "[Gone](../../src/lib.rs)\n")
        git = ["git", "-C", str(self.root), "-c", "user.name=t", "-c", "user.email=t@example.invalid"]
        subprocess.run(git + ["init", "-q"], check=True)
        subprocess.run(git + ["add", "AGENTS.md", "CLAUDE.md", "README.md", "docs/ai/tracked.md"],
                       check=True)
        # A local, never-committed note: CI cannot see it, so it is not guidance.
        self.write("docs/ai/local-note.md", "[Gone](../../src/tools/edit.rs)\n")
        errors = guidance.check_guidance(self.root)
        self.assertTrue(any("docs/ai/tracked.md" in error for error in errors), errors)
        self.assertFalse(any("local-note.md" in error for error in errors), errors)

    def test_ci_msrv_and_command_drift_are_detected(self):
        manifest = tomllib.loads((SCRIPTS.parent / "Cargo.toml").read_text())
        self.assertNotIn("package", manifest, "the root manifest must stay a virtual workspace")
        version = manifest["workspace"]["package"]["rust-version"]
        self.write("Cargo.toml", VIRTUAL_MANIFEST.format(version=version))
        toolchain = version + ".0" if version.count(".") == 1 else version
        self.write("scripts/verify.py", (SCRIPTS / "verify.py").read_text())
        self.write("rust-toolchain.toml", (SCRIPTS.parent / "rust-toolchain.toml").read_text())
        source = (SCRIPTS.parent / ".github/workflows/ci.yml").read_text()
        self.write(".github/workflows/ci.yml", source)
        self.assertEqual(guidance.check_ci(self.root), [])
        dev = tomllib.loads((SCRIPTS.parent / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
        self.write(".github/workflows/ci.yml", source.replace(f"@{dev}", "@stable"))
        self.assertTrue(guidance.check_ci(self.root))
        self.write(".github/workflows/ci.yml", source)
        self.write(".github/workflows/ci.yml", source.replace(f"rustup run {toolchain} cargo check", "cargo check"))
        self.assertTrue(guidance.check_ci(self.root))
        self.write(".github/workflows/ci.yml", source.replace("-- -D warnings", ""))
        self.assertTrue(guidance.check_ci(self.root))
        self.write(".github/workflows/ci.yml", source.replace(f"@{toolchain}", "@0.0.0"))
        self.assertTrue(guidance.check_ci(self.root))
        for workspace_command in ("cargo test --locked --workspace",
                                  "cargo clippy --locked --workspace",
                                  "cargo check --locked --workspace"):
            self.write(".github/workflows/ci.yml", source.replace(
                workspace_command, workspace_command.replace(" --workspace", "")))
            self.assertTrue(guidance.check_ci(self.root), workspace_command)
        self.write(".github/workflows/ci.yml", source.replace(
            "bash scripts/verify.sh macos", 'cargo test --locked tui::'))
        errors = guidance.check_ci(self.root)
        self.assertTrue(any("verify.sh macos" in error for error in errors), errors)

    def test_ci_reads_msrv_from_the_workspace_package(self):
        self.write("scripts/verify.py", (SCRIPTS / "verify.py").read_text())
        self.write("rust-toolchain.toml", (SCRIPTS.parent / "rust-toolchain.toml").read_text())
        source = (SCRIPTS.parent / ".github/workflows/ci.yml").read_text()
        self.write(".github/workflows/ci.yml", source)
        version = tomllib.loads((SCRIPTS.parent / "Cargo.toml").read_text())["workspace"]["package"]["rust-version"]
        self.write("Cargo.toml", VIRTUAL_MANIFEST.format(version=version))
        self.assertEqual(guidance.check_ci(self.root), [])
        # Drifting only the workspace MSRV must be noticed against the CI job.
        self.write("Cargo.toml", VIRTUAL_MANIFEST.format(version="1.80"))
        errors = guidance.check_ci(self.root)
        self.assertTrue(any("1.80.0" in error for error in errors), errors)

    def test_ci_tests_the_whole_workspace_without_legacy_setup(self):
        source = (SCRIPTS.parent / ".github/workflows/ci.yml").read_text()
        self.assertIn("run: cargo test --locked --workspace", source)
        # ripgrep was only needed by the legacy search tool tests.
        self.assertNotIn("ripgrep", source)

    def test_workflow_doc_lists_wrapper_rust_commands(self):
        self.write("scripts/verify.py", (SCRIPTS / "verify.py").read_text())
        self.write("Cargo.toml", (SCRIPTS.parent / "Cargo.toml").read_text())
        source = (SCRIPTS.parent / "docs/ai/workflow.md").read_text()
        self.write("docs/ai/workflow.md", source)
        self.assertEqual(guidance.check_workflow_doc(self.root), [])
        self.write("docs/ai/workflow.md", source.replace(" --workspace", ""))
        errors = guidance.check_workflow_doc(self.root)
        self.assertTrue(any("cargo test --locked --workspace" in error for error in errors), errors)


if __name__ == "__main__":
    unittest.main()
