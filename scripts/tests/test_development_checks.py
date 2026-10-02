"""Behavior regressions for verification, routing guards, and measured comparisons."""

from contextlib import redirect_stderr, redirect_stdout
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest.mock import patch


SCRIPTS = Path(__file__).resolve().parents[1]


def load_module(name, filename):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


verify = load_module("verify", "verify.py")
guidance = load_module("guidance", "check-agent-guidance.py")
evals = load_module("evals", "compare-agent-evals.py")
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

    def test_msrv_follows_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_text('[package]\nrust-version = "1.88"\n')
            command = verify.commands("msrv", None, root)[0][0]
            self.assertEqual(command[:3], ["rustup", "run", "1.88.0"])
            self.assertIn("--locked", command)

    def test_python_discovery_zero_tests_is_not_verification(self):
        requirement = verify.commands("guidance", None, SCRIPTS.parent)[1][1]
        code, _ = self.run_fixture("Ran 0 tests in 0.000s\n\nOK", require_tests=requirement)
        self.assertEqual(code, 2)

    def test_python_discovery_counts_actual_tests(self):
        code, _ = self.run_fixture("Ran 3 tests in 0.005s\n\nOK", require_tests="unittest")
        self.assertEqual(code, 0)


class TuiDependencyTests(unittest.TestCase):
    def run_graph(self, direct, full, requirement="^0.29"):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_text(
                f'[package]\nname = "fixture"\n[dependencies]\ncrossterm = "{requirement}"\n'
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


class GuidanceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.write("src/llm/tool_def.rs", "pub fn default_tools_def() -> Vec<ToolDef> {\n"
                   "vec![tools::read::tool_def(),]\n}\n")
        self.write("src/tools/read.rs", 'pub fn tool_def() -> ToolDef {\n'
                   'ToolDef { function: ToolFunctionDef { name: "fs_read".to_string(), } }\n}\n')
        self.write("src/llm/tool_execution/dispatch.rs", 'match name {\n'
                   '"fs_read" => fs::fs_read(runtime, args_val).await,\n}\n')
        self.write("README.md", "## 🛠️ Tools and Commands\n- `fs_read`: Read files\n\n## Other\n")
        self.write("docs/ai/tool-routing-exceptions.json", json.dumps({
            "dispatch_only": {}, "runtime_tools": {}, "readme_commands": {}
        }))

    def write(self, path, content):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content)

    def test_consistent_catalog(self):
        errors, count = guidance.check_catalog(self.root)
        self.assertEqual(errors, [])
        self.assertEqual(count, 1)

    def test_missing_dispatch_is_detected(self):
        self.write("src/llm/tool_execution/dispatch.rs", "match name {}\n")
        errors, _ = guidance.check_catalog(self.root)
        self.assertTrue(any("fs_read" in error for error in errors))

    def test_missing_readme_is_detected(self):
        self.write("README.md", "## 🛠️ Tools and Commands\n\n## Other\n")
        errors, _ = guidance.check_catalog(self.root)
        self.assertTrue(any("README" in error for error in errors))

    def test_duplicate_schema_is_detected(self):
        self.write("src/llm/tool_def.rs", "pub fn default_tools_def() -> Vec<ToolDef> {\n"
                   "vec![tools::read::tool_def(), tools::read::tool_def(),]\n}\n")
        errors, _ = guidance.check_catalog(self.root)
        self.assertTrue(any("Duplicate schema" in error for error in errors))

    def test_internal_route_requires_explicit_exception(self):
        self.write("src/llm/tool_execution/dispatch.rs", 'match name {\n'
                   '"fs_read" => fs::fs_read(runtime, args_val).await,\n'
                   '"internal" => tools::internal(runtime, args_val).await,\n}\n')
        errors, _ = guidance.check_catalog(self.root)
        self.assertTrue(any("internal" in error for error in errors))
        self.write("docs/ai/tool-routing-exceptions.json", json.dumps({
            "dispatch_only": {"internal": "An intentional internal fixture route"},
            "runtime_tools": {}, "readme_commands": {}
        }))
        self.assertEqual(guidance.check_catalog(self.root)[0], [])

    def test_unsupported_registry_syntax_fails_closed(self):
        self.write("src/llm/tool_def.rs", "vec![make_dynamic_tools()]\n")
        with self.assertRaises(ValueError):
            guidance.check_catalog(self.root)

    def test_constant_schema_name_is_resolved(self):
        self.write("src/tools/read.rs", 'pub const NAME: &str = "fs_read";\n'
                   'pub fn tool_def() -> ToolDef {\n'
                   'ToolDef { function: ToolFunctionDef { name: NAME.to_string(), } }\n}\n')
        self.assertEqual(guidance.check_catalog(self.root)[0], [])

    def test_broken_or_outside_link_is_detected(self):
        self.write("docs/guide.md", "[missing](gone.md)\n[outside](../../elsewhere.md)\n")
        self.assertEqual(len(guidance.check_links(self.root / "docs/guide.md", self.root)), 2)

    def test_host_links_must_target_same_canonical_skill(self):
        self.write("AGENTS.md", "[Skill](docs/ai/skills/dgc-test/SKILL.md)\n")
        self.write("CLAUDE.md", "@AGENTS.md\n")
        self.write("docs/tool-output-contract.md", "Contract\n")
        self.write("docs/ai/skills/dgc-test/SKILL.md", "---\nname: dgc-test\n"
                   "description: Verify a scoped dgc fixture.\n---\n")
        for host in guidance.HOSTS:
            directory = self.root / host
            directory.mkdir(parents=True)
            (directory / "dgc-test").symlink_to("../../docs/ai/skills/dgc-test", target_is_directory=True)
        self.assertEqual(guidance.check_skills(self.root), [])
        link = self.root / ".agents/skills/dgc-test"
        link.unlink()
        link.symlink_to("../../docs/ai/skills/missing", target_is_directory=True)
        self.assertTrue(guidance.check_skills(self.root))

    def test_ci_msrv_and_command_drift_are_detected(self):
        version = tomllib.loads((SCRIPTS.parent / "Cargo.toml").read_text())["package"]["rust-version"]
        self.write("Cargo.toml", f'[package]\nrust-version = "{version}"\n')
        toolchain = version + ".0" if version.count(".") == 1 else version
        self.write("scripts/verify.py", (SCRIPTS / "verify.py").read_text())
        source = (SCRIPTS.parent / ".github/workflows/ci.yml").read_text()
        self.write(".github/workflows/ci.yml", source)
        self.assertEqual(guidance.check_ci(self.root), [])
        self.write(".github/workflows/ci.yml", source.replace("-- -D warnings", ""))
        self.assertTrue(guidance.check_ci(self.root))
        self.write(".github/workflows/ci.yml", source.replace(f"@{toolchain}", "@0.0.0"))
        self.assertTrue(guidance.check_ci(self.root))

    def test_skill_description_rejects_invalid_yaml_scalar(self):
        self.write("AGENTS.md", "Guidance\n")
        self.write("CLAUDE.md", "@AGENTS.md\n")
        self.write("docs/tool-output-contract.md", "Contract\n")
        for description in ("dgc: invalid mapping", "[dgc, a list]", "*dgc-alias",
                            " [dgc, a list]", "0xAB", ".5", "true"):
            with self.subTest(description=description):
                self.write("docs/ai/skills/dgc-test/SKILL.md", "---\nname: dgc-test\n"
                           f"description: {description}\n---\n")
                for host in guidance.HOSTS:
                    directory = self.root / host
                    directory.mkdir(parents=True, exist_ok=True)
                    link = directory / "dgc-test"
                    if not link.is_symlink():
                        link.symlink_to("../../docs/ai/skills/dgc-test", target_is_directory=True)
                self.assertTrue(guidance.check_skills(self.root))

    def test_quoted_skill_description_accepts_yaml_punctuation(self):
        self.assertTrue(guidance.valid_description('"dgc: scoped #description"'))
        self.assertTrue(guidance.valid_description("A scoped dgc workflow."))
        self.assertFalse(guidance.valid_description('"unterminated'))


