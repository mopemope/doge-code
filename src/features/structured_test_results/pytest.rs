//! Observation of pytest's built-in JUnit report for the trusted default runner.
//! Reports are untrusted input; only bounded counts/status are persisted.
use super::{CaptureState, Counts, ResultFormat, ResultObservation, StructuredTestResult};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, io::Read, path::Path};

const MAX_REPORT: u64 = 64 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Passed,
    Failed,
    ZeroTests,
    AllSkipped,
    CollectionError,
    Interrupted,
    RunnerError,
    Incomplete,
    Unknown,
}
impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::Passed => "reported Python tests passed",
            Self::Failed => "reported Python tests failed",
            Self::ZeroTests => "zero Python tests",
            Self::AllSkipped => "all reported Python tests skipped",
            Self::CollectionError => "Python collection error",
            Self::Interrupted => "Python test execution interrupted",
            Self::RunnerError => "Python test runner error",
            Self::Incomplete => "Python report incomplete",
            Self::Unknown => "Python test outcome unknown",
        }
    }
}
pub fn default_invocation(program: &str, args: &[String]) -> bool {
    matches!(
        Path::new(program).file_name().and_then(|s| s.to_str()),
        Some("python" | "python3" | "python.exe")
    ) && args == ["-m", "pytest", "-v"]
}
pub fn observer_eligible(
    program: &str,
    args: &[String],
    addopts: Option<&std::ffi::OsStr>,
) -> bool {
    default_invocation(program, args) && addopts.is_none_or(|s| s.is_empty())
}
pub fn outcome(result: &StructuredTestResult) -> Option<Outcome> {
    match result.observation {
        ResultObservation::Pytest { outcome, .. } => Some(outcome),
        _ => None,
    }
}
pub struct Report {
    directory: tempfile::TempDir,
}
impl Report {
    pub fn prepare() -> std::io::Result<Self> {
        let mut builder = tempfile::Builder::new();
        builder.prefix("dgc-pytest-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        Ok(Self {
            directory: builder.tempdir()?,
        })
    }
    pub fn addopts(&self) -> String {
        // pytest uses shlex for ADDOPTS. The shell is never invoked.
        let path = self
            .directory
            .path()
            .join("report.xml")
            .to_string_lossy()
            .into_owned();
        format!("--junitxml='{}'", path.replace('\'', "'\"'\"'"))
    }
    pub fn observe(&self, exit: Option<i32>, state: CaptureState) -> Box<StructuredTestResult> {
        let path = self.directory.path().join("report.xml");
        let report = read_report(&path);
        observe(report.as_deref().ok(), exit, state)
    }
}
fn read_report(path: &Path) -> std::io::Result<String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    // Refuse directories, symlinks and special files; check the opened descriptor too.
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(std::io::Error::other("not a regular report"));
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_REPORT {
        return Err(std::io::Error::other("report limit/type"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_REPORT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_REPORT {
        return Err(std::io::Error::other("report limit"));
    }
    String::from_utf8(bytes).map_err(std::io::Error::other)
}
pub fn observe(
    xml: Option<&str>,
    exit: Option<i32>,
    state: CaptureState,
) -> Box<StructuredTestResult> {
    let mut counts = None;
    let result = match state {
        CaptureState::Incomplete | CaptureState::TimedOut => Outcome::Incomplete,
        CaptureState::Complete => match exit {
            Some(2) => match xml.and_then(|text| parse(text).ok()) {
                Some((_, true, _)) => Outcome::CollectionError,
                Some(_) => Outcome::Interrupted,
                None => Outcome::Incomplete,
            },
            Some(3 | 4 | 6) => Outcome::RunnerError,
            Some(0 | 1 | 5) => match xml {
                None | Some("") => Outcome::Incomplete,
                Some(text) => match parse(text) {
                    Ok((events, false, false)) => {
                        let status = match exit {
                            Some(0) if events.failed == 0 && events.total() > 0 => {
                                if events.passed > 0 {
                                    Outcome::Passed
                                } else {
                                    Outcome::AllSkipped
                                }
                            }
                            Some(1) if events.failed > 0 => Outcome::Failed,
                            Some(5) if events.total() == 0 => Outcome::ZeroTests,
                            _ => Outcome::Unknown,
                        };
                        if status != Outcome::Unknown {
                            counts = Some(events);
                        }
                        status
                    }
                    Ok(_) => Outcome::Unknown,
                    Err(_) => Outcome::Incomplete,
                },
            },
            _ => Outcome::Unknown,
        },
    };
    Box::new(StructuredTestResult {
        format_version: 1,
        format: ResultFormat::PytestJunit,
        observation: ResultObservation::Pytest {
            outcome: result,
            test_events: counts,
        },
    })
}
fn parse(text: &str) -> Result<(Counts, bool, bool), ()> {
    if text.is_empty() || text.len() as u64 > MAX_REPORT {
        return Err(());
    }
    let doc = roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 4096,
            ..Default::default()
        },
    )
    .map_err(|_| ())?;
    let root = doc.root_element();
    if !root.has_tag_name("testsuites") {
        return Err(());
    }
    let suites: Vec<_> = root.children().filter(|n| n.is_element()).collect();
    // Initial support is the single-suite report emitted by builtin pytest.
    if suites.len() != 1 || !suites[0].has_tag_name("testsuite") {
        return Err(());
    }
    let suite = suites[0];
    let number = |name| {
        suite
            .attribute(name)
            .ok_or(())?
            .parse::<u32>()
            .map_err(|_| ())
    };
    let (tests, failures, skipped, errors) = (
        number("tests")?,
        number("failures")?,
        number("skipped")?,
        number("errors")?,
    );
    if tests > 512 {
        return Err(());
    }
    let mut counts = Counts::default();
    let mut observed_errors = 0;
    let mut collection = false;
    let mut unfinished = false;
    let mut names = BTreeSet::new();
    for case in suite.children().filter(|n| n.is_element()) {
        if case.has_tag_name("properties") {
            continue;
        }
        if !case.has_tag_name("testcase") {
            return Err(());
        }
        // pytest writes an unfinished, nameless testcase on KeyboardInterrupt.
        // It is not a counted terminal event and can never authorize success.
        if case.attribute("name").is_none()
            && case.attribute("classname").is_none()
            && case.attribute("time").is_some()
            && case.attributes().all(|a| a.name() == "time")
            && !case.children().any(|n| n.is_element())
        {
            if unfinished {
                return Err(());
            }
            unfinished = true;
            continue;
        }
        let name = case.attribute("name").ok_or(())?;
        let class = case.attribute("classname").ok_or(())?;
        if !names.insert((class, name)) {
            return Err(());
        }
        let mut terminal = None;
        for child in case.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "failure" | "error" | "skipped" => {
                    if terminal.replace(child.tag_name().name()).is_some() {
                        return Err(());
                    }
                    if child.has_tag_name("error")
                        && child.attribute("message") == Some("collection failure")
                    {
                        collection = true;
                    }
                }
                "properties" | "system-out" | "system-err" => {}
                _ => return Err(()),
            }
        }
        match terminal {
            Some("skipped") => counts.skipped += 1,
            Some("error") => {
                observed_errors += 1;
                counts.failed += 1;
            }
            Some("failure") => counts.failed += 1,
            None => counts.passed += 1,
            _ => return Err(()),
        }
    }
    if counts.total() != u64::from(tests)
        || counts.skipped != skipped
        || observed_errors != errors
        || counts.failed != failures.checked_add(errors).ok_or(())?
    {
        return Err(());
    }
    Ok((counts, collection, unfinished))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn xml(cases: &str, tests: u32, failures: u32, errors: u32, skipped: u32) -> String {
        format!(
            "<testsuites><testsuite tests='{tests}' failures='{failures}' errors='{errors}' skipped='{skipped}'>{cases}</testsuite></testsuites>"
        )
    }
    fn status(text: Option<&str>, exit: i32) -> (Outcome, Option<u64>) {
        let result = observe(text, Some(exit), CaptureState::Complete);
        (outcome(&result).expect("python"), result.test_count())
    }
    #[test]
    fn pytest_report_replay_fixtures() {
        for (text, exit, expected) in [
            (
                include_str!("fixtures/pytest/passed.xml"),
                0,
                Outcome::Passed,
            ),
            (
                include_str!("fixtures/pytest/failed.xml"),
                1,
                Outcome::Failed,
            ),
            (
                include_str!("fixtures/pytest/skipped.xml"),
                0,
                Outcome::AllSkipped,
            ),
            (
                include_str!("fixtures/pytest/zero.xml"),
                5,
                Outcome::ZeroTests,
            ),
            (
                include_str!("fixtures/pytest/interrupted.xml"),
                2,
                Outcome::Interrupted,
            ),
            (
                include_str!("fixtures/pytest/collection-error.xml"),
                2,
                Outcome::CollectionError,
            ),
        ] {
            assert_eq!(status(Some(text), exit).0, expected);
        }
    }
    #[test]
    fn pytest_report_distinguishes_terminal_outcomes_and_exit_contradictions() {
        let pass = xml("<testcase classname='p' name='test_a'/>", 1, 0, 0, 0);
        let fail = xml(
            "<testcase classname='p' name='test_a'><failure message='failed'/></testcase>",
            1,
            1,
            0,
            0,
        );
        let skip = xml(
            "<testcase classname='p' name='test_a'><skipped message='skip'/></testcase>",
            1,
            0,
            0,
            1,
        );
        let collect = xml(
            "<testcase classname='' name='test_a.py'><error message='collection failure'/></testcase>",
            1,
            0,
            1,
            0,
        );
        let zero = xml("", 0, 0, 0, 0);
        for (report, exit, expected, count) in [
            (&pass, 0, Outcome::Passed, Some(1)),
            (&fail, 1, Outcome::Failed, Some(1)),
            (&skip, 0, Outcome::AllSkipped, Some(1)),
            (&zero, 5, Outcome::ZeroTests, Some(0)),
            (&collect, 2, Outcome::CollectionError, None),
            (&pass, 2, Outcome::Interrupted, None),
            (&pass, 3, Outcome::RunnerError, None),
            (&pass, 4, Outcome::RunnerError, None),
            (&pass, 1, Outcome::Unknown, None),
            (&fail, 0, Outcome::Unknown, None),
            (&zero, 0, Outcome::Unknown, None),
            (&pass, 5, Outcome::Unknown, None),
        ] {
            assert_eq!(status(Some(report), exit), (expected, count));
        }
        let interrupted = include_str!("fixtures/pytest/interrupted.xml");
        assert_eq!(status(Some(interrupted), 0), (Outcome::Unknown, None));
        assert_eq!(status(Some(interrupted), 5), (Outcome::Unknown, None));
        assert_eq!(status(None, 0), (Outcome::Incomplete, None));
        assert_eq!(status(Some(""), 0), (Outcome::Incomplete, None));
        assert_eq!(status(None, 2), (Outcome::Incomplete, None));
        assert_eq!(status(None, 99), (Outcome::Unknown, None));
        for state in [CaptureState::Incomplete, CaptureState::TimedOut] {
            let observed = observe(Some(&pass), Some(0), state);
            assert_eq!(outcome(&observed), Some(Outcome::Incomplete));
            assert_eq!(observed.test_count(), None);
        }
        let roundtrip: StructuredTestResult = serde_json::from_str(
            &serde_json::to_string(&observe(Some(&skip), Some(0), CaptureState::Complete))
                .expect("encode"),
        )
        .expect("decode");
        assert_eq!(outcome(&roundtrip), Some(Outcome::AllSkipped));
    }
    #[test]
    fn pytest_report_rejects_partial_xml_unsafe_entities_ambiguous_and_mismatched_counts() {
        for text in [
            "<testsuites>",
            "<!DOCTYPE x [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><testsuites>&x;</testsuites>",
            "<testsuites><testsuite tests='0' failures='0' errors='0' skipped='0'/><testsuite tests='0' failures='0' errors='0' skipped='0'/></testsuites>",
            &xml("<testcase classname='p' name='a'/>", 2, 0, 0, 0),
            &xml(
                "<testcase classname='p' name='a'><failure/><skipped/></testcase>",
                1,
                1,
                0,
                0,
            ),
            &xml(
                "<testcase classname='p' name='a'/><testcase classname='p' name='a'/>",
                2,
                0,
                0,
                0,
            ),
            &xml(
                "<testcase classname='p' name='a'><failure/></testcase>",
                1,
                0,
                0,
                0,
            ),
            &" ".repeat(MAX_REPORT as usize + 1),
        ] {
            assert_eq!(status(Some(text), 0), (Outcome::Incomplete, None));
        }
    }
    #[test]
    fn pytest_report_only_recognizes_default_python_module_runner() {
        let args = ["-m".into(), "pytest".into(), "-v".into()];
        assert!(default_invocation("/project/.venv/bin/python", &args));
        assert!(default_invocation("python3", &args));
        assert!(observer_eligible("python3", &args, None));
        assert!(observer_eligible(
            "python3",
            &args,
            Some(std::ffi::OsStr::new(""))
        ));
        for addopts in ["-q", "--junitxml=old.xml", " "] {
            assert!(!observer_eligible(
                "python3",
                &args,
                Some(std::ffi::OsStr::new(addopts))
            ));
        }
        for program in ["pytest", "poetry", "uv", "bash", "python-wrapper"] {
            assert!(!default_invocation(program, &args));
        }
        assert!(!default_invocation(
            "python3",
            &["-m".into(), "pytest".into(), "--junitxml=old.xml".into()]
        ));
    }
    #[test]
    fn pytest_report_generation_is_private_fresh_and_does_not_reuse_old_reports() {
        let first = Report::prepare().expect("first");
        let second = Report::prepare().expect("second");
        assert_ne!(first.directory.path(), second.directory.path());
        std::fs::write(
            first.directory.path().join("report.xml"),
            xml("<testcase classname='p' name='a'/>", 1, 0, 0, 0),
        )
        .expect("first report");
        assert_eq!(
            outcome(&first.observe(Some(0), CaptureState::Complete)),
            Some(Outcome::Passed)
        );
        assert_eq!(
            outcome(&second.observe(Some(0), CaptureState::Complete)),
            Some(Outcome::Incomplete)
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            assert_eq!(
                first
                    .directory
                    .path()
                    .metadata()
                    .expect("mode")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            symlink(
                first.directory.path().join("report.xml"),
                second.directory.path().join("report.xml"),
            )
            .expect("symlink");
            assert_eq!(
                outcome(&second.observe(Some(0), CaptureState::Complete)),
                Some(Outcome::Incomplete)
            );
        }
        let path = first.directory.path().to_path_buf();
        drop(first);
        assert!(!path.exists());
    }
}
