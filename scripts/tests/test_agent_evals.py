#!/usr/bin/env python3
"""Regression tests for the runtime matched evaluation harness.

Every test uses deterministic fake agents; no network, no live LLM, and no
real provider credentials are required. Real `dgc` binaries are never
executed here.
"""

import importlib.util
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from contextlib import contextmanager
from pathlib import Path


SCRIPTS = Path(__file__).resolve().parents[1]


def load_module(name, filename):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


common = load_module("agent_eval_common", "agent_eval_common.py")
runner = load_module("run_agent_evals", "run-agent-evals.py")
evals = load_module("evals_compare", "compare-agent-evals.py")

NEEDS_GIT = shutil.which("git") is None

FAKE_AGENT = r"""#!/usr/bin/env python3
import json, os, sys, time

args = sys.argv[1:]
log_path = os.environ.get("FAKE_ARGV_LOG")
if log_path:
    with open(log_path, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(args) + "\n")

if len(args) >= 2 and args[0] == "session" and args[1] == "evidence":
    if os.environ.get("FAKE_EVIDENCE_FAIL") == "1":
        sys.stderr.write("evidence boom\n")
        sys.exit(2)
    if os.environ.get("FAKE_EVIDENCE_INVALID") == "1":
        sys.stdout.write("not json evidence\n")
        sys.exit(0)
    sid = args[2] if len(args) > 2 else "unknown"
    json.dump({"session": sid, "format": "json", "redacted": True}, sys.stdout)
    sys.stdout.write("\n")
    sys.exit(0)

# Exec mode: never resume; the harness must not pass --resume.
if any(a == "--resume" or a.startswith("--resume=") for a in args):
    sys.stderr.write("resume is forbidden in eval runs\n")
    sys.exit(42)
if "exec" not in args or "--json" not in args:
    sys.stderr.write("expected `<cmd> exec <prompt> --json` invocation\n")
    sys.exit(43)

sleep_seconds = float(os.environ.get("FAKE_SLEEP_SECONDS", "0"))
if os.environ.get("FAKE_IGNORE_TERM") == "1":
    import signal as _sig
    _sig.signal(_sig.SIGTERM, _sig.SIG_IGN)
if sleep_seconds > 0:
    time.sleep(sleep_seconds)

mode = os.environ.get("FAKE_STDOUT", "")
if mode == "garbage":
    sys.stdout.write("this is not json{{{")
    sys.exit(0)
if mode == "empty":
    sys.exit(0)

names = int(os.environ.get("FAKE_SESSIONS", "1"))
for index in range(names):
    session_dir = os.path.join(".doge", "sessions", f"fake-session-{index}")
    os.makedirs(session_dir, exist_ok=True)
    with open(os.path.join(session_dir, "session.json"), "w") as handle:
        json.dump({"id": f"fake-session-{index}"}, handle)

created = os.environ.get("FAKE_CREATE_FILE")
if created:
    with open(created, "w") as handle:
        handle.write(os.environ.get("FAKE_CREATE_CONTENT", "fake untracked\n"))
if os.environ.get("FAKE_MODIFY_TRACKED") == "1":
    with open("tracked.txt", "a") as handle:
        handle.write("fake agent edit\n")

USAGE_OK = {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120,
            "cached_tokens": 80, "cached_usage_records": 1, "attempts": 1, "usage_records": 1,
            "unknown_usage_attempts": 0, "all_tracked_attempts_reported": True}
BUDGET = {"iterations": 2, "tool_calls": 3, "charged_tokens": 120,
          "elapsed_ms": 1000, "provider_reported_tokens": 120,
          "estimated_tokens": 0, "request_attempts": 1, "usage_records": 1}
if os.environ.get("FAKE_PROGRESS"):
    BUDGET["progress"] = json.loads(os.environ["FAKE_PROGRESS"])
payload = os.environ.get("FAKE_PAYLOAD", "ok")
if payload == "no_usage":
    doc = {"success": True, "status": "completed", "stop_reason": None,
           "budget": dict(BUDGET), "response": "fake",
           "tokens_used": 0, "tools_called": [], "conversation_length": 2}
elif payload == "no_cache":
    usage = dict(USAGE_OK)
    del usage["cached_tokens"]
    doc = {"success": True, "status": "completed", "stop_reason": None,
           "budget": dict(BUDGET), "response": "fake",
           "tokens_used": 120, "usage": usage,
           "tools_called": [], "conversation_length": 2}
elif payload == "partial_optional":
    usage = dict(USAGE_OK, attempts=2, usage_records=2, cached_usage_records=1,
                 prompt_tokens=200, completion_tokens=40, total_tokens=240,
                 reasoning_tokens=30, reasoning_usage_records=1,
                 cache_write_tokens=10, cache_write_usage_records=1)
    budget = dict(BUDGET, request_attempts=2, usage_records=2)
    doc = {"success": True, "status": "completed", "usage": usage,
           "budget": budget, "tools_called": []}
elif payload == "reasoning":
    usage = dict(USAGE_OK)
    usage["reasoning_tokens"] = 50
    usage["reasoning_usage_records"] = 1
    doc = {"success": True, "status": "completed", "stop_reason": None,
           "budget": dict(BUDGET), "response": "fake",
           "tokens_used": 120, "usage": usage,
           "tools_called": [], "conversation_length": 2}
elif payload in ("partial", "partial_incomplete"):
    doc = {"success": True, "status": "partial", "stop_reason": "token_budget",
           "budget": dict(BUDGET), "response": "fake partial",
           "tokens_used": 120, "usage": dict(USAGE_OK),
           "tools_called": [], "conversation_length": 2}
elif payload == "fail":
    doc = {"success": False, "error": "fake llm boom",
           "tokens_used": 5, "usage": dict(USAGE_OK)}
else:
    doc = {"success": True, "status": "completed", "stop_reason": None,
           "budget": dict(BUDGET), "response": "fake",
           "tokens_used": 120, "usage": dict(USAGE_OK),
           "tools_called": ["fs_read"], "conversation_length": 4}
if payload == "partial_incomplete":
    doc["success"] = False
json.dump(doc, sys.stdout)
sys.stdout.write("\n")
sys.exit(int(os.environ.get("FAKE_EXIT", "0")))
"""


@contextmanager
def fake_env(**overrides):
    """Temporarily set FAKE_* environment variables for a child agent."""
    saved = dict(os.environ)
    for key, value in overrides.items():
        if value is None:
            os.environ.pop(key, None)
        else:
            os.environ[key] = value
    try:
        yield
    finally:
        os.environ.clear()
        os.environ.update(saved)


@contextmanager
def changed_dir(path):
    previous = os.getcwd()
    os.chdir(path)
    try:
        yield
    finally:
        os.chdir(previous)


def git(*args, cwd):
    result = subprocess.run(
        ["git"] + list(args), cwd=str(cwd), capture_output=True, text=True,
        check=False,
    )
    if result.returncode != 0:
        raise AssertionError(f"git {args} failed: {result.stderr.strip()}")
    return result.stdout.strip()


