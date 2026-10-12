"""Local oracle regressions, separate from model-visible fixture files."""
import importlib.util
import math
from pathlib import Path
import unittest
import subprocess
import sys
import tempfile

spec = importlib.util.spec_from_file_location("timeout_eval_contract", Path(__file__).parents[1] / "timeout_eval_contract.py")
contract = importlib.util.module_from_spec(spec)
spec.loader.exec_module(contract)


def reference(value):
    if isinstance(value, bool) or not isinstance(value, (str, int, float)):
        raise ValueError("invalid timeout")
    try:
        seconds = float(value)
    except (TypeError, ValueError, OverflowError) as error:
        raise ValueError("invalid timeout") from error
    if not math.isfinite(seconds) or seconds <= 0:
        raise ValueError("invalid timeout")
    return seconds


class TimeoutContractTests(unittest.TestCase):
    def test_cli_does_not_create_bytecode_in_evaluated_workspace(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "settings.py").write_text("def parse_timeout(value):\n    return float(value)\n")
            result = subprocess.run([sys.executable, str(Path(__file__).parents[1] / "timeout_eval_contract.py"), directory], capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)
            self.assertIn("33 cases", result.stdout)
            self.assertEqual(sorted(p.name for p in root.iterdir()), ["settings.py"])

    def test_reference_satisfies_normal_boundary_invalid_cases(self):
        count, failures = contract.evaluate(reference)
        self.assertEqual(count, 33)
        self.assertEqual(failures, [])

    def test_incomplete_fix_matching_observed_failure_is_rejected(self):
        def incomplete(value):
            if isinstance(value, bool) or not isinstance(value, (str, int, float)):
                raise ValueError("invalid timeout")
            try:
                seconds = float(value)
            except (TypeError, ValueError) as error:
                raise ValueError("invalid timeout") from error
            if not math.isfinite(seconds) or seconds <= 0:
                raise ValueError("invalid timeout")
            return seconds
        _, failures = contract.evaluate(incomplete)
        self.assertEqual(len(failures), 2)
        self.assertTrue(all("integer_overflow" in f and "OverflowError" in f for f in failures))
        for value in (10**1000, -(10**1000)):
            with self.assertRaises(OverflowError):
                incomplete(value)

    def test_bool_and_nonfinite_mutants_are_detected(self):
        for broken in (lambda value: float(value), lambda value: 1.0):
            _, failures = contract.evaluate(broken)
            self.assertTrue(failures)
            self.assertTrue(any("true" in f for f in failures))
            self.assertTrue(any("nan" in f for f in failures))

    def test_fixture_has_no_reference_fix(self):
        root = Path(__file__).parents[2] / "docs/ai/fixtures/timeout-validation-v2"
        spec = importlib.util.spec_from_file_location("fixture_settings", root / "settings.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        _, failures = contract.evaluate(module.parse_timeout)
        self.assertTrue(any("integer_overflow" in f for f in failures))
        self.assertFalse((root / "reference.py").exists())


if __name__ == "__main__":
    unittest.main()
