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

    def test_reject_raw_identifiers(self):
        write_scope(
            self.root,
            "src/llm/fixture.rs",
            'fn f() {\n'
            '  debug!(payload = %payload, "request");\n'
            '}\n',
        )
        write_scope(
            self.root,
            "src/llm/other.rs",
            'fn f() {\n'
            '  warn!(response_content = ?content, "bad");\n'
            '}\n',
        )
        write_scope(
            self.root,
            "src/tui/llm_response_handler.rs",
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
            "src/llm/fixture.rs",
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
            "src/features/openai_subscription/fixture.rs",
            'fn f() {\n'
            '  debug!(tool_result_count = 2, body_bytes = 10, "ok");\n'
            '}\n',
        )
        self.assertEqual(CHECKER.check_root(self.root), [])

    def test_multiline_macro_is_detected(self):
        write_scope(
            self.root,
            "src/llm/fixture.rs",
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
            "src/llm/fixture.rs",
            'fn f() {\n'
            '  debug!(message_count = 1, "payload response_content");\n'
            '}\n',
        )
        self.assertEqual(CHECKER.check_root(self.root), [])

    def test_outside_scope_is_ignored(self):
        write_scope(
            self.root,
            "src/tools/fixture.rs",
            'fn f() {\n'
            '  debug!(payload = %payload, "request");\n'
            '}\n',
        )
        self.assertEqual(CHECKER.check_root(self.root), [])


if __name__ == "__main__":
    unittest.main()
