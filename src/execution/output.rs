//! Bounded stdout/stderr capture for child processes.
//!
//! A runaway `cargo test` can emit megabytes of output. Buffering all of it
//! would let RAM grow with output size, so capture is bounded: only the head
//! and the tail of each stream are retained. The final LLM payload is further
//! budgeted to ~6,000 chars via the shared tool-budget helpers.

/// Bytes retained from the start of a stream.
pub const CAPTURE_HEAD_BYTES: usize = 32 * 1024;
/// Bytes retained from the end of a stream.
pub const CAPTURE_TAIL_BYTES: usize = 32 * 1024;

/// Bounded byte capture with head + tail retention.
#[derive(Debug, Default)]
pub struct BoundedCapture {
    head: Vec<u8>,
    tail: Vec<u8>,
    total_bytes: u64,
    truncated: bool,
}

impl BoundedCapture {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk. Memory stays bounded: at most
    /// `CAPTURE_HEAD_BYTES + CAPTURE_TAIL_BYTES` are retained.
    pub fn push(&mut self, chunk: &[u8]) {
        self.push_with_limits(chunk, CAPTURE_HEAD_BYTES, CAPTURE_TAIL_BYTES);
    }

    /// The runner uses a fixed smaller budget for the entire probe stream.
    pub(crate) fn push_with_limits(&mut self, chunk: &[u8], head_limit: usize, tail_limit: usize) {
        if chunk.is_empty() {
            return;
        }
        self.total_bytes += chunk.len() as u64;
        if self.head.len() < head_limit {
            let take = (head_limit - self.head.len()).min(chunk.len());
            self.head.extend_from_slice(&chunk[..take]);
            if take == chunk.len() {
                return;
            }
            self.push_tail(&chunk[take..], tail_limit);
        } else {
            self.push_tail(chunk, tail_limit);
        }
        if self.total_bytes > (head_limit + tail_limit) as u64 {
            self.truncated = true;
        }
    }

    /// Append to the tail window, evicting the oldest bytes past the limit.
    /// One `drain` per call, proportional to the chunk (read in 8KB units).
    fn push_tail(&mut self, chunk: &[u8], tail_limit: usize) {
        self.tail.extend_from_slice(chunk);
        if self.tail.len() > tail_limit {
            let excess = self.tail.len() - tail_limit;
            self.tail.drain(..excess);
            self.truncated = true;
        }
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub fn was_truncated(&self) -> bool {
        self.truncated
    }

    /// Finish capture, returning lossy text with an omission marker when the
    /// middle was dropped.
    pub fn finish(&self) -> (String, bool) {
        if !self.truncated {
            let mut all = Vec::with_capacity(self.head.len() + self.tail.len());
            all.extend_from_slice(&self.head);
            all.extend_from_slice(&self.tail);
            return (String::from_utf8_lossy(&all).into_owned(), false);
        }
        let head = String::from_utf8_lossy(&self.head).into_owned();
        let tail = String::from_utf8_lossy(&self.tail).into_owned();
        let omitted = self
            .total_bytes
            .saturating_sub((self.head.len() + self.tail.len()) as u64);
        let text = format!("{head}\n...[output truncated: {omitted} bytes omitted]...\n{tail}");
        (text, true)
    }
}

/// Apply the combined tool output budget (stdout + stderr ~6,000 chars,
/// head/tail preserved). Thin wrapper over the shared budget helpers so the
/// execution core and the legacy `execute_bash` path share one implementation.
pub fn budget_command_output(stdout: &str, stderr: &str) -> (String, String, bool, Vec<String>) {
    use crate::tools::budget::{DEFAULT_TOOL_BUDGET_CHARS, head_tail_truncate};

    // Spend the budget on signal: passing-test and build-progress lines carry
    // no information the model can act on, yet they fill head-and-tail windows.
    let stdout = &strip_progress_noise(stdout);
    let stderr = &strip_progress_noise(stderr);
    let budget = DEFAULT_TOOL_BUDGET_CHARS;
    let total = stdout.chars().count() + stderr.chars().count();
    if total <= budget {
        return (stdout.to_string(), stderr.to_string(), false, Vec::new());
    }

    let (stdout_budget, stderr_budget) = if stderr.is_empty() {
        (budget, 0)
    } else {
        let out = budget * 7 / 10;
        (out, budget - out)
    };

    let out = head_tail_truncate(stdout, stdout_budget.max(200));
    let err = head_tail_truncate(stderr, stderr_budget.max(200));
    let mut warnings = vec![format!(
        "command output trimmed to ~{} chars (head and tail preserved); refine the command to see more",
        budget
    )];
    if out.truncated {
        warnings.push("stdout was truncated".to_string());
    }
    if err.truncated {
        warnings.push("stderr was truncated".to_string());
    }
    (out.text, err.text, true, warnings)
}

/// A stream must contain at least this many noise lines before any are
/// removed, so short outputs stay byte-for-byte intact.
const MIN_NOISE_LINES: usize = 8;

/// Streams shorter than this are never filtered: removing a few lines saves
/// little, and a short log may be exactly what the caller asked to see.
const MIN_FILTER_CHARS: usize = 1_500;

fn progress_noise_patterns() -> Option<&'static regex::RegexSet> {
    static PATTERNS: std::sync::OnceLock<Option<regex::RegexSet>> = std::sync::OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            regex::RegexSet::new([
                // Rust libtest: a passing test, with optional `--report-time` suffix.
                r"^test \S.* \.\.\. ok( <\d+(\.\d+)?s>)?$",
                // Cargo progress verbs, right-aligned to 12 columns (so
                // `Downloading` has a single leading space). `Blocking`, `Locking`,
                // `Adding` and `Running` stay: they explain stalls and changes.
                r"^\s+(Compiling|Downloading|Downloaded|Checking|Fresh) \S",
                // `go test -v`: run/pause announcements and passing results.
                r"^=== (RUN|PAUSE)\s+\S+$",
                r"^\s*--- PASS: \S+ \(\d+(\.\d+)?s\)$",
                // `pytest -v`: a passing test item.
                r"^\S+::\S+ PASSED(\s+\[\s*\d+%\])?$",
            ])
            .ok()
        })
        .as_ref()
}

