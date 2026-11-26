"""Regression tests for the sensitive-logging source policy checker."""

import importlib.util
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]


def load_checker():
    spec = importlib.util.spec_from_file_location(
        "check_sensitive_logging", SCRIPTS / "check-sensitive-logging.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


CHECKER = load_checker()


def write_scope(root: Path, rel: str, content: str) -> None:
    target = root / rel
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(content)


class SensitiveLoggingCheckerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        # Every expected crate has a source directory unless a test removes one.
        for crate in CHECKER.EXPECTED_CRATES:
            (self.root / "crates" / crate / "src").mkdir(parents=True)

    def test_reject_raw_identifiers(self):
        write_scope(
            self.root,
            "crates/dgc-daemon/src/provider/fixture.rs",
            'fn f() {\n'
            '  debug!(payload = %payload, "request");\n'
            '}\n',
        )
        write_scope(
            self.root,
            "crates/dgc-daemon/src/agent/actor.rs",
            'fn f() {\n'
            '  warn!(response_content = ?content, "bad");\n'
            '}\n',
        )
        write_scope(
            self.root,
            "crates/dgc-cli/src/tui/update.rs",
            'fn f() {\n'
            '  debug!(arguments = %arguments, "tool");\n'
            '}\n',
        )
        errors = CHECKER.check_root(self.root)
        self.assertTrue(any("payload" in e for e in errors), errors)
        self.assertTrue(any("response_content" in e for e in errors), errors)
        self.assertTrue(any("arguments" in e for e in errors), errors)

    def test_accept_safe_metadata(self):
        write_scope(
            self.root,
            "crates/dgc-daemon/src/provider/claude.rs",
            'fn f() {\n'
            '  debug!(\n'
            '    message_count = 4,\n'
            '    prompt_tokens = 100,\n'
            '    content_chars = 42,\n'
            '    "request"\n'
            '  );\n'
            '}\n',
        )
        write_scope(
            self.root,
            "crates/dgc-daemon/src/agent.rs",
            'fn f() {\n'
            '  debug!(tool_result_count = 2, body_bytes = 10, "ok");\n'
            '}\n',
        )
        self.assertEqual(CHECKER.check_root(self.root), [])

    def test_multiline_macro_is_detected(self):
        write_scope(
            self.root,
            "crates/dgc-daemon/src/provider/claude.rs",
            'fn f() {\n'
            '  debug!(\n'
            '    response_chunk = %chunk,\n'
            '    "stream"\n'
            '  );\n'
            '}\n',
        )
        errors = CHECKER.check_root(self.root)
        self.assertTrue(any("response_chunk" in e for e in errors), errors)

    def test_string_literal_does_not_trigger(self):
        write_scope(
            self.root,
            "crates/dgc-daemon/src/provider/claude.rs",
            'fn f() {\n'
            '  debug!(message_count = 1, "payload response_content");\n'
            '}\n',
        )
        self.assertEqual(CHECKER.check_root(self.root), [])

    def test_outside_scope_is_ignored(self):
        for rel in ("crates/dgc-daemon/tests/store.rs", "crates/dgc-daemon/build.rs",
                    "crates/dgc-cli/tests/common/mod.rs", "src/llm/fixture.rs"):
            write_scope(
                self.root,
                rel,
                'fn f() {\n'
                '  debug!(payload = %payload, "request");\n'
                '}\n',
            )
        # One clean in-scope file: an empty scope is itself an error.
        write_scope(self.root, "crates/dgc-daemon/src/agent.rs", "fn f() {}\n")
        self.assertEqual(CHECKER.check_root(self.root), [])

    def test_path_qualified_macro_in_agent_module_is_detected(self):
        write_scope(
            self.root,
            "crates/dgc-daemon/src/agent/actor.rs",
            'fn f() {\n'
            '  tracing::warn!(agent_id = %id, text = %text, "bad");\n'
            '}\n',
        )
        errors = CHECKER.check_root(self.root)
        self.assertTrue(any("text" in e and "agent/actor.rs" in e for e in errors), errors)

    def test_scope_covers_every_crate_source_directory(self):
        rels = ("crates/dgc-daemon/src/provider/codex.rs", "crates/dgc-daemon/src/store.rs",
                "crates/dgc-daemon/src/agent/actor.rs", "crates/dgc-cli/src/tui/render.rs",
                "crates/dgc-client/src/lib.rs", "crates/dgc-protocol/src/lib.rs")
        for rel in rels:
            write_scope(self.root, rel, "fn f() {}\n")
        files = {path.relative_to(self.root).as_posix()
                 for path in CHECKER.collect_scope_files(self.root)}
        self.assertEqual(files, set(rels))

    def test_scope_includes_crates_added_later(self):
        write_scope(self.root, "crates/dgc-newcrate/src/lib.rs",
                    'fn f() {\n  info!(prompt = %prompt, "x");\n}\n')
        errors = CHECKER.check_root(self.root)
        self.assertTrue(any("dgc-newcrate" in e and "prompt" in e for e in errors), errors)

    def test_violation_in_a_non_agent_module_is_detected(self):
        write_scope(self.root, "crates/dgc-client/src/lib.rs",
                    'fn f() {\n  warn!(body = %body, "x");\n}\n')
        errors = CHECKER.check_root(self.root)
        self.assertTrue(any("dgc-client" in e and "body" in e for e in errors), errors)

    def test_missing_expected_crate_source_directory_fails(self):
        write_scope(self.root, "crates/dgc-daemon/src/agent.rs", "fn f() {}\n")
        (self.root / "crates/dgc-client/src").rmdir()
        errors = CHECKER.check_root(self.root)
        self.assertTrue(any("dgc-client" in e and "missing" in e for e in errors), errors)

    def test_repository_scope_is_not_empty(self):
        # A moved module must not silently turn the check into a no-op.
        self.assertTrue(CHECKER.collect_scope_files(SCRIPTS.parent))
        self.assertEqual(CHECKER.check_root(SCRIPTS.parent), [])

    def test_empty_scope_fails_instead_of_passing(self):
        errors = CHECKER.check_root(self.root)
        self.assertTrue(any("no files" in e for e in errors), errors)


if __name__ == "__main__":
    unittest.main()
