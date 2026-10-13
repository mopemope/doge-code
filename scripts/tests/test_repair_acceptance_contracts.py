"""Review-policy checks with no trial directories, process or provider access."""
import copy
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from repair_acceptance_contracts import validate_review


class ReviewContractTests(unittest.TestCase):
    def evidence(self, accepted=False, status="blocked"):
        return {"accepted": accepted, "notes": "Independent operator decision.",
                "execution": {"status": status, "elapsed_seconds": None,
                              "review_seconds": None, "rework_seconds": None,
                              "interventions": [],
                              "cost": {"estimated_usd": 0, "confirmed_usd": 0}}}

    def test_unknown_effort_and_negative_decision_preserved_without_mutation(self):
        for status in ("completed", "failed", "timed_out", "blocked", "cancelled"):
            with self.subTest(status=status):
                evidence = self.evidence(status=status)
                original = copy.deepcopy(evidence)
                validate_review(evidence, "offline-replay", False)
                self.assertEqual(evidence, original)

    def test_acceptance_needs_both_completed_execution_and_machine_checks(self):
        validate_review(self.evidence(True, "completed"), "offline-replay", True)
        for passed, status in ((False, "completed"), (True, "cancelled"), (False, "failed")):
            with self.subTest(passed=passed, status=status), self.assertRaisesRegex(ValueError, "acceptance requires"):
                validate_review(self.evidence(True, status), "offline-replay", passed)

    def test_model_identity_and_unknown_cost_remain_distinct_from_offline_zero(self):
        evidence = self.evidence()
        evidence["execution"]["cost"] = {"estimated_usd": None, "confirmed_usd": None}
        with self.assertRaisesRegex(ValueError, "explicit zero"):
            validate_review(evidence, "offline-replay", False)
        evidence["execution"]["identity"] = {key: "recorded" for key in ("model", "provider", "configuration", "permissions", "environment")}
        validate_review(evidence, "model-evaluation", False)
        evidence["execution"]["identity"]["provider"] = " "
        with self.assertRaisesRegex(ValueError, "actual model"):
            validate_review(evidence, "model-evaluation", False)
