---
name: dgc-execution
description: Change doge-code (dgc) process execution, policy, timeouts, cancellation, process cleanup, or managed TUI jobs.
---

# dgc execution

Start at the affected boundary: `src/execution/runner.rs` for managed process
mechanics, `process.rs`/`policy.rs` for LLM execution policy, `lifecycle.rs` for
cleanup, and `src/jobs/` for user-visible ownership and shutdown.
Read [execution and job contracts](../../../../docs/ai/contracts.md) when changing those boundaries.

- Keep the runner policy-free and FsTools adapters thin. Direct execution passes
  program and argv separately; never introduce a shell or prefix-match programs.
- Finite background commands reuse the runner. PTY, MCP transport, or service
  exceptions must be intentional and documented. Long-running TUI jobs use JobManager.
- Preserve bounded stdout/stderr, timeout/cancellation distinction, explicit
  reaping, Unix process-group cleanup, and future-drop cleanup.
- Trusted `/test` and `/lint` use managed mechanics with their diagnostic budget,
  independently of the LLM program allowlist and tool-output budget.

Select regression tests for the changed risk: nonzero exit/spawn error, large
output, timeout, cancellation, descendant cleanup, or future drop. Start with
`bash scripts/verify.sh test execution::`; add `jobs::`, dispatch, workflow, or
trusted command tests when those callers change. Unix/macOS behavior requires
validation on the affected platform.