/// Drop passing-test and build-progress lines from a captured stream.
///
/// Failures, warnings, summaries and every unrecognized line are kept. When
/// lines are removed, a single header states how many, so the omission is
/// explicit rather than silent. Streams shorter than [`MIN_FILTER_CHARS`] or
/// with fewer than [`MIN_NOISE_LINES`] matches are returned unchanged.
fn strip_progress_noise(text: &str) -> std::borrow::Cow<'_, str> {
    let Some(patterns) = progress_noise_patterns() else {
        return std::borrow::Cow::Borrowed(text);
    };
    if text.chars().count() < MIN_FILTER_CHARS {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut kept = String::with_capacity(text.len());
    let mut noise = 0usize;
    for line in text.split_inclusive('\n') {
        if patterns.is_match(line.trim_end_matches(['\r', '\n'])) {
            noise += 1;
        } else {
            kept.push_str(line);
        }
    }
    if noise < MIN_NOISE_LINES {
        return std::borrow::Cow::Borrowed(text);
    }
    std::borrow::Cow::Owned(format!(
        "[omitted {noise} passing-test/build-progress lines]\n{kept}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_small_output_not_truncated() {
        let mut cap = BoundedCapture::new();
        cap.push(b"hello\n");
        let (text, truncated) = cap.finish();
        assert_eq!(text, "hello\n");
        assert!(!truncated);
        assert!(!cap.was_truncated());
    }

    #[test]
    fn test_large_output_bounded_with_head_tail() {
        let mut cap = BoundedCapture::new();
        // Emit > 64KB with identifiable head and tail.
        let head_marker = b"HEAD-MARKER-";
        let tail_marker = b"TAIL-MARKER-";
        cap.push(head_marker);
        let filler = vec![b'x'; 100_000];
        cap.push(&filler);
        cap.push(tail_marker);
        let (text, truncated) = cap.finish();
        assert!(truncated);
        assert!(text.contains("HEAD-MARKER-"));
        assert!(text.contains("TAIL-MARKER-"));
        assert!(text.contains("output truncated"));
        // Bounded: head + tail + marker overhead, well under input size.
        assert!(text.len() < 100_000);
        assert!(text.len() <= CAPTURE_HEAD_BYTES + CAPTURE_TAIL_BYTES + 1024);
    }

    #[test]
    fn test_memory_does_not_grow_with_input() {
        let mut cap = BoundedCapture::new();
        for _ in 0..1000 {
            cap.push(&vec![b'a'; 10_000]);
        }
        let (text, truncated) = cap.finish();
        assert!(truncated);
        assert!(text.len() <= CAPTURE_HEAD_BYTES + CAPTURE_TAIL_BYTES + 1024);
        assert!(cap.total_bytes() == 10_000_000);
    }

    #[test]
    fn test_budget_command_output_under_budget() {
        let (out, err, truncated, warnings) = budget_command_output("hello", "");
        assert_eq!(out, "hello");
        assert_eq!(err, "");
        assert!(!truncated);
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_budget_command_output_trims() {
        let stdout = "a".repeat(10_000);
        let stderr = "b".repeat(2_000);
        let (out, err, truncated, warnings) = budget_command_output(&stdout, &stderr);
        assert!(truncated);
        assert!(!warnings.is_empty());
        let total = out.chars().count() + err.chars().count();
        assert!(total <= 7_000, "combined too large: {total}");
    }

    fn libtest_run(passing: usize) -> String {
        let mut out = format!("running {} tests\n", passing + 1);
        for i in 0..passing {
            out.push_str(&format!("test module::case_{i} ... ok\n"));
        }
        out.push_str("test module::broken ... FAILED\n\nfailures:\n\n---- module::broken stdout ----\nthread 'module::broken' panicked at src/lib.rs:9:5:\nassertion `left == right` failed\n\ntest result: FAILED. 80 passed; 1 failed\n");
        out
    }

    #[test]
    fn passing_test_lines_are_dropped_but_failures_and_summary_survive() {
        let (out, _, truncated, warnings) = budget_command_output(&libtest_run(80), "");
        assert!(out.starts_with("[omitted 80 passing-test/build-progress lines]\n"));
        assert!(!out.contains("case_0 ... ok"));
        assert!(out.contains("running 81 tests"));
        assert!(out.contains("test module::broken ... FAILED"));
        assert!(out.contains("panicked at src/lib.rs:9:5"));
        assert!(out.contains("test result: FAILED. 80 passed; 1 failed"));
        assert!(!truncated, "filtered output fits the budget");
        assert!(warnings.is_empty());
    }

    #[test]
    fn filtering_frees_budget_for_the_signal_that_used_to_be_cut() {
        // Raw output is far above the budget; the failure sits in the middle.
        let raw = libtest_run(600);
        assert!(raw.chars().count() > 6_000);
        let (out, _, truncated, _) = budget_command_output(&raw, "");
        assert!(!truncated);
        assert!(out.contains("assertion `left == right` failed"));
    }

    #[test]
    fn short_or_unrecognized_output_is_untouched() {
        let few = libtest_run(MIN_NOISE_LINES - 1);
        let (out, _, _, _) = budget_command_output(&few, "");
        assert_eq!(out, few);
        // Short logs stay intact even when many lines match: they are cheap
        // and may be exactly what was asked for.
        let short = libtest_run(20);
        assert!(short.chars().count() < MIN_FILTER_CHARS);
        let (out, _, _, _) = budget_command_output(&short, "");
        assert_eq!(out, short);
        let prose = "test results are in\ntest x ... ok but with prose after\n".repeat(20);
        let (out, _, _, _) = budget_command_output(&prose, "");
        assert_eq!(out, prose);
    }

    #[test]
    fn cargo_progress_is_dropped_but_diagnostics_and_stalls_stay() {
        let mut err = String::new();
        for i in 0..80 {
            err.push_str(&format!("   Compiling dep{i} v1.0.{i}\n"));
            err.push_str(&format!(" Downloading crates dep{i} v1.0.{i}\n"));
        }
        err.push_str("    Blocking waiting for file lock on build directory\n");
        err.push_str("error[E0308]: mismatched types\n --> src/main.rs:3:5\n");
        err.push_str("error: could not compile `app` (bin \"app\") due to 1 previous error\n");
        let (_, out, _, _) = budget_command_output("", &err);
        assert!(out.starts_with("[omitted 160 passing-test/build-progress lines]\n"));
        assert!(!out.contains("Compiling dep"));
        assert!(!out.contains("Downloading crates"));
        assert!(out.contains("Blocking waiting for file lock"));
        assert!(out.contains("error[E0308]"));
        assert!(out.contains("could not compile `app`"));
    }

    #[test]
    fn go_and_pytest_passing_lines_are_dropped_with_failures_kept() {
        let mut go = String::new();
        for i in 0..40 {
            go.push_str(&format!(
                "=== RUN   TestCase{i}\n--- PASS: TestCase{i} (0.00s)\n"
            ));
        }
        go.push_str("=== RUN   TestBad\n    bad_test.go:7: got 1, want 2\n--- FAIL: TestBad (0.00s)\nFAIL\n");
        let (out, _, _, _) = budget_command_output(&go, "");
        assert!(out.starts_with("[omitted 81 passing-test/build-progress lines]\n"));
        assert!(out.contains("--- FAIL: TestBad"));
        assert!(out.contains("got 1, want 2"));
        assert!(!out.contains("TestCase3"));

        let mut py = String::new();
        for i in 0..60 {
            py.push_str(&format!(
                "tests/test_a.py::test_ok_{i} PASSED [ {}%]\n",
                i * 10
            ));
        }
        py.push_str("tests/test_a.py::test_bad FAILED [100%]\n");
        let (out, _, _, _) = budget_command_output(&py, "");
        assert!(out.contains("tests/test_a.py::test_bad FAILED"));
        assert!(!out.contains("test_ok_3"));
    }
}
