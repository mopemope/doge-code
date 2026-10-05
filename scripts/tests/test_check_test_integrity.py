"""Regression tests for scripts/check-test-integrity.py."""

import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPTS = Path(__file__).resolve().parents[1]


def load_module():
    spec = importlib.util.spec_from_file_location(
        "check_test_integrity", SCRIPTS / "check-test-integrity.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


checker = load_module()


class CheckTestIntegrityTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def write(self, path, content):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content)

    def test_rejects_doge_skip_httptest(self):
        self.write(
            "src/llm/tool_execution/requests.rs",
            'fn test_request() {\n'
            '    if std::env::var("DOGE_SKIP_HTTPTEST").is_ok() {\n'
            "        return;\n"
            "    }\n"
            "}\n",
        )
        errors = checker.check_root(self.root)
        self.assertTrue(any("DOGE_SKIP_HTTPTEST" in error for error in errors))

    def test_rejects_legacy_skip_message(self):
        self.write(
            "src/exec.rs",
            'fn helper() {\n'
            '    eprintln!("Skipping httptest-based test (DOGE_SKIP_HTTPTEST set)");\n'
            "}\n",
        )
        errors = checker.check_skip_message(self.root)
        self.assertTrue(errors)

    def test_rejects_ignored_critical_http_test(self):
        self.write(
            "src/llm/client_core/network.rs",
            "#[tokio::test]\n"
            "#[ignore]\n"
            "async fn chat_once_500_retries_then_succeeds() {}\n",
        )
        self.write(
            "src/llm/tool_execution/requests.rs",
            "#[tokio::test]\n"
            "#[ignore]\n"
            "async fn test_reasoning_usage_recorded_from_response() {}\n",
        )
        self.write(
            "src/llm/client_core.rs",
            "#[tokio::test]\n"
            "#[ignore]\n"
            "pub async fn chat_once_happy_path() {}\n",
        )
        errors = checker.check_ignored_critical_tests(self.root)
        self.assertEqual(len(errors), 3)
        self.assertTrue(
            any("chat_once_500_retries_then_succeeds" in error for error in errors)
        )
        self.assertTrue(
            any("test_reasoning_usage_recorded_from_response" in error for error in errors)
        )

    def test_allows_unrelated_intentional_ignore(self):
        self.write(
            "src/llm/client_core/network.rs",
            "#[test]\n"
            "#[ignore] // slow manual repro, needs hardware token\n"
            "fn slow_manual_token_refresh() {}\n",
        )
        self.write(
            "src/llm/tool_execution/requests.rs",
            "#[test]\n"
            "#[ignore]\n"
            "fn platform_specific_keychain_probe() {}\n",
        )
        self.assertEqual(checker.check_root(self.root), [])

    def test_allows_normal_http_test(self):
        self.write(
            "src/llm/client_core/network.rs",
            "#[tokio::test]\n"
            "async fn chat_once_408_retries_then_succeeds() {\n"
            "    assert_eq!(1, 1);\n"
            "}\n",
        )
        self.write(
            "src/llm/tool_execution/requests.rs",
            "#[tokio::test]\n"
            "async fn test_first_payload_defers_remote_and_builtin_schemas() {}\n",
        )
        self.assertEqual(checker.check_root(self.root), [])


if __name__ == "__main__":
    unittest.main()
