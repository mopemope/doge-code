//! `/test` reports observed test outcomes separately from process completion.
use crate::features::{
    testing::TestResult,
    verification_queue::{self, Outcome},
};

#[derive(Default)]
pub struct TestRunSummary {
    commands: usize,
    unconfirmed: bool,
}
impl TestRunSummary {
    pub fn observe(&mut self, result: &TestResult) -> Outcome {
        let outcome = verification_queue::classify(result);
        self.commands += 1;
        self.unconfirmed |= outcome != Outcome::Passed;
        outcome
    }
    pub fn missing_commands(&mut self) {
        self.unconfirmed = true;
    }
    pub fn completion_message(&self) -> &'static str {
        if self.commands == 0 {
            "No test commands ran; test success was not confirmed."
        } else if self.unconfirmed {
            "Commands completed; test success was not confirmed for all detected languages."
        } else {
            "All reported tests passed for executed commands (reported invocation scope only)."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::structured_test_results::{self as reports, CaptureState};

    fn go(raw: &str, capture: CaptureState, success: bool) -> TestResult {
        let args = vec!["test".into(), "-json".into(), "./...".into()];
        let raw = format!("{{\"Action\":\"start\",\"Package\":\"p\"}}\n{raw}");
        TestResult {
            structured_test_result: reports::observe("go", &args, &raw, capture, success),
            diagnostic_stdout: None,
            execution_observed: true,
            command: "go test -json ./...".into(),
            stdout: raw,
            stderr: String::new(),
            success,
            exit_code: Some(if success { 0 } else { 1 }),
            failed_tests: vec![],
            timed_out: false,
            output_truncated: matches!(capture, CaptureState::Incomplete),
            warnings: vec![],
            cancelled: false,
        }
    }
    #[test]
    fn test_summary_go_reports_distinguish_pass_zero_skip_incomplete_and_failure() {
        for (raw, capture, success, expected) in [
            (
                "{\"Action\":\"run\",\"Package\":\"p\",\"Test\":\"TestA\"}\n{\"Action\":\"pass\",\"Package\":\"p\",\"Test\":\"TestA\"}\n{\"Action\":\"pass\",\"Package\":\"p\"}\n",
                CaptureState::Complete,
                true,
                Outcome::Passed,
            ),
            (
                "{\"Action\":\"pass\",\"Package\":\"p\"}\n",
                CaptureState::Complete,
                true,
                Outcome::ZeroTests,
            ),
            (
                "{\"Action\":\"run\",\"Package\":\"p\",\"Test\":\"TestA\"}\n{\"Action\":\"skip\",\"Package\":\"p\",\"Test\":\"TestA\"}\n{\"Action\":\"pass\",\"Package\":\"p\"}\n",
                CaptureState::Complete,
                true,
                Outcome::Skipped,
            ),
            (
                "{\"Action\":\"run\",\"Package\":\"p\",\"Test\":\"TestA\"}\n",
                CaptureState::Complete,
                true,
                Outcome::Incomplete,
            ),
            (
                "{\"Action\":\"pass\",\"Package\":\"p\"}\n",
                CaptureState::Incomplete,
                true,
                Outcome::Incomplete,
            ),
            (
                "{\"Action\":\"fail\",\"Package\":\"p\"}\n",
                CaptureState::Complete,
                false,
                Outcome::Failed,
            ),
        ] {
            let mut summary = TestRunSummary::default();
            assert_eq!(summary.observe(&go(raw, capture, success)), expected);
            assert_eq!(
                summary
                    .completion_message()
                    .starts_with("All reported tests passed"),
                expected == Outcome::Passed
            );
        }
    }
    #[test]
    fn test_summary_unknown_spoofed_text_and_mixed_commands_never_confirm_success() {
        let mut result = go("", CaptureState::Complete, true);
        result.structured_test_result = None;
        result.command = "cargo test".into();
        result.stdout = "All tests passed!\ntest result: ok. 10 passed; 0 failed\n".into();
        let mut summary = TestRunSummary::default();
        assert_eq!(summary.observe(&result), Outcome::Unknown);
        let pass = go(
            "{\"Action\":\"run\",\"Package\":\"p\",\"Test\":\"TestA\"}\n{\"Action\":\"pass\",\"Package\":\"p\",\"Test\":\"TestA\"}\n{\"Action\":\"pass\",\"Package\":\"p\"}\n",
            CaptureState::Complete,
            true,
        );
        assert_eq!(summary.observe(&pass), Outcome::Passed);
        assert!(summary.completion_message().contains("not confirmed"));
        let mut missing = TestRunSummary::default();
        missing.observe(&pass);
        missing.missing_commands();
        assert!(missing.completion_message().contains("not confirmed"));
        assert!(
            TestRunSummary::default()
                .completion_message()
                .contains("No test commands ran")
        );
    }
    #[test]
    fn test_summary_timeout_cancel_and_spawn_failure_do_not_confirm_success() {
        for expected in [Outcome::TimedOut, Outcome::Cancelled, Outcome::SpawnFailed] {
            let mut result = go("", CaptureState::Complete, false);
            match expected {
                Outcome::TimedOut => result.timed_out = true,
                Outcome::Cancelled => result.cancelled = true,
                Outcome::SpawnFailed => result.execution_observed = false,
                _ => unreachable!(),
            }
            let mut summary = TestRunSummary::default();
            assert_eq!(summary.observe(&result), expected);
            assert!(summary.completion_message().contains("not confirmed"));
        }
    }
}
