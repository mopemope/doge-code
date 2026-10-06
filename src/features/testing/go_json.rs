//! Transient Go diagnostics for the trusted test workflow, never count evidence.
use super::{FailedTest, budget_diagnostic_output};
use serde::Deserialize;
use std::{borrow::Cow, collections::BTreeMap};
const MAX_EVENTS: usize = 4096;
const MAX_INPUT: usize = 128 * 1024;
const MAX_TESTS: usize = 512;
const MAX_LINE: usize = 16 * 1024;
const MAX_KEY: usize = 256;

#[derive(Deserialize)]
struct Event<'a> {
    #[serde(rename = "Action", borrow)]
    action: Cow<'a, str>,
    #[serde(rename = "Package", default, borrow)]
    package: Cow<'a, str>,
    #[serde(rename = "Test", default, borrow)]
    test: Option<Cow<'a, str>>,
    #[serde(rename = "Output", default)]
    output: Option<String>,
}

/// Preserve Output events as human text, including build diagnostics. Any
/// malformed capture is marked incomplete while intact Output events stay visible.
pub(super) fn diagnostics(stdout: &str) -> String {
    let (stdout, limited) = bounded_input(stdout);
    let mut text = String::new();
    if limited {
        text.push_str("[Go diagnostics input incomplete: capture limit]\n");
    }
    for (i, line) in stdout.lines().enumerate() {
        if i >= MAX_EVENTS {
            text.push_str("[Go diagnostics exceeded event limit]\n");
            break;
        }
        if line.len() > MAX_LINE {
            text.push_str("[Go diagnostic event exceeded line limit]\n");

            continue;
        }
        match serde_json::from_str::<Event<'_>>(line) {
            Ok(e) => {
                if let Some(output) = e.output {
                    text.push_str(&output);
                }
            }
            Err(_) => {
                text.push_str("[unparsed Go JSON capture]\n");
            }
        }
    }
    if text.is_empty() && !stdout.is_empty() {
        text.push_str("Go JSON capture contained no diagnostic Output events.\n");
    }
    budget_diagnostic_output(&text)
}

/// Associate diagnostics with exact package/test keys, so parallel output and
/// locations emitted before the terminal failure do not lose their attribution.
pub(super) fn failures(stdout: &str) -> Vec<FailedTest> {
    let (stdout, _) = bounded_input(stdout);
    let mut outputs: BTreeMap<(String, String), String> = BTreeMap::new();
    let mut failed = Vec::new();
    for line in stdout.lines().take(MAX_EVENTS) {
        if line.len() > MAX_LINE {
            continue;
        }
        let Ok(e) = serde_json::from_str::<Event<'_>>(line) else {
            continue;
        };
        let Some(test) = e.test else {
            continue;
        };
        if e.package.is_empty()
            || e.package.len() > MAX_KEY
            || test.is_empty()
            || test.len() > MAX_KEY
        {
            continue;
        }
        let key = (e.package.into_owned(), test.into_owned());
        if !outputs.contains_key(&key) && outputs.len() >= MAX_TESTS {
            continue;
        }
        if e.action == "run" {
            outputs.remove(&key);
        }
        if matches!(e.action.as_ref(), "pass" | "skip") {
            outputs.remove(&key);
            continue;
        }
        let text = outputs.entry(key.clone()).or_default();
        if let Some(output) = e.output {
            text.push_str(&output);
        }
        if e.action == "fail" && failed.len() < MAX_TESTS {
            let mut file = None;
            let mut line = None;
            let mut message = String::new();
            let text = outputs.remove(&key).unwrap_or_default();
            for diagnostic in text.lines() {
                let diagnostic = diagnostic.trim_start();
                if let Some((path, rest)) = diagnostic.split_once(":")
                    && path.ends_with(".go")
                    && let Some((number, detail)) = rest.split_once(":")
                    && let Ok(number) = number.parse::<u32>()
                {
                    file = Some(path.to_string());
                    line = Some(number);
                    message = detail.trim().to_string();
                }
            }
            if message.is_empty() {
                message = budget_diagnostic_output(&text);
            }
            failed.push(FailedTest {
                name: format!("{}::{}", key.0, key.1),
                file_path: file,
                line_number: line,
                message,
                expected: None,
                actual: None,
                stack_trace: None,
                related_files: vec![],
            });
        }
    }
    failed
}

