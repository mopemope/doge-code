//! Compact test observations for model feedback, distinct from process success.
//! No commands, corrections or completion decisions; unsupported counts stay unknown.
use crate::features::{
    structured_test_results::StructuredTestResult,
    verification_queue::{self, Outcome},
};
use serde_json::{Value, json};

pub(crate) fn observe(program: &str, args: &[String], value: &Value) -> Option<Value> {
    if crate::provenance::classify_verification(program, args)
        != Some(crate::provenance::VerificationKind::Test)
    {
        return None;
    }
    let status = value.get("status")?.as_str()?;
    if !matches!(status, "completed" | "timed_out") {
        return None;
    }
    let success = value
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let incomplete = value
        .get("output_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(true)
        || value
            .get("warnings")
            .and_then(Value::as_array)
            .is_none_or(|v| !v.is_empty());
    let structured: Option<StructuredTestResult> = value
        .get("structured_test_result")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    let mut outcome = if status == "timed_out" {
        Outcome::TimedOut
    } else {
        verification_queue::classify_reported_tests(structured.as_ref(), success, incomplete)
    };
    let plain_zero = outcome == Outcome::Unknown
        && success
        && !incomplete
        && std::path::Path::new(program)
            .file_name()
            .and_then(|s| s.to_str())
            == Some("go")
        && value.get("stderr").and_then(Value::as_str) == Some("")
        && value
            .get("stdout")
            .and_then(Value::as_str)
            .is_some_and(only_no_test_files);
    if plain_zero {
        outcome = Outcome::ZeroTests;
    }
    let count = if plain_zero {
        Some(0)
    } else {
        structured
            .as_ref()
            .and_then(StructuredTestResult::test_count)
    };
    let unit = if plain_zero {
        Some("go_tests_in_reported_packages")
    } else {
        structured
            .as_ref()
            .and_then(StructuredTestResult::test_count_unit)
    };
    let label = match outcome {
        Outcome::Passed => "reported_tests_passed",
        Outcome::ZeroTests => "zero_tests",
        Outcome::Skipped => "all_reported_tests_skipped",
        Outcome::Failed => "failed",
        Outcome::TimedOut => "timed_out",
        Outcome::Incomplete => "incomplete",
        _ => "unknown",
    };
    let note = if outcome == Outcome::Passed {
        "Only reported tests in the selected command's scope passed; this does not establish whole-project correctness."
    } else {
        "Process success does not establish test success. Zero, skipped, incomplete or unknown tests are not passing test evidence. Use a supported structured test invocation to obtain reported counts."
    };
    Some(json!({"outcome":label,"test_count":count,"test_count_unit":unit,"note":note}))
}

/// Recognize only complete Go package lines. Mixed normal results, diagnostics,
/// empty output and truncated capture cannot establish a global zero count.
fn only_no_test_files(stdout: &str) -> bool {
    if stdout.is_empty() || stdout.len() > 64 * 1024 {
        return false;
    }
    let mut lines = stdout.lines().filter(|l| !l.trim().is_empty()).peekable();
    lines.peek().is_some()
        && lines.all(|l| {
            let mut fields = l.split_whitespace();
            fields.next() == Some("?")
                && fields.next().is_some()
                && fields.next() == Some("[no")
                && fields.next() == Some("test")
                && fields.next() == Some("files]")
                && fields.next().is_none()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::structured_test_results::{Counts, ResultFormat, ResultObservation};
    fn result() -> Value {
        json!({"status":"completed","success":true,"exit_code":0,"stdout":"","stderr":"","output_truncated":false,"warnings":[]})
    }
    fn args() -> Vec<String> {
        vec!["test".into(), "pair.go".into()]
    }

    #[test]
    fn process_test_feedback_plain_zero_unknown_and_general() {
        let mut v = result();
        v["stdout"] = json!("? \tfixture\t[no test files]\n? \tother\t[no test files]\n");
        assert_eq!(observe("go", &args(), &v).unwrap()["outcome"], "zero_tests");
        assert!(observe("echo", &args(), &v).is_none());
        v["stdout"] = json!("ok \tfixture\t0.01s\n");
        let o = observe("go", &args(), &v).unwrap();
        assert_eq!(o["outcome"], "unknown");
        assert!(o["test_count"].is_null());
        for text in [
            "",
            " \n",
            "? fixture [no test files]\nok other 0.01s",
            "? fixture [no test files] extra",
            "prefix [no test files]",
        ] {
            v["stdout"] = json!(text);
            assert_eq!(observe("go", &args(), &v).unwrap()["outcome"], "unknown");
        }
    }

    #[test]
    fn process_test_feedback_existing_structured_outcomes() {
        for (counts, success, expected) in [
            (Counts::default(), true, "zero_tests"),
            (
                Counts {
                    passed: 1,
                    ..Default::default()
                },
                true,
                "reported_tests_passed",
            ),
            (
                Counts {
                    failed: 1,
                    ..Default::default()
                },
                false,
                "failed",
            ),
            (
                Counts {
                    skipped: 1,
                    ..Default::default()
                },
                true,
                "all_reported_tests_skipped",
            ),
        ] {
            let mut v = result();
            v["success"] = json!(success);
            v["structured_test_result"] = json!(StructuredTestResult {
                format_version: 1,
                format: ResultFormat::GoTestJson,
                observation: ResultObservation::Complete {
                    test_events: counts,
                    packages: Counts {
                        passed: 1,
                        ..Default::default()
                    }
                }
            });
            let o = observe("go", &args(), &v).unwrap();
            assert_eq!(o["outcome"], expected);
            assert_eq!(o["test_count"], counts.total());
            assert!(serde_json::to_string(&o).unwrap().len() < 1000);
        }
    }

    #[test]
    fn process_test_feedback_incomplete_failure_and_nonexecution() {
        let mut v = result();
        v["stdout"] = json!("? fixture [no test files]\n");
        for status in ["policy_denied", "spawn_failed"] {
            v["status"] = json!(status);
            assert!(observe("go", &args(), &v).is_none());
        }
        v["status"] = json!("timed_out");
        assert_eq!(observe("go", &args(), &v).unwrap()["outcome"], "timed_out");
        v["status"] = json!("completed");
        v["output_truncated"] = json!(true);
        assert_eq!(observe("go", &args(), &v).unwrap()["outcome"], "incomplete");
        v["output_truncated"] = json!(false);
        v["warnings"] = json!(["capture incomplete"]);
        assert_eq!(observe("go", &args(), &v).unwrap()["outcome"], "incomplete");
        v["warnings"] = json!([]);
        v["stderr"] = json!("diagnostic");
        assert_eq!(observe("go", &args(), &v).unwrap()["outcome"], "unknown");
        v["success"] = json!(false);
        v["exit_code"] = json!(1);
        assert_eq!(observe("go", &args(), &v).unwrap()["outcome"], "failed");
    }
}
