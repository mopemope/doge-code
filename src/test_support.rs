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