class EvaluationTests(unittest.TestCase):
    def setUp(self):
        self.record = {
            "case_id": "fixture", "trial": 1, "base_commit": "commit",
            "model": "model", "settings": {"effort": "medium"}, "variant": "baseline",
            "accepted": True, "input_tokens": 100, "output_tokens": 20,
            "cached_input_tokens": 50, "elapsed_seconds": 3,
            "tool_calls": 2, "rework_count": 0, "contract_violations": 0,
        }

    def load_fixture(self, records):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "runs.jsonl"
            path.write_text("\n".join(json.dumps(record) for record in records))
            return evals.load_runs(path)

    def test_failed_runs_count_toward_effort_per_success(self):
        failed = dict(self.record, trial=2, accepted=False)
        summary = evals.summarize(self.load_fixture([self.record, failed]))
        self.assertEqual(summary["acceptance_rate"], 0.5)
        self.assertEqual(summary["tokens_per_accepted_run"], 240)
        self.assertEqual(summary["seconds_per_accepted_run"], 6)

    def test_no_success_produces_null_efficiency(self):
        summary = evals.summarize(self.load_fixture([dict(self.record, accepted=False)]))
        self.assertIsNone(summary["tokens_per_accepted_run"])

    def test_invalid_telemetry_and_accepted_violations_rejected(self):
        for update in ({"elapsed_seconds": float("nan")}, {"input_tokens": -1},
                       {"cached_input_tokens": 101}, {"contract_violations": 1},
                       {"accepted": 1}, {"settings": None}):
            with self.subTest(update=update), self.assertRaises(ValueError):
                self.load_fixture([dict(self.record, **update)])

    def test_non_object_and_duplicate_trials_rejected(self):
        for records in ([[]], [self.record, self.record]):
            with self.assertRaises(ValueError):
                self.load_fixture(records)

    def test_comparison_requires_matched_configuration(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            baseline, candidate = root / "baseline.jsonl", root / "candidate.jsonl"
            baseline.write_text(json.dumps(self.record))
            candidate.write_text(json.dumps(dict(self.record, variant="candidate", model="other")))
            command = [sys.executable, str(SCRIPTS / "compare-agent-evals.py"), str(baseline), str(candidate)]
            result = subprocess.run(command, capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 1)
            candidate.write_text(json.dumps(dict(self.record, variant="candidate", input_tokens=80)))
            result = subprocess.run(command, capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0)
            compared = json.loads(result.stdout)
            self.assertEqual(compared["candidate"]["total_tokens"], 100)


if __name__ == "__main__":
    unittest.main()