fn bounded_input(stdout: &str) -> (&str, bool) {
    if stdout.len() <= MAX_INPUT {
        return (stdout, false);
    }
    let mut end = MAX_INPUT;
    while !stdout.is_char_boundary(end) {
        end -= 1;
    }
    (&stdout[..end], true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parallel_failure_output_before_terminal_is_package_and_test_scoped() {
        let output = concat!(
            "{\"Action\":\"output\",\"Package\":\"a\",\"Test\":\"TestSame\",\"Output\":\"    a_test.go:12: first detail\\n\"}\n",
            "{\"Action\":\"output\",\"Package\":\"b\",\"Test\":\"TestSame\",\"Output\":\"    b_test.go:22: second detail\\n\"}\n",
            "{\"Action\":\"output\",\"Package\":\"a\",\"Test\":\"TestParent/child\",\"Output\":\"    child_test.go:32: child detail\\n\"}\n",
            "{\"Action\":\"fail\",\"Package\":\"a\",\"Test\":\"TestSame\"}\n",
            "{\"Action\":\"fail\",\"Package\":\"b\",\"Test\":\"TestSame\"}\n",
            "{\"Action\":\"fail\",\"Package\":\"a\",\"Test\":\"TestParent/child\"}\n",
            "{\"Action\":\"fail\",\"Package\":\"a\",\"Test\":\"TestParent\"}\n"
        );
        let failed = failures(output);
        assert_eq!(failed.len(), 4);
        assert_eq!(
            (
                &failed[0].name,
                failed[0].file_path.as_deref(),
                failed[0].line_number,
                failed[0].message.as_str()
            ),
            (
                &"a::TestSame".to_string(),
                Some("a_test.go"),
                Some(12),
                "first detail"
            )
        );
        assert_eq!(failed[1].name, "b::TestSame");
        assert_eq!(failed[1].message, "second detail");
        assert_eq!(failed[2].message, "child detail");
        assert!(failed[3].file_path.is_none());
        assert!(!failed[3].message.contains("child detail"));
    }
    #[test]
    fn go_json_diagnostics_keep_build_output_unknown_actions_and_capture_notices_readable() {
        let input = "{\"Action\":\"build-output\",\"Output\":\"broken.go:3: undefined: missing\\n\"}\n{broken JSON\n{\"Action\":\"output\",\"Output\":\"FAIL example.invalid/fixture\\n\"}\n";
        let display = diagnostics(input);
        assert!(display.contains("broken.go:3: undefined: missing"));
        assert!(display.contains("unparsed Go JSON capture"));
        assert!(display.contains("FAIL example.invalid/fixture"));
        assert!(!display.contains("\"Action\""));
        assert!(failures(input).is_empty());
        let large = format!(
            "{{\"Action\":\"output\",\"Output\":\"{}\"}}\n",
            "x".repeat(MAX_LINE)
        );
        assert!(diagnostics(&large).contains("line limit"));
        assert!(diagnostics(&large).len() < 100);
    }
    #[test]
    fn current_go_fixture_displays_output_without_json_envelope() {
        let capture = include_str!("../structured_test_results/fixtures/go-current.jsonl");
        let display = diagnostics(capture);
        assert!(display.contains("TestPass"));
        assert!(display.contains("PASS"));
        assert!(!display.contains("\"Action\""));
        assert!(failures(capture).is_empty());
    }
    #[test]
    fn repeated_go_json_runs_do_not_reuse_old_failure_details() {
        let events = [
            ("run", None),
            ("output", Some("    same_test.go:1: previous detail\n")),
            ("fail", None),
            ("run", None),
            ("fail", None),
            ("run", None),
            ("output", Some("discarded skipped detail\n")),
            ("skip", None),
            ("run", None),
            ("output", Some("    same_test.go:2: current detail\n")),
            ("fail", None),
        ];
        let raw=events.iter().map(|(action,output)|serde_json::json!({"Action":action,"Package":"fixture","Test":"TestSame","Output":output}).to_string()+"\n").collect::<String>();
        let failures = failures(&raw);
        assert_eq!(failures.len(), 3);
        assert_eq!(failures[0].message, "previous detail");
        assert_eq!(failures[1].message, "");
        assert_eq!(failures[2].message, "current detail");
    }
}
