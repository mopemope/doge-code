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
        if chunk.is_empty() {
            return;
        }
        self.total_bytes += chunk.len() as u64;
        if self.head.len() < CAPTURE_HEAD_BYTES {
            let take = (CAPTURE_HEAD_BYTES - self.head.len()).min(chunk.len());
            self.head.extend_from_slice(&chunk[..take]);
            if take == chunk.len() {
                return;
            }
            self.push_tail(&chunk[take..]);
        } else {
            self.push_tail(chunk);
        }
        if self.total_bytes > (CAPTURE_HEAD_BYTES + CAPTURE_TAIL_BYTES) as u64 {
            self.truncated = true;
        }
    }

    /// Append to the tail window, evicting the oldest bytes past the limit.
    /// One `drain` per call, proportional to the chunk (read in 8KB units).
    fn push_tail(&mut self, chunk: &[u8]) {
        self.tail.extend_from_slice(chunk);
        if self.tail.len() > CAPTURE_TAIL_BYTES {
            let excess = self.tail.len() - CAPTURE_TAIL_BYTES;
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
}
