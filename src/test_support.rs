//! Shared HTTP fixture server pool for tests.
//!
//! HTTP-heavy `httptest` suites spin up one listener per test. Bounding the
//! number of simultaneous listeners with a small fixed pool keeps
//! port/file-descriptor pressure down without serializing the whole suite.
//!
//! Test-only: wired into the binary via `#[cfg(test)] mod test_support` in
//! `src/main.rs`, so production builds never contain this helper.
//!
//! Contract: acquire the fixture with `HTTP_SERVER_POOL.get_server()` and let
//! the returned handle drop normally. `httptest` verifies expectations on
//! drop, and fixture startup failure panics — a required HTTP fixture that
//! cannot start is a test failure, never an implicit skip.

/// Bounded pool of local HTTP fixture servers.
///
/// Size is fixed small (4) to relieve listener pressure. Correctness must
/// never depend on this value: `get_server()` blocks until a server is free
/// when the pool is exhausted.
pub(crate) static HTTP_SERVER_POOL: httptest::ServerPool = httptest::ServerPool::new(4);

#[derive(Debug)]
pub(crate) struct FixtureAttemptDenied;
impl std::fmt::Display for FixtureAttemptDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("fixture attempt denied")
    }
}
impl std::error::Error for FixtureAttemptDenied {}

#[derive(Default)]
pub(crate) struct SingleAttemptPolicy {
    spent: std::sync::atomic::AtomicBool,
    pub(crate) reported: std::sync::atomic::AtomicU64,
}
impl crate::llm::client_core::RequestAttemptPolicy for SingleAttemptPolicy {
    fn before_attempt(&self) -> anyhow::Result<()> {
        use std::sync::atomic::Ordering;
        if self
            .spent
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(anyhow::anyhow!(FixtureAttemptDenied));
        }
        Ok(())
    }
    fn observe_usage(&self, usage: &crate::llm::types::Usage) {
        self.reported.fetch_add(
            u64::from(usage.total_tokens),
            std::sync::atomic::Ordering::SeqCst,
        );
    }
}

/// Per-future tracing writer; no global subscriber or shared test log state.
#[derive(Clone, Default)]
pub(crate) struct DiagnosticCapture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl DiagnosticCapture {
    pub(crate) fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl std::io::Write for DiagnosticCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Build a per-future JSON tracing subscriber writing to `capture`.
///
/// Uses the well-tested `tracing_subscriber::fmt` JSON formatter (no custom
/// `Layer`/`Subscriber` stacking). Each JSON line is one event object;
/// helpers below extract numeric fields as structured values instead of
/// `charged_tokens=1234` substring matching. No global state; each test owns
/// its capture. Production code unchanged.
pub(crate) fn json_subscriber(
    capture: &DiagnosticCapture,
) -> impl tracing::Subscriber + Send + Sync {
    let writer = capture.clone();
    tracing_subscriber::fmt()
        .json()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(move || writer.clone())
        .finish()
}

fn find_u64_key(value: &serde_json::Value, key: &str) -> Option<u64> {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if k == key {
                    if let Some(n) = v.as_u64() {
                        return Some(n);
                    }
                    // u128 delays (e.g. `as_millis()`) may exceed JSON u64?
                    // Our hints (1000/60000/300000) fit u64; try i64/string.
                    if let Some(n) = v.as_i64().and_then(|n| u64::try_from(n).ok()) {
                        return Some(n);
                    }
                    if let Some(s) = v.as_str()
                        && let Ok(n) = s.trim().parse::<u64>()
                    {
                        return Some(n);
                    }
                }
                if let Some(found) = find_u64_key(v, key) {
                    return Some(found);
                }
            }
            None
        }
        serde_json::Value::Array(items) => items.iter().find_map(|v| find_u64_key(v, key)),
        _ => None,
    }
}

/// Structured retry delay from JSON fmt lines.
///
/// Prefers Responses `retry_delay_ms`, falls back to chat-completions
/// `wait_ms`. Returns the exact server-honoured delay; callers assert it
/// equals the expected hint to prove no shortening.
pub(crate) fn parse_retry_delay_ms(text: &str) -> Option<u64> {
    text.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(|v| find_u64_key(&v, "retry_delay_ms").or_else(|| find_u64_key(&v, "wait_ms")))
}

/// Poll `capture` for a structured retry delay with explicit timeout.
///
/// Uses the robust `FmtSubscriber` JSON output (not custom layers) plus
/// numeric parsing. Returns the delay on success; callers assert the exact
/// value. `server_received` should already be true (explicit `watch` signal);
/// this additionally proves the 503 was processed and wait scheduled.
pub(crate) async fn wait_for_retry_delay(
    capture: &DiagnosticCapture,
    timeout: std::time::Duration,
) -> Option<u64> {
    let start = std::time::Instant::now();
    loop {
        if let Some(delay) = parse_retry_delay_ms(&capture.text()) {
            return Some(delay);
        }
        if start.elapsed() >= timeout {
            return parse_retry_delay_ms(&capture.text());
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}
