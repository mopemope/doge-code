//! Bounded interpretation of reported test events, separate from process outcome.
//! No commands or artifact reads; names/output are never part of the durable shape.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

const MAX_BYTES: usize = 64 * 1024;
const MAX_LINE: usize = 16 * 1024;
const MAX_EVENTS: usize = 4096;
const MAX_PACKAGES: usize = 128;
const MAX_TESTS: usize = 512;
const MAX_KEY: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredTestResult {
    #[serde(deserialize_with = "read_format_version")]
    pub format_version: u32,
    pub format: ResultFormat,
    pub observation: ResultObservation,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultFormat {
    GoTestJson,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResultObservation {
    Complete {
        test_events: Counts,
        packages: Counts,
    },
    Unknown {
        reason: UnknownReason,
    },
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
}
impl Counts {
    pub fn total(self) -> u64 {
        u64::from(self.passed) + u64::from(self.failed) + u64::from(self.skipped)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    UnsupportedInvocation,
    CaptureIncomplete,
    TimedOut,
    LimitExceeded,
    Malformed,
    UnsupportedEvent,
    IncompleteLifecycle,
    InconsistentOutcome,
}
#[derive(Clone, Copy)]
pub enum CaptureState {
    Complete,
    Incomplete,
    TimedOut,
}
fn read_format_version<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    let v = u32::deserialize(d)?;
    if v == 1 {
        Ok(v)
    } else {
        Err(serde::de::Error::custom(
            "unsupported structured test result format",
        ))
    }
}
impl StructuredTestResult {
    pub fn test_count(&self) -> Option<u64> {
        match self.observation {
            ResultObservation::Complete { test_events, .. } => Some(test_events.total()),
            ResultObservation::Unknown { .. } => None,
        }
    }
}

/// Recognize explicit Go JSON requests only. Unknown formats retain absent data.
pub fn observe(
    program: &str,
    args: &[String],
    stdout: &str,
    state: CaptureState,
    success: bool,
) -> Option<Box<StructuredTestResult>> {
    let eligible = invocation(program, args)?;
    let observation = if !eligible {
        Err(UnknownReason::UnsupportedInvocation)
    } else {
        match state {
            CaptureState::Incomplete => Err(UnknownReason::CaptureIncomplete),
            CaptureState::TimedOut => Err(UnknownReason::TimedOut),
            CaptureState::Complete => parse(stdout, success),
        }
    };
    Some(Box::new(StructuredTestResult {
        format_version: 1,
        format: ResultFormat::GoTestJson,
        observation: observation.unwrap_or_else(|reason| ResultObservation::Unknown { reason }),
    }))
}
fn invocation(program: &str, args: &[String]) -> Option<bool> {
    if Path::new(program).file_name()?.to_str()? != "go" || args.first()?.as_str() != "test" {
        return None;
    }
    let mut json = false;
    let mut supported = true;
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        if matches!(a, "-args" | "--") {
            supported = false;
            break;
        }
        let (flag, value) = a.split_once('=').map_or((a, None), |(k, v)| (k, Some(v)));
        match flag {
            "-json" => match value {
                None | Some("true") => json = true,
                Some("false") => json = false,
                _ => {
                    supported = false;
                    json = true;
                }
            },
            "-count" => {
                let count = value.or_else(|| args.get(i + 1).map(String::as_str));
                if count != Some("1") {
                    supported = false;
                }
                if value.is_none() {
                    i += 1;
                }
            }
            "-bench" | "-benchtime" | "-fuzz" | "-fuzztime" | "-fuzzminimizetime" | "-list"
            | "-exec" => {
                supported = false;
                if value.is_none() {
                    i += 1;
                }
            }
            "-run" | "-skip" | "-timeout" | "-parallel" | "-shuffle" | "-cpu" | "-tags"
            | "-ldflags" | "-gcflags" | "-asmflags" | "-coverprofile" | "-coverpkg"
            | "-covermode" | "-outputdir" | "-o" => {
                if value.is_none() {
                    i += 1;
                }
                if flag == "-cpu" {
                    supported = false;
                }
            }
            "-v" | "-race" | "-short" | "-failfast" | "-cover" | "-trimpath" => {}
            _ if a.starts_with('-') => supported = false,
            _ => {}
        }
        i += 1;
    }
    json.then_some(supported)
}
#[derive(Deserialize)]
struct Event<'a> {
    #[serde(rename = "Action", borrow)]
    action: &'a str,
    #[serde(rename = "Package", borrow)]
    package: &'a str,
    #[serde(rename = "Test", default, borrow)]
    test: Option<&'a str>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Running,
    Paused,
    Finished,
}
#[derive(Default)]
struct Package<'a> {
    finished: bool,
    failed_test: bool,
    tests: BTreeMap<&'a str, Phase>,
}
fn parse(stdout: &str, success: bool) -> Result<ResultObservation, UnknownReason> {
    if stdout.len() > MAX_BYTES {
        return Err(UnknownReason::LimitExceeded);
    }
    if stdout.is_empty() || !stdout.ends_with('\n') {
        return Err(UnknownReason::IncompleteLifecycle);
    }
    let mut packages: BTreeMap<&str, Package<'_>> = BTreeMap::new();
    let mut test_events = Counts::default();
    let mut package_counts = Counts::default();
    let mut test_keys = 0;
    for (index, line) in stdout.lines().enumerate() {
        if index >= MAX_EVENTS || line.len() > MAX_LINE {
            return Err(UnknownReason::LimitExceeded);
        }
        let event: Event<'_> = serde_json::from_str(line).map_err(|_| UnknownReason::Malformed)?;
        if event.package.is_empty()
            || event.package.len() > MAX_KEY
            || event
                .test
                .is_some_and(|v| v.is_empty() || v.len() > MAX_KEY)
        {
            return Err(UnknownReason::LimitExceeded);
        }
        if event.action == "start" {
            if event.test.is_some() || packages.contains_key(event.package) {
                return Err(UnknownReason::IncompleteLifecycle);
            }
            if packages.len() >= MAX_PACKAGES {
                return Err(UnknownReason::LimitExceeded);
            }
            packages.insert(event.package, Package::default());
            continue;
        }
        let package = packages
            .get_mut(event.package)
            .ok_or(UnknownReason::IncompleteLifecycle)?;
        if package.finished {
            return Err(UnknownReason::IncompleteLifecycle);
        }
        let terminal = matches!(event.action, "pass" | "fail" | "skip");
        if let Some(test) = event.test {
            if !(test.starts_with("Test") || test.starts_with("Example")) {
                return Err(UnknownReason::UnsupportedEvent);
            }
            match event.action {
                "run" => {
                    if package.tests.contains_key(test) {
                        return Err(UnknownReason::IncompleteLifecycle);
                    }
                    if test_keys >= MAX_TESTS {
                        return Err(UnknownReason::LimitExceeded);
                    }
                    package.tests.insert(test, Phase::Running);
                    test_keys += 1;
                }
                "pause" => {
                    let phase = package
                        .tests
                        .get_mut(test)
                        .ok_or(UnknownReason::IncompleteLifecycle)?;
                    if *phase != Phase::Running {
                        return Err(UnknownReason::IncompleteLifecycle);
                    }
                    *phase = Phase::Paused;
                }
                "cont" => {
                    let phase = package
                        .tests
                        .get_mut(test)
                        .ok_or(UnknownReason::IncompleteLifecycle)?;
                    if *phase != Phase::Paused {
                        return Err(UnknownReason::IncompleteLifecycle);
                    }
                    *phase = Phase::Running;
                }
                "output" => {
                    if package.tests.get(test) != Some(&Phase::Running)
                        && package.tests.get(test) != Some(&Phase::Paused)
                    {
                        return Err(UnknownReason::IncompleteLifecycle);
                    }
                }
                _ if terminal => {
                    let phase = package
                        .tests
                        .get_mut(test)
                        .ok_or(UnknownReason::IncompleteLifecycle)?;
                    if *phase != Phase::Running {
                        return Err(UnknownReason::IncompleteLifecycle);
                    }
                    *phase = Phase::Finished;
                    package.failed_test |= event.action == "fail";
                    count(&mut test_events, event.action);
                }
                _ => return Err(UnknownReason::UnsupportedEvent),
            }
        } else if terminal {
            if (event.action == "pass" && package.failed_test)
                || (event.action == "skip" && !package.tests.is_empty())
            {
                return Err(UnknownReason::InconsistentOutcome);
            }
            if package.tests.values().any(|p| *p != Phase::Finished) {
                return Err(UnknownReason::IncompleteLifecycle);
            }
            package.finished = true;
            count(&mut package_counts, event.action);
        } else if event.action != "output" {
            return Err(UnknownReason::UnsupportedEvent);
        }
    }
    if packages.is_empty() || packages.values().any(|p| !p.finished) {
        return Err(UnknownReason::IncompleteLifecycle);
    }
    if success != (package_counts.failed == 0) {
        return Err(UnknownReason::InconsistentOutcome);
    }
    Ok(ResultObservation::Complete {
        test_events,
        packages: package_counts,
    })
}
fn count(counts: &mut Counts, action: &str) {
    match action {
        "pass" => counts.passed += 1,
        "fail" => counts.failed += 1,
        "skip" => counts.skipped += 1,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(action: &str, package: &str, test: Option<&str>) -> String {
        let mut v = serde_json::json!({"Action":action,"Package":package});
        if let Some(t) = test {
            v["Test"] = t.into();
        }
        format!("{v}\n")
    }
    fn observe_text(text: &str, success: bool) -> StructuredTestResult {
        *observe(
            "go",
            &["test".into(), "-json".into()],
            text,
            CaptureState::Complete,
            success,
        )
        .expect("eligible")
    }
    #[test]
    fn actual_go_output_and_cache_preserve_terminal_counts_without_names() {
        for text in [
            include_str!("fixtures/go-current.jsonl"),
            include_str!("fixtures/go-cached.jsonl"),
        ] {
            let result = observe_text(text, true);
            assert_eq!(result.test_count(), Some(3));
            assert!(matches!(
                result.observation,
                ResultObservation::Complete {
                    test_events: Counts {
                        passed: 2,
                        failed: 0,
                        skipped: 1
                    },
                    packages: Counts {
                        passed: 1,
                        failed: 0,
                        skipped: 0
                    }
                }
            ));
            let json = serde_json::to_string(&result).expect("json");
            assert!(!json.contains("fixture"));
            assert!(!json.contains("TestPass"));
        }
    }
    #[test]
    fn key_and_line_limits_are_enforced() {
        let mut text = String::new();
        for n in 0..=MAX_PACKAGES {
            text += &event("start", &format!("p{n}"), None);
        }
        assert!(matches!(
            observe_text(&text, true).observation,
            ResultObservation::Unknown {
                reason: UnknownReason::LimitExceeded
            }
        ));
        let text =
            event("start", "p", None) + &event("output", "p", Some(&"X".repeat(MAX_KEY + 1)));
        assert!(matches!(
            observe_text(&text, true).observation,
            ResultObservation::Unknown {
                reason: UnknownReason::LimitExceeded
            }
        ));
        let text = event("start", "p", None)
            + &format!(
                "{{\"Action\":\"output\",\"Package\":\"p\",\"Output\":\"{}\"}}\n",
                "X".repeat(MAX_LINE)
            );
        assert!(matches!(
            observe_text(&text, true).observation,
            ResultObservation::Unknown {
                reason: UnknownReason::LimitExceeded
            }
        ));
    }
    #[test]
    fn interleaved_parallel_parent_child_and_failure_are_reported_separately() {
        let text = event("start", "private/pkg", None)
            + &event("start", "other", None)
            + &event("run", "private/pkg", Some("TestPrivate"))
            + &event("pause", "private/pkg", Some("TestPrivate"))
            + &event("skip", "other", None)
            + &event("cont", "private/pkg", Some("TestPrivate"))
            + &event("run", "private/pkg", Some("TestPrivate/sub"))
            + &event("fail", "private/pkg", Some("TestPrivate/sub"))
            + &event("fail", "private/pkg", Some("TestPrivate"))
            + &event("fail", "private/pkg", None);
        let r = observe_text(&text, false);
        assert_eq!(r.test_count(), Some(2));
        assert!(matches!(
            r.observation,
            ResultObservation::Complete {
                test_events: Counts { failed: 2, .. },
                packages: Counts {
                    failed: 1,
                    skipped: 1,
                    ..
                }
            }
        ));
        let json = serde_json::to_string(&r).expect("json");
        assert!(!json.contains("private"));
        assert!(!json.contains("TestPrivate"));
        assert!(matches!(
            observe_text(&text, true).observation,
            ResultObservation::Unknown {
                reason: UnknownReason::InconsistentOutcome
            }
        ));
    }
    #[test]
    fn no_tests_is_known_zero_only_after_complete_package() {
        let text = event("start", "pkg", None) + &event("skip", "pkg", None);
        assert_eq!(observe_text(&text, true).test_count(), Some(0));
        assert_eq!(observe_text("", true).test_count(), None);
        assert_eq!(
            observe_text(&event("start", "pkg", None), true).test_count(),
            None
        );
    }
    #[test]
    fn unsupported_and_effective_flags_never_invent_counts() {
        for argv in [
            vec!["test", "-json=false"],
            vec!["test", "-run", "-json"],
            vec!["test", "-args", "-json"],
            vec!["test", "-json", "-json=false"],
        ] {
            assert!(
                observe(
                    "go",
                    &argv.into_iter().map(String::from).collect::<Vec<_>>(),
                    "",
                    CaptureState::Complete,
                    true
                )
                .is_none()
            );
        }
        for argv in [
            vec!["test", "-json", "-bench=."],
            vec!["test", "-json", "-count=2"],
            vec!["test", "-json", "--", "-json"],
        ] {
            let r = observe(
                "go",
                &argv.into_iter().map(String::from).collect::<Vec<_>>(),
                "",
                CaptureState::Complete,
                true,
            )
            .expect("json");
            assert!(matches!(
                r.observation,
                ResultObservation::Unknown {
                    reason: UnknownReason::UnsupportedInvocation
                }
            ));
        }
    }
    #[test]
    fn malformed_duplicate_unknown_missing_and_truncated_are_unknown() {
        let complete = event("start", "pkg", None)
            + &event("run", "pkg", Some("TestA"))
            + &event("pass", "pkg", Some("TestA"))
            + &event("pass", "pkg", None);
        for text in [
            "TOKEN=private\n".into(),
            complete.trim_end().into(),
            complete.clone() + &event("pass", "pkg", None),
            event("start", "pkg", None) + &event("future", "pkg", None),
            event("start", "pkg", None)
                + &event("run", "pkg", Some("TestA"))
                + &event("pass", "pkg", None),
        ] {
            assert!(observe_text(&text, true).test_count().is_none());
        }
        for state in [CaptureState::Incomplete, CaptureState::TimedOut] {
            assert!(
                observe(
                    "go",
                    &["test".into(), "-json".into()],
                    &complete,
                    state,
                    true
                )
                .expect("eligible")
                .test_count()
                .is_none()
            );
        }
        assert!(matches!(
            observe_text(&"x".repeat(MAX_BYTES + 1), true).observation,
            ResultObservation::Unknown {
                reason: UnknownReason::LimitExceeded
            }
        ));
        let mut json = serde_json::to_value(observe_text(&complete, true)).expect("json");
        json["format_version"] = 2.into();
        assert!(serde_json::from_value::<StructuredTestResult>(json).is_err());
    }
}