class HarnessCase(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    # -- fixtures -----------------------------------------------------
    def make_repo(self):
        repo = self.root / "repo"
        repo.mkdir()
        git("init", cwd=repo)
        git("config", "user.email", "eval@example.com", cwd=repo)
        git("config", "user.name", "eval", cwd=repo)
        (repo / "tracked.txt").write_text("base content\n", encoding="utf-8")
        git("add", "tracked.txt", cwd=repo)
        git("commit", "-m", "base", cwd=repo)
        return repo

    def make_fake(self, name="fake-dgc"):
        fake = self.root / name
        fake.write_text(FAKE_AGENT, encoding="utf-8")
        fake.chmod(fake.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        return fake

    def write_cases(self, cases, name="cases.json"):
        path = self.root / name
        path.write_text(json.dumps(cases), encoding="utf-8")
        return path

    def base_case(self, **overrides):
        case = {
            "case_id": "fake-case",
            "prompt": "Do the fake thing.",
            "expected_skills": [],
            "acceptance": ["fake acceptance holds"],
        }
        case.update(overrides)
        return case

    def write_manifest(self, fake, cases_path, **overrides):
        manifest = {
            "schema_version": 1,
            "base_ref": "HEAD",
            "cases": str(cases_path),
            "trials": 1,
            "model": "exact-model-id",
            "provider": "openai",
            "environment_id": "test-machine-v1",
            "timeout_seconds": 60,
            "termination_grace_seconds": 5,
            "seed": 1,
            "variants": [
                {"name": "baseline", "agent_command": [str(fake)]},
            ],
        }
        manifest.update(overrides)
        path = self.root / "eval-run.json"
        path.write_text(json.dumps(manifest), encoding="utf-8")
        return path

    def load_harness(self, manifest_path):
        manifest = common.load_manifest(manifest_path)
        cases = common.load_cases(manifest["cases_path"])
        return manifest, cases

    def run_trial(self, repo, manifest, case, trial=1, variant="baseline",
                  keep=False, out_name="out"):
        out_root = self.root / out_name
        settings = common.build_settings(manifest, "digest-for-tests")
        variant_by_name = {v["name"]: v for v in manifest["variants"]}
        metas = {
            v["name"]: runner.variant_metadata(v) for v in manifest["variants"]
        }
        base_commit = git("rev-parse", "HEAD", cwd=repo)
        return runner.run_single(
            repo, manifest, variant_by_name, metas, {case["case_id"]: case},
            {"variant": variant, "case_id": case["case_id"], "trial": trial},
            base_commit, settings, out_root, keep,
        ), out_root


class ManifestTests(HarnessCase):
    def test_manifest_rejects_nonfinite_execution_deadlines(self):
        fake = self.make_fake()
        cases = self.write_cases([self.base_case()])
        for field in ("timeout_seconds", "termination_grace_seconds"):
            for value in (float("inf"), float("-inf"), float("nan"), 10**400,
                          0, -1, True, "10", None):
                with self.subTest(field=field, value=value):
                    path = self.write_manifest(fake, cases, **{field: value})
                    with self.assertRaisesRegex(ValueError, field):
                        common.load_manifest(path)

    def test_manifest_preserves_positive_fractional_execution_deadlines(self):
        fake = self.make_fake()
        cases = self.write_cases([self.base_case()])
        path = self.write_manifest(
            fake, cases, timeout_seconds=0.25, termination_grace_seconds=0.5,
        )
        manifest = common.load_manifest(path)
        self.assertEqual(manifest["timeout_seconds"], 0.25)
        self.assertEqual(manifest["termination_grace_seconds"], 0.5)

    def test_overflowing_json_deadlines_fail_before_agent_probe(self):
        from unittest.mock import patch

        fake = self.make_fake()
        cases = self.write_cases([self.base_case()])
        output = self.root / "output"
        for field in ("timeout_seconds", "termination_grace_seconds"):
            with self.subTest(field=field):
                path = self.write_manifest(fake, cases, **{field: 1.0})
                path.write_text(
                    path.read_text(encoding="utf-8").replace(
                        f'"{field}": 1.0', f'"{field}": 1e309',
                    ), encoding="utf-8",
                )
                with patch.object(runner, "variant_metadata") as probe:
                    self.assertEqual(runner.main([
                        "--manifest", str(path), "--output", str(output),
                    ]), 2)
                    probe.assert_not_called()
                self.assertFalse(output.exists())

    def test_manifest_rejects_duplicate_variants(self):
        fake = self.make_fake()
        cases = self.write_cases([self.base_case()])
        with self.assertRaises(ValueError) as ctx:
            self.write_manifest(fake, cases, variants=[
                {"name": "same", "agent_command": [str(fake)]},
                {"name": "same", "agent_command": [str(fake)]},
            ])
            common.load_manifest(self.root / "eval-run.json")
        self.assertIn("duplicate variant", str(ctx.exception))

    def test_manifest_rejects_zero_trials(self):
        fake = self.make_fake()
        cases = self.write_cases([self.base_case()])
        path = self.write_manifest(fake, cases, trials=0)
        with self.assertRaises(ValueError) as ctx:
            common.load_manifest(path)
        self.assertIn("trials", str(ctx.exception))

    def test_manifest_rejects_unsafe_case_id(self):
        path = self.write_cases([self.base_case(case_id="../escape")])
        with self.assertRaises(ValueError) as ctx:
            common.load_cases(path)
        self.assertIn("case_id", str(ctx.exception))

    def test_manifest_rejects_secret_cli_args(self):
        fake = self.make_fake()
        cases = self.write_cases([self.base_case()])
        for secret in ("--api-key", "--api_key=sekret", "--bearer-token",
                       "--token", "--token=sekret"):
            with self.subTest(secret=secret):
                path = self.root / "eval-run.json"
                path.write_text(json.dumps({
                    "schema_version": 1, "base_ref": "HEAD",
                    "cases": str(cases), "trials": 1, "model": "m",
                    "environment_id": "e", "timeout_seconds": 10,
                    "termination_grace_seconds": 5, "seed": 1,
                    "variants": [{"name": "v", "agent_command": [str(fake), secret]}],
                }), encoding="utf-8")
                with self.assertRaises(ValueError) as ctx:
                    common.load_manifest(path)
                self.assertIn("secret", str(ctx.exception))

    def test_manifest_allows_token_prefixed_non_secret_flags(self):
        fake = self.make_fake()
        cases = self.write_cases([self.base_case()])
        path = self.root / "eval-run.json"
        path.write_text(json.dumps({
            "schema_version": 1, "base_ref": "HEAD",
            "cases": str(cases), "trials": 1, "model": "m",
            "environment_id": "e", "timeout_seconds": 10,
            "termination_grace_seconds": 5, "seed": 1,
            "variants": [{"name": "v",
                          "agent_command": [str(fake), "--token-budget", "100"]}],
        }), encoding="utf-8")
        manifest = common.load_manifest(path)
        self.assertEqual(
            manifest["variants"][0]["agent_command"],
            [str(fake), "--token-budget", "100"],
        )

    def test_manifest_rejects_runner_managed_args(self):
        fake = self.make_fake()
        cases = self.write_cases([self.base_case()])
        for extra in (["--model", "other"], ["exec"], ["--json"]):
            with self.subTest(extra=extra):
                path = self.root / "eval-run.json"
                path.write_text(json.dumps({
                    "schema_version": 1, "base_ref": "HEAD",
                    "cases": str(cases), "trials": 1, "model": "m",
                    "environment_id": "e", "timeout_seconds": 10,
                    "termination_grace_seconds": 5, "seed": 1,
                    "variants": [{"name": "v",
                                  "agent_command": [str(fake)] + extra}],
                }), encoding="utf-8")
                with self.assertRaises(ValueError):
                    common.load_manifest(path)

    def test_manifest_paths_resolve_relative_to_manifest(self):
        subdir = self.root / "nested"
        subdir.mkdir()
        (subdir / "cases.json").write_text(
            json.dumps([self.base_case()]), encoding="utf-8")
        (subdir / "eval.toml").write_text("[eval]\n", encoding="utf-8")
        fake = self.make_fake()
        path = subdir / "eval-run.json"
        path.write_text(json.dumps({
            "schema_version": 1, "base_ref": "HEAD", "cases": "cases.json",
            "trials": 1, "model": "m", "environment_id": "e",
            "timeout_seconds": 10, "termination_grace_seconds": 5, "seed": 1,
            "variants": [{"name": "v", "agent_command": [str(fake)],
                          "config": "eval.toml"}],
        }), encoding="utf-8")
        manifest = common.load_manifest(path)
        self.assertEqual(manifest["cases_path"], str(subdir / "cases.json"))
        self.assertEqual(
            manifest["variants"][0]["config"], subdir / "eval.toml")

    def test_version_probe_runs_outside_repository(self):
        snitch = self.root / "snitch-dgc"
        log = self.root / "probe-cwd.log"
        snitch.write_text(
            "#!/usr/bin/env python3\n"
            "import os\n"
            f"open({str(log)!r}, 'a').write(os.getcwd() + chr(10))\n"
            "print('snitch 1.0')\n",
            encoding="utf-8",
        )
        snitch.chmod(snitch.stat().st_mode | stat.S_IXUSR)
        meta = runner.variant_metadata(
            {"agent_command": [str(snitch)], "config": None})
        self.assertEqual(meta["agent_version"], "snitch 1.0")
        cwd = Path(log.read_text(encoding="utf-8").strip()).resolve()
        self.assertNotEqual(cwd, Path.cwd().resolve())
        self.assertNotEqual(cwd, self.root.resolve())

    def test_execution_order_is_seeded_and_interleaved(self):
        manifest = {
            "seed": 7, "trials": 2,
            "variants": [{"name": "baseline"}, {"name": "candidate"}],
        }
        cases = [{"case_id": "a"}, {"case_id": "b"}]
        first = common.execution_matrix(manifest, cases)
        second = common.execution_matrix(manifest, cases)
        self.assertEqual(first, second)
        self.assertEqual(len(first), 2 * 2 * 2)
        # Variants must interleave per (case, trial): the first two entries
        # cover the same case/trial with both variants, never one variant's
        # whole block before the other.
        self.assertEqual(
            {(item["case_id"], item["trial"]) for item in first[:2]},
            {("a", 1)},
        )
        self.assertEqual({item["variant"] for item in first[:2]},
                         {"baseline", "candidate"})


    def test_acceptance_check_schema_rejects_ambiguous_or_unsafe_contracts(self):
        check = {"name": "oracle", "argv": [sys.executable, "-c", "pass"]}
        for checks in [
            [check, check],
            [dict(check, protected_paths=["../outside.py"])],
            [dict(check, protected_paths=["/absolute.py"])],
            [dict(check, protected_paths=None)],
            [dict(check, protected_path=["typo.py"])],
            [dict(check, timeout_seconds=float("inf"))],
            [dict(check, timeout_seconds=10**400)],
        ]:
            with self.subTest(checks=checks), self.assertRaises(ValueError):
                common.load_cases(self.write_cases([self.base_case(post_checks=checks)]))


@unittest.skipIf(NEEDS_GIT, "git is required for worktree tests")
class RunnerTests(HarnessCase):
    @unittest.skipUnless(os.name == "posix", "detached sessions require POSIX")
    def test_detached_output_reader_stops_before_artifacts_are_returned(self):
        import signal
        import threading
        import time
        pidfile = self.root / "detached.pid"
        child_code = (
            "import os,pathlib,time; pathlib.Path(%r).write_text(str(os.getpid())); "
            "time.sleep(0.6); print('late output', flush=True); time.sleep(0.2)"
        ) % str(pidfile)
        code = (
            "import subprocess,sys,time; subprocess.Popen([sys.executable, '-c', %r], "
            "start_new_session=True); time.sleep(0.1)"
        ) % child_code
        stdout, stderr = self.root / "stdout", self.root / "stderr"
        threads_before = set(threading.enumerate())
        try:
            result = runner.stream_child(
                [sys.executable, "-c", code], self.root, dict(os.environ),
                2, 0.05, stdout, stderr,
            )
            self.assertIn("output_capture_incomplete", result[3])
            self.assertFalse(set(threading.enumerate()) - threads_before)
            captured = (stdout.read_bytes(), stderr.read_bytes())
            time.sleep(0.8)
            self.assertEqual((stdout.read_bytes(), stderr.read_bytes()), captured)
        finally:
            if pidfile.exists():
                try:
                    os.killpg(int(pidfile.read_text()), signal.SIGKILL)
                except ProcessLookupError:
                    pass

    def test_stream_output_is_bounded_while_child_runs(self):
        from unittest.mock import patch
        stdout = self.root / "stdout.txt"
        stderr = self.root / "stderr.txt"
        with patch.object(runner, "MAX_STREAM_BYTES", 1024):
            result = runner.stream_child(
                [sys.executable, "-c", "import os; os.write(1, b'x'*4096); os.write(2, b'y'*4096)"],
                self.root, dict(os.environ), 5, 0.1, stdout, stderr,
            )
        self.assertEqual(result[0], 0)
        self.assertTrue(result[4])
        self.assertEqual(stdout.stat().st_size, 1024)
        self.assertEqual(stderr.stat().st_size, 1024)

    @unittest.skipUnless(os.name == "posix", "process groups require POSIX")
    def test_timeout_cleans_descendants_when_leader_exits_on_term(self):
        import signal
        import time
        marker = self.root / "descendant-survived"
        pidfile = self.root / "leader.pid"
        child_code = "import time,pathlib; time.sleep(0.8); pathlib.Path(%r).write_text('alive')" % str(marker)
        code = (
            "import os,pathlib,subprocess,sys,time; "
            "subprocess.Popen([sys.executable, '-c', %r]); "
            "pathlib.Path(%r).write_text(str(os.getpid())); time.sleep(20)"
        ) % (child_code, str(pidfile))
        try:
            result = runner.stream_child(
                [sys.executable, "-c", code], self.root, dict(os.environ),
                0.2, 0.1, self.root / "stdout", self.root / "stderr",
            )
            self.assertTrue(result[1])
            self.assertTrue(pidfile.exists(), "fixture must start before timeout")
            time.sleep(1)
            self.assertFalse(marker.exists(), "descendant survived parent termination")
        finally:
            if pidfile.exists():
                try:
                    os.killpg(int(pidfile.read_text()), signal.SIGKILL)
                except ProcessLookupError:
                    pass

    def test_committed_agent_edit_is_captured_and_read_only_violation(self):
        repo = self.make_repo()
        (repo / ".gitignore").write_text(".doge/\n")
        git("add", ".gitignore", cwd=repo)
        git("commit", "-qm", "ignore runtime state", cwd=repo)
        fake = self.make_fake()
        script = fake.read_text()
        script = script.replace("args = sys.argv[1:]", '''args = sys.argv[1:]
if "exec" in args:
    import pathlib, subprocess
    pathlib.Path("tracked.txt").write_text("committed agent change\\n")
    subprocess.run(["git", "add", "tracked.txt"], check=True)
    subprocess.run(["git", "commit", "-qm", "agent change"], check=True)
''')
        fake.write_text(script)
        manifest, cases = self.load_harness(self.write_manifest(
            fake, self.write_cases([self.base_case(workspace_policy="read_only")]),
        ))
        measurement, out = self.run_trial(repo, manifest, cases[0])
        self.assertIn("workspace_modified_in_read_only_case", measurement["machine_findings"])
        patch = out / "baseline/runs/fake-case/trial-001/agent.patch"
        self.assertIn("+committed agent change", patch.read_text())

    def test_each_trial_starts_from_clean_base(self):
        repo = self.make_repo()
        fake = self.make_fake()
        manifest, cases = self.load_harness(
            self.write_manifest(fake, self.write_cases([self.base_case()])))
        with fake_env(FAKE_CREATE_FILE="marker-from-trial-1"):
            first, _ = self.run_trial(
                repo, manifest, cases[0], trial=1, keep=True, out_name="out1")
        self.assertEqual(first["run_status"], "completed")
        trial1_ws = Path(first["workspace"])
        self.assertTrue((trial1_ws / "marker-from-trial-1").exists())
        with fake_env(FAKE_CREATE_FILE=None):
            second, _ = self.run_trial(
                repo, manifest, cases[0], trial=2, keep=True, out_name="out2")
        self.assertEqual(second["run_status"], "completed")
        trial2_ws = Path(second["workspace"])
        self.assertNotEqual(trial1_ws, trial2_ws)
        self.assertFalse((trial2_ws / "marker-from-trial-1").exists())
        self.assertEqual(
            (trial2_ws / "tracked.txt").read_text(encoding="utf-8"),
            "base content\n",
        )

    def test_agent_invocation_uses_exec_json_without_resume(self):
        repo = self.make_repo()
        fake = self.make_fake()
        argv_log = self.root / "argv.log"
        manifest, cases = self.load_harness(
            self.write_manifest(fake, self.write_cases([self.base_case()])))
        with fake_env(FAKE_ARGV_LOG=str(argv_log)):
            measurement, _ = self.run_trial(repo, manifest, cases[0])
        self.assertEqual(measurement["run_status"], "completed")
        invocations = [
            json.loads(line) for line in argv_log.read_text().splitlines()
        ]
        agent_call = [a for a in invocations if "exec" in a][0]
        self.assertNotIn("--resume", agent_call)
        self.assertFalse(any(a.startswith("--resume") for a in agent_call))
        self.assertEqual(agent_call[-1], "--json")
        exec_index = agent_call.index("exec")
        self.assertIn("--model", agent_call[:exec_index])
        self.assertIn("exact-model-id", agent_call[:exec_index])
        self.assertIn("--provider", agent_call[:exec_index])

    def test_partial_status_supports_legacy_and_corrected_completion_flags(self):
        for success, exit_code in [(True, 0), (False, 2)]:
            with self.subTest(success=success):
                self.assertEqual(common.map_run_status(
                    {"exec_success": success, "exec_status": "partial", "stop_reason": "tool_call_budget"},
                    exit_code, False), ("partial", "tool_call_budget"))

    def test_partial_run_is_preserved(self):
        repo = self.make_repo()
        fake = self.make_fake()
        manifest, cases = self.load_harness(
            self.write_manifest(fake, self.write_cases([self.base_case()])))
        with fake_env(FAKE_PAYLOAD="partial_incomplete", FAKE_EXIT="2"):
            measurement, _ = self.run_trial(repo, manifest, cases[0])
        self.assertEqual(measurement["exit_code"], 2)
        self.assertEqual(measurement["run_status"], "partial")
        self.assertEqual(measurement["stop_reason"], "token_budget")
        # The runner never invents review outcomes, even for partial runs.
        self.assertIsNone(measurement["accepted"])
        self.assertIsNone(measurement["rework_count"])
        self.assertIsNone(measurement["contract_violations"])

    def test_timeout_preserves_trial_and_continues(self):
        repo = self.make_repo()
        fake = self.make_fake()
        manifest, cases = self.load_harness(
            self.write_manifest(
                fake, self.write_cases([self.base_case()]),
                trials=2, timeout_seconds=1, termination_grace_seconds=2,
            ))
        with fake_env(FAKE_PAYLOAD="ok", FAKE_SLEEP_SECONDS="30",
                       FAKE_SESSIONS="0"):
            first, out_root = self.run_trial(
                repo, manifest, cases[0], trial=1, out_name="out")
            second, _ = self.run_trial(
                repo, manifest, cases[0], trial=2, out_name="out")
        for measurement in (first, second):
            self.assertEqual(measurement["run_status"], "timed_out")
            self.assertTrue(measurement["timed_out"])
        run_dir = out_root / "baseline" / "runs" / "fake-case" / "trial-001"
        self.assertTrue((run_dir / "stdout.txt").exists())
        self.assertTrue((run_dir / "stderr.txt").exists())
        self.assertTrue((run_dir / "run.json").exists())
        # Disposable worktrees are removed even after timeouts.
        self.assertEqual(git("worktree", "list", "--porcelain", cwd=repo).count("worktree"), 1)

    def test_sigterm_ignored_agent_is_force_killed_after_grace(self):
        repo = self.make_repo()
        fake = self.make_fake()
        manifest, cases = self.load_harness(
            self.write_manifest(
                fake, self.write_cases([self.base_case()]),
                timeout_seconds=1, termination_grace_seconds=2,
            ))
        with fake_env(FAKE_SLEEP_SECONDS="30", FAKE_IGNORE_TERM="1",
                       FAKE_SESSIONS="0"):
            measurement, _ = self.run_trial(
                repo, manifest, cases[0], out_name="out")
        self.assertEqual(measurement["run_status"], "timed_out")
        # SIGTERM was ignored, so the runner must have waited out the full
        # grace period before force-killing the process group.
        self.assertGreaterEqual(measurement["elapsed_seconds"], 2.5)

    def test_nonzero_exit_is_preserved(self):
        repo = self.make_repo()
        fake = self.make_fake()
        manifest, cases = self.load_harness(
            self.write_manifest(fake, self.write_cases([self.base_case()])))
        with fake_env(FAKE_PAYLOAD="fail", FAKE_EXIT="3"):
            measurement, _ = self.run_trial(repo, manifest, cases[0])
        self.assertEqual(measurement["run_status"], "failed")
        self.assertEqual(measurement["exit_code"], 3)

    def test_malformed_stdout_is_preserved(self):
        repo = self.make_repo()
        fake = self.make_fake()
        manifest, cases = self.load_harness(
            self.write_manifest(fake, self.write_cases([self.base_case()])))
        with fake_env(FAKE_STDOUT="garbage"):
            measurement, _ = self.run_trial(repo, manifest, cases[0])
        self.assertEqual(measurement["run_status"], "harness_error")
        self.assertIsNone(measurement["accepted"])

    def test_empty_stdout_is_preserved(self):
        repo = self.make_repo()
        fake = self.make_fake()
        manifest, cases = self.load_harness(
            self.write_manifest(fake, self.write_cases([self.base_case()])))
        with fake_env(FAKE_STDOUT="empty"):
            measurement, _ = self.run_trial(repo, manifest, cases[0])
        self.assertEqual(measurement["run_status"], "harness_error")
        self.assertIsNone(measurement["accepted"])

    def test_progress_observation_survives_run_artifact(self):
        repo = self.make_repo()
        fake = self.make_fake()
        manifest, cases = self.load_harness(
            self.write_manifest(fake, self.write_cases([self.base_case()])))
        progress = {"read_tool_calls": 4, "successful_read_tool_calls": 4,
                    "search_tool_calls": 1, "repeated_read_ranges": 2,
                    "first_mutation_tool_call": None,
                    "first_verification_tool_call": None,
                    "verification_tool_calls": 0,
                    "successful_verification_tool_calls": 0}
        with fake_env(FAKE_PROGRESS=json.dumps(progress)):
            measurement, out_root = self.run_trial(repo, manifest, cases[0])
        run_dir = out_root / "baseline" / "runs" / "fake-case" / "trial-001"
        saved = json.loads((run_dir / "run.json").read_text())
        self.assertEqual(measurement["progress"], progress)
        self.assertEqual(saved["progress"], progress)
        self.assertEqual(saved["run_status"], "completed")
        self.assertIsNone(saved["accepted"])

    def test_patch_includes_tracked_and_untracked(self):
        repo = self.make_repo()
        fake = self.make_fake()
        manifest, cases = self.load_harness(
            self.write_manifest(fake, self.write_cases([self.base_case()])))
        with fake_env(FAKE_MODIFY_TRACKED="1", FAKE_CREATE_FILE="new-file.txt",
                       FAKE_CREATE_CONTENT="brand new\n"):
            measurement, out_root = self.run_trial(
                repo, manifest, cases[0], out_name="out")
        run_dir = out_root / "baseline" / "runs" / "fake-case" / "trial-001"
        patch = (run_dir / "agent.patch").read_text(encoding="utf-8")
        self.assertIn("tracked.txt", patch)
        self.assertIn("fake agent edit", patch)
        self.assertIn("new-file.txt", patch)
        self.assertIn("brand new", patch)
        self.assertTrue((run_dir / "git-status.txt").read_text().strip())
        self.assertTrue((run_dir / "diff-stat.txt").read_text().strip())

    def test_patch_includes_newline_filenames(self):
        repo = self.make_repo()
        fake = self.make_fake()
        manifest, cases = self.load_harness(
            self.write_manifest(fake, self.write_cases([self.base_case()])))
        odd = "odd\nname.txt"
        with fake_env(FAKE_CREATE_FILE=odd, FAKE_CREATE_CONTENT="odd content\n"):
            measurement, out_root = self.run_trial(
                repo, manifest, cases[0], out_name="out")
        run_dir = out_root / "baseline" / "runs" / "fake-case" / "trial-001"
        patch = (run_dir / "agent.patch").read_text(encoding="utf-8")
        self.assertIn("odd content", patch)

    def test_variant_config_resume_true_rejected_false_allowed(self):
        fake = self.make_fake()
        for content, rejected in (('[exec]\nresume = true\n', True),
                                  ('resume = false\n', False)):
            with self.subTest(content=content):
                config = self.root / "eval.toml"
                config.write_text(content, encoding="utf-8")
                variant = {"name": "baseline",
                           "agent_command": [str(fake)],
                           "config": str(config)}
                if rejected:
                    with self.assertRaises(ValueError):
                        runner.validate_agent_binary(variant)
                else:
                    runner.validate_agent_binary(variant)

    def test_read_only_case_records_finding_without_acceptance(self):
        repo = self.make_repo()
        fake = self.make_fake()
        manifest, cases = self.load_harness(
            self.write_manifest(fake, self.write_cases([
                self.base_case(workspace_policy="read_only")])))
        with fake_env(FAKE_MODIFY_TRACKED="1"):
            measurement, _ = self.run_trial(repo, manifest, cases[0])
        self.assertIn(
            "workspace_modified_in_read_only_case",
            measurement["machine_findings"],
        )
        self.assertIsNone(measurement["accepted"])

    def acceptance_fixture(self):
        repo = self.make_repo()
        (repo / "calc.py").write_text("def total(values): return sum(values) + 1\n")
        (repo / "accept.py").write_text(
            "from calc import total\nassert total([2, 3]) == 5\nassert total([]) == 0\n"
        )
        git("add", "calc.py", "accept.py", cwd=repo)
        git("commit", "-m", "acceptance fixture", cwd=repo)
        check = {"name": "oracle", "argv": [sys.executable, "accept.py"],
                 "protected_paths": ["accept.py"], "timeout_seconds": 10}
        return repo, self.make_fake(), check

    def test_acceptance_weakened_oracle_cannot_pass(self):
        repo, fake, check = self.acceptance_fixture()
        manifest, cases = self.load_harness(self.write_manifest(
            fake, self.write_cases([self.base_case(post_checks=[check])]))
        )
        with fake_env(FAKE_CREATE_FILE="accept.py", FAKE_CREATE_CONTENT="pass\n"):
            measurement, output = self.run_trial(repo, manifest, cases[0])
        self.assertFalse(measurement["required_checks_passed"])
        self.assertEqual(measurement["post_checks"][0]["status"], "invalidated")
        self.assertEqual(measurement["run_status"], "completed")
        self.assertIsNone(measurement["accepted"])
        artifact = output / "baseline/runs/fake-case/trial-001"
        self.assertIn("accept.py", (artifact / "agent.patch").read_text())
        self.assertEqual(json.loads((artifact / "case.json").read_text())["post_checks"][0]["protected_paths"], ["accept.py"])

    def test_acceptance_reference_fix_and_partial_are_independent(self):
        repo, fake, check = self.acceptance_fixture()
        manifest, cases = self.load_harness(self.write_manifest(
            fake, self.write_cases([self.base_case(post_checks=[check])]))
        )
        before, _ = self.run_trial(repo, manifest, cases[0], out_name="before")
        self.assertEqual(before["run_status"], "completed")
        self.assertFalse(before["required_checks_passed"])
        for payload, exit_code, expected in [("ok", "0", "completed"), ("partial_incomplete", "2", "partial")]:
            with fake_env(FAKE_CREATE_FILE="calc.py", FAKE_CREATE_CONTENT="def total(values): return sum(values)\n", FAKE_PAYLOAD=payload, FAKE_EXIT=exit_code):
                after, output = self.run_trial(repo, manifest, cases[0], out_name=expected)
            self.assertTrue(after["required_checks_passed"])
            self.assertEqual(after["run_status"], expected)
            self.assertIsNone(after["accepted"])
            self.assertEqual(after["verification_status"], "passed")
            self.assertEqual(after["post_checks"][0]["status"], "passed")
            artifact = output / "baseline/runs/fake-case/trial-001"
            self.assertIn("calc.py", (artifact / "agent.patch").read_text())
            self.assertEqual(after["case_sha256"], common.sha256_file(artifact / "case.json"))

    def test_acceptance_failure_timeout_and_spawn_error_are_distinct(self):
        repo = self.make_repo()
        fake = self.make_fake()
        checks = [
            {"name": "fail", "argv": [sys.executable, "-c", "raise SystemExit(3)"]},
            {"name": "timeout", "argv": [sys.executable, "-c", "import time; time.sleep(10)"], "timeout_seconds": 0.1},
            {"name": "missing", "argv": [str(self.root / "missing-command")]},
        ]
        manifest, cases = self.load_harness(self.write_manifest(fake, self.write_cases([self.base_case(post_checks=checks)])))
        measurement, _ = self.run_trial(repo, manifest, cases[0])
        results = {r["name"]: r for r in measurement["post_checks"]}
        self.assertEqual(results["fail"]["status"], "failed")
        self.assertEqual(results["fail"]["exit_code"], 3)
        self.assertEqual(results["timeout"]["status"], "timed_out")
        self.assertTrue(results["timeout"]["timed_out"])
        self.assertEqual(results["missing"]["status"], "error")
        self.assertIn("spawn_failed", results["missing"]["reason"])
        self.assertEqual(measurement["run_status"], "completed")
        self.assertFalse(measurement["required_checks_passed"])

    def test_acceptance_not_run_is_recorded_after_agent_spawn_failure(self):
        repo, fake, check = self.acceptance_fixture()
        manifest, cases = self.load_harness(self.write_manifest(fake, self.write_cases([self.base_case(post_checks=[check])])))
        fake.unlink()
        measurement, _ = self.run_trial(repo, manifest, cases[0])
        self.assertEqual(measurement["run_status"], "harness_error")
        self.assertEqual(measurement["verification_status"], "not_run")
        self.assertIsNone(measurement["required_checks_passed"])
        self.assertEqual(len(measurement["post_checks"]), 1)
        self.assertIn("harness_error", measurement["post_checks"][0]["reason"])
        self.assertIsNone(measurement["post_checks"][0]["exit_code"])

    def test_acceptance_checks_still_run_after_agent_timeout(self):
        repo = self.make_repo()
        fake = self.make_fake()
        check = {"name": "oracle", "argv": [sys.executable, "-c", "print('independent check')"]}
        manifest, cases = self.load_harness(self.write_manifest(fake, self.write_cases([self.base_case(post_checks=[check])]), timeout_seconds=0.1, termination_grace_seconds=0.1))
        with fake_env(FAKE_SLEEP_SECONDS="10"):
            measurement, _ = self.run_trial(repo, manifest, cases[0])
        self.assertEqual(measurement["run_status"], "timed_out")
        self.assertEqual(measurement["verification_status"], "passed")
        self.assertIsNone(measurement["accepted"])

    def test_acceptance_missing_baseline_is_not_a_pass(self):
        repo, fake, check = self.acceptance_fixture()
        check["protected_paths"] = ["missing.py"]
        manifest, cases = self.load_harness(self.write_manifest(fake, self.write_cases([self.base_case(post_checks=[check])])))
        measurement, _ = self.run_trial(repo, manifest, cases[0])
        self.assertEqual(measurement["verification_status"], "not_run")
        self.assertIsNone(measurement["required_checks_passed"])
        self.assertEqual(measurement["post_checks"][0]["reason"], "protected_baseline_unavailable")

    def test_acceptance_verifier_mutation_during_check_invalidates_zero_exit(self):
        repo, fake, check = self.acceptance_fixture()
        check["argv"] = [sys.executable, "-c", "from pathlib import Path; Path('accept.py').write_text('pass')"]
        manifest, cases = self.load_harness(self.write_manifest(fake, self.write_cases([self.base_case(post_checks=[check])])))
        measurement, _ = self.run_trial(repo, manifest, cases[0])
        result = measurement["post_checks"][0]
        self.assertEqual(result["exit_code"], 0)
        self.assertEqual(result["status"], "invalidated")
        self.assertIn("during_check", result["reason"])
        self.assertFalse(measurement["required_checks_passed"])

    def test_acceptance_interruption_preserves_completed_and_pending_checks(self):
        from unittest.mock import patch
        repo = self.make_repo()
        fake = self.make_fake()
        checks = [
            {"name": "done", "argv": [sys.executable, "-c", "print('checked')"]},
            {"name": "interrupt", "argv": [sys.executable, "-c", "print('interrupt fixture')"]},
            {"name": "pending", "argv": [sys.executable, "-c", "print('must not run')"]},
        ]
        manifest, cases = self.load_harness(self.write_manifest(fake, self.write_cases([self.base_case(post_checks=checks)])))
        original = runner.stream_child
        def interrupt(argv, *args, **kwargs):
            if argv == checks[1]["argv"]:
                raise KeyboardInterrupt
            return original(argv, *args, **kwargs)
        with patch.object(runner, "stream_child", side_effect=interrupt), self.assertRaises(KeyboardInterrupt):
            self.run_trial(repo, manifest, cases[0])
        artifact = self.root / "out/baseline/runs/fake-case/trial-001"
        saved = json.loads((artifact / "run.json").read_text())
        self.assertEqual(saved["run_status"], "cancelled")
        results = {r["name"]: r for r in saved["post_checks"]}
        self.assertEqual(results["done"]["status"], "passed")
        self.assertEqual(results["interrupt"]["reason"], "check_interrupted")
        self.assertEqual(results["pending"]["reason"], "verification_not_reached")
        self.assertEqual(saved["verification_status"], "not_run")
        self.assertIsNone(saved["required_checks_passed"])
        self.assertIn("checked", (artifact / "checks/done.stdout.txt").read_text())

    def test_acceptance_environment_does_not_inherit_variant_config(self):
        repo = self.make_repo()
        fake = self.make_fake()
        config = self.root / "variant.toml"
        config.write_text("# fixture\n")
        check = {"name": "environment", "argv": [sys.executable, "-c", "import os; assert os.environ.get('DOGE_CODE_CONFIG') == 'parent-fixture'"]}
        manifest, cases = self.load_harness(self.write_manifest(fake, self.write_cases([self.base_case(post_checks=[check])]), variants=[{"name": "baseline", "agent_command": [str(fake)], "config": str(config)}]))
        with fake_env(DOGE_CODE_CONFIG="parent-fixture"):
            measurement, _ = self.run_trial(repo, manifest, cases[0])
        self.assertEqual(measurement["verification_status"], "passed")

    def test_post_checks_record_evidence(self):
        repo = self.make_repo()
        fake = self.make_fake()
        passing = {
            "name": "passing",
            "argv": [sys.executable, "-c", "import sys; sys.exit(0)"],
            "timeout_seconds": 30,
        }
        failing = {
            "name": "failing",
            "argv": [sys.executable, "-c", "import sys; sys.exit(2)"],
            "timeout_seconds": 30,
        }
        manifest, cases = self.load_harness(
            self.write_manifest(fake, self.write_cases([
                self.base_case(post_checks=[passing, failing])])))
        measurement, out_root = self.run_trial(
            repo, manifest, cases[0], out_name="out")
        by_name = {c["name"]: c for c in measurement["post_checks"]}
        self.assertEqual(by_name["passing"]["exit_code"], 0)
        self.assertFalse(by_name["passing"]["timed_out"])
        self.assertEqual(by_name["failing"]["exit_code"], 2)
        self.assertIn("elapsed_seconds", by_name["failing"])
        self.assertFalse(measurement["required_checks_passed"])
        self.assertIsNone(measurement["accepted"])
        checks_dir = out_root / "baseline" / "runs" / "fake-case" / "trial-001" / "checks"
        self.assertTrue((checks_dir / "passing.stdout.txt").exists())

    def test_partial_optional_metrics_survive_runner_artifacts_and_comparison(self):
        repo = self.make_repo()
        fake = self.make_fake()
        manifest, cases = self.load_harness(
            self.write_manifest(fake, self.write_cases([self.base_case()])))
        with fake_env(FAKE_PAYLOAD="partial_optional"):
            measurement, out_root = self.run_trial(repo, manifest, cases[0], out_name="out")
        saved = json.loads((out_root / "baseline" / "runs" / "fake-case" /
                            "trial-001" / "run.json").read_text())
        for record in (measurement, saved):
            for field, count, known, value in (
                ("cached_input_tokens", "cached_usage_records", "known_cached_input_tokens", 80),
                ("reasoning_tokens", "reasoning_usage_records", "known_reasoning_tokens", 30),
                ("cache_write_tokens", "cache_write_usage_records", "known_cache_write_tokens", 10),
            ):
                self.assertIsNone(record[field])
                self.assertEqual(record[count], 1)
                self.assertEqual(record[known], value)
        reviewed = dict(saved, accepted=True, rework_count=0, contract_violations=0)
        path = out_root / "reviewed.jsonl"
        path.write_text(json.dumps(reviewed))
        summary = evals.summarize(evals.load_runs(path))
        self.assertEqual(summary["total_tokens"], 240)
        self.assertIsNone(summary["cached_input_tokens"])
        self.assertEqual(summary["known_cached_input_tokens"], 80)

    def test_runner_leaves_review_null_and_compare_rejects_unreviewed(self):
        repo = self.make_repo()
        fake = self.make_fake()
        manifest, cases = self.load_harness(
            self.write_manifest(fake, self.write_cases([self.base_case()])))
        measurement, out_root = self.run_trial(
            repo, manifest, cases[0], out_name="out")
        self.assertIsNone(measurement["accepted"])
        unreviewed = out_root / "unreviewed.jsonl"
        unreviewed.write_text(json.dumps(measurement) + "\n", encoding="utf-8")
        with self.assertRaises(ValueError) as ctx:
            evals.load_runs(unreviewed)
        self.assertIn("has not been reviewed", str(ctx.exception))

    def test_secret_values_are_never_recorded(self):
        repo = self.make_repo()
        fake = self.make_fake()
        config = self.root / "eval.toml"
        secret = "hunter2-fake-secret-value"
        config.write_text(f'[llm]\npassword = "{secret}"\n', encoding="utf-8")
        manifest, cases = self.load_harness(
            self.write_manifest(
                fake, self.write_cases([self.base_case()]),
                variants=[{"name": "baseline", "agent_command": [str(fake)],
                           "config": str(config)}]))
        with fake_env(OPENAI_API_KEY="sk-fake-secret-api-key"):
            measurement, out_root = self.run_trial(
                repo, manifest, cases[0], out_name="out")
        runner.write_outputs(
            out_root, manifest, ["baseline"], {"baseline": [measurement]},
            {"variants": [{"name": "baseline",
                           "agent_command_fingerprint": "abc"}]},
        )
        haystacks = []
        for path in sorted(out_root.rglob("*")):
            if path.is_file():
                try:
                    haystacks.append(path.read_bytes())
                except OSError:
                    pass
        for blob in haystacks:
            self.assertNotIn(b"sk-fake-secret-api-key", blob)
            self.assertNotIn(secret.encode(), blob)
        self.assertEqual(
            measurement["variant_metadata"].get("config_sha256"),
            common.sha256_file(config),
        )

    def test_evidence_status_when_no_single_session(self):
        repo = self.make_repo()
        fake = self.make_fake()
        manifest, cases = self.load_harness(
            self.write_manifest(fake, self.write_cases([self.base_case()])))
        with fake_env(FAKE_SESSIONS="0"):
            measurement, _ = self.run_trial(repo, manifest, cases[0])
        self.assertEqual(measurement["evidence_status"], "no_sessions")
        self.assertEqual(measurement["run_status"], "completed")
        with fake_env(FAKE_SESSIONS="2"):
            measurement, _ = self.run_trial(repo, manifest, cases[0])
        self.assertTrue(
            measurement["evidence_status"].startswith("ambiguous_sessions"))
        self.assertEqual(measurement["run_status"], "completed")


@unittest.skipIf(NEEDS_GIT, "git is required for worktree tests")
class DryRunTests(HarnessCase):
    def test_dry_run_calls_no_agent(self):
        repo = self.make_repo()
        fake = self.make_fake()
        sentinel = self.root / "agent-was-here"
        cases = self.write_cases([self.base_case()])
        manifest_path = self.write_manifest(
            fake, cases,
            variants=[
                {"name": "baseline", "agent_command": [str(fake)]},
                {"name": "candidate", "agent_command": [str(fake)]},
            ],
            trials=2,
        )
        out_root = self.root / "out"
        with changed_dir(repo), fake_env(FAKE_CREATE_FILE=str(sentinel)):
            code = runner.main([
                "--manifest", str(manifest_path),
                "--output", str(out_root),
                "--dry-run",
            ])
        self.assertEqual(code, 0)
        self.assertFalse(out_root.exists())
        self.assertFalse(sentinel.exists())
        self.assertEqual(
            git("worktree", "list", "--porcelain", cwd=repo).count("worktree"), 1)

    def test_output_guard_refuses_implicit_overwrite(self):
        repo = self.make_repo()
        fake = self.make_fake()
        cases = self.write_cases([self.base_case()])
        manifest_path = self.write_manifest(fake, cases)
        out_root = self.root / "out"
        existing = out_root / "baseline"
        existing.mkdir(parents=True)
        (existing / "measurements.jsonl").write_text("{}\n", encoding="utf-8")
        with changed_dir(repo):
            code = runner.main([
                "--manifest", str(manifest_path),
                "--output", str(out_root),
            ])
        self.assertEqual(code, 2)


class TelemetryTests(unittest.TestCase):
    def payload(self, **overrides):
        usage = {"prompt_tokens": 100, "completion_tokens": 20,
                 "total_tokens": 120, "cached_tokens": 80, "cached_usage_records": 1, "attempts": 1,
                 "usage_records": 1, "unknown_usage_attempts": 0,
                 "all_tracked_attempts_reported": True}
        budget = {"iterations": 2, "tool_calls": 3, "charged_tokens": 120,
                  "elapsed_ms": 1000, "provider_reported_tokens": 120,
                  "estimated_tokens": 0, "request_attempts": 1,
                  "usage_records": 1}
        doc = {"success": True, "status": "completed", "stop_reason": None,
               "budget": budget, "response": "x", "tokens_used": 120,
               "usage": usage, "tools_called": [], "conversation_length": 4}
        doc.update(overrides)
        return doc

    def test_progress_observation_is_preserved_without_acceptance_inference(self):
        doc = self.payload()
        progress = {"read_tool_calls": 4, "successful_read_tool_calls": 3,
                    "search_tool_calls": 2, "repeated_read_ranges": 1,
                    "first_mutation_tool_call": None,
                    "first_verification_tool_call": 7,
                    "verification_tool_calls": 1,
                    "successful_verification_tool_calls": 0}
        doc["budget"]["progress"] = progress
        self.assertEqual(common.extract_exec_telemetry(doc)["progress"], progress)
        for value in (None, {}, {"read_tool_calls": True}, {"read_tool_calls": -1}):
            doc["budget"]["progress"] = value
            self.assertIsNone(common.extract_exec_telemetry(doc)["progress"])
        self.assertIsNone(common.extract_exec_telemetry(self.payload())["progress"])

    def test_complete_usage_maps_to_eval_tokens(self):
        telemetry = common.extract_exec_telemetry(self.payload())
        self.assertEqual(telemetry["input_tokens"], 100)
        self.assertEqual(telemetry["output_tokens"], 20)
        self.assertEqual(telemetry["cached_input_tokens"], 80)
        self.assertEqual(telemetry["tool_calls"], 3)
        self.assertEqual(telemetry["agent_elapsed_seconds"], 1.0)
        self.assertEqual(telemetry["budget_charged_tokens"], 120)

    def test_missing_usage_becomes_null_not_zero(self):
        doc = self.payload()
        del doc["usage"]
        telemetry = common.extract_exec_telemetry(doc)
        self.assertIsNone(telemetry["input_tokens"])
        self.assertIsNone(telemetry["output_tokens"])
        self.assertIsNone(telemetry["cached_input_tokens"])
        # Budget internals stay separate and still parse.
        self.assertEqual(telemetry["budget_charged_tokens"], 120)

    def test_incomplete_usage_becomes_null_not_zero(self):
        doc = self.payload()
        doc["usage"]["unknown_usage_attempts"] = 1
        doc["usage"]["all_tracked_attempts_reported"] = False
        telemetry = common.extract_exec_telemetry(doc)
        self.assertIsNone(telemetry["input_tokens"])
        self.assertIsNone(telemetry["output_tokens"])

    def test_missing_cache_usage_becomes_null(self):
        doc = self.payload()
        del doc["usage"]["cached_tokens"]
        telemetry = common.extract_exec_telemetry(doc)
        self.assertEqual(telemetry["input_tokens"], 100)
        self.assertIsNone(telemetry["cached_input_tokens"])

    def test_reasoning_not_double_counted(self):
        doc = self.payload()
        doc["usage"]["reasoning_tokens"] = 50
        doc["usage"]["reasoning_usage_records"] = 1
        telemetry = common.extract_exec_telemetry(doc)
        # output_tokens is exactly completion_tokens; reasoning is separate.
        self.assertEqual(telemetry["output_tokens"], 20)
        self.assertEqual(telemetry["reasoning_tokens"], 50)

    def test_budget_estimate_kept_separate_from_provider_usage(self):
        doc = self.payload()
        doc["budget"]["estimated_tokens"] = 977
        telemetry = common.extract_exec_telemetry(doc)
        self.assertEqual(telemetry["budget_estimated_tokens"], 977)
        self.assertEqual(telemetry["input_tokens"], 100)
        self.assertEqual(telemetry["output_tokens"], 20)

    def test_partial_optional_usage_keeps_known_subtotals_not_complete_totals(self):
        doc = self.payload()
        doc["usage"].update(attempts=2, usage_records=2, cached_usage_records=1,
            reasoning_usage_records=1, cache_write_usage_records=1,
            cached_tokens=80, reasoning_tokens=30, cache_write_tokens=10)
        doc["budget"].update(request_attempts=2, usage_records=2)
        telemetry = common.extract_exec_telemetry(doc)
        self.assertEqual(telemetry["input_tokens"], 100)
        self.assertEqual(telemetry["output_tokens"], 20)
        for field, count, known, value in (
            ("cached_input_tokens", "cached_usage_records", "known_cached_input_tokens", 80),
            ("reasoning_tokens", "reasoning_usage_records", "known_reasoning_tokens", 30),
            ("cache_write_tokens", "cache_write_usage_records", "known_cache_write_tokens", 10),
        ):
            self.assertIsNone(telemetry[field])
            self.assertEqual(telemetry[count], 1)
            self.assertEqual(telemetry[known], value)

    def test_optional_usage_complete_zero_differs_from_missing_and_partial(self):
        metrics = [("cached_tokens", "cached_input_tokens", "cached_usage_records", "known_cached_input_tokens"),
                   ("reasoning_tokens", "reasoning_tokens", "reasoning_usage_records", "known_reasoning_tokens"),
                   ("cache_write_tokens", "cache_write_tokens", "cache_write_usage_records", "known_cache_write_tokens")]
        for raw, field, counter, known in metrics:
            for count in (None, 0, 1):
                doc = self.payload()
                doc["usage"].update({raw: 0, counter: count})
                with self.subTest(raw=raw, count=count):
                    telemetry = common.extract_exec_telemetry(doc)
                    self.assertEqual(telemetry[known], 0)
                    self.assertEqual(telemetry[field], 0 if count == 1 else None)
            doc = self.payload()
            doc["usage"].pop(raw, None)
            doc["usage"][counter] = 1
            telemetry = common.extract_exec_telemetry(doc)
            self.assertIsNone(telemetry[field])
            self.assertIsNone(telemetry[known])

    def test_optional_known_usage_survives_incomplete_attempt_coverage(self):
        doc = self.payload()
        doc["usage"].update(all_tracked_attempts_reported=False, unknown_usage_attempts=1,
            attempts=2, cached_usage_records=1)
        telemetry = common.extract_exec_telemetry(doc)
        self.assertIsNone(telemetry["cached_input_tokens"])
        self.assertEqual(telemetry["known_cached_input_tokens"], 80)
        self.assertEqual(telemetry["cached_usage_records"], 1)

    def test_optional_counter_invalid_types_never_invent_completeness(self):
        for raw, field, counter, known in common.OPTIONAL_USAGE_METRICS:
            for value in (True, False, -1, 1.5, "1", 2):
                doc = self.payload()
                doc["usage"].update({raw: 7, counter: value})
                with self.subTest(field=field, count=value):
                    telemetry = common.extract_exec_telemetry(doc)
                    self.assertIsNone(telemetry[field])
                    self.assertEqual(telemetry[known], 7)

    def test_unparseable_exec_has_no_telemetry(self):
        telemetry = common.extract_exec_telemetry(None)
        self.assertIsNone(telemetry["input_tokens"])
        self.assertIsNone(telemetry["tool_calls"])


if __name__ == "__main__":
    unittest.main()
