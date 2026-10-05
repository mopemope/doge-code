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
