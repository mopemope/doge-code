# Agent development workflow

## Where guidance lives

[AGENTS.md](../../AGENTS.md) holds the stable rules (security invariants, TUI
permission safety, adapter contract, tests, commit conventions); `CLAUDE.md`
imports it. [Architecture](architecture.md) describes the crates and their
boundaries. Decisions and the hand-off state are in
[PROGRESS.md](../orchestrator/PROGRESS.md); the requirements are in the
[design](../superpowers/specs/2026-11-25-orchestrator-design.md). User-facing
behavior is documented only in the [README](../../README.md). The repository has
no shared agent skills.

## Toolchain

The development toolchain is pinned to 1.99.0 in `rust-toolchain.toml`; the MSRV
is `[workspace.package] rust-version` in the root `Cargo.toml` (1.94). A `cargo`
found first on `PATH` can be newer and report different Clippy warnings; if
`cargo --version` in the repository is not 1.99.0, prefix the commands with
`PATH=$HOME/.rustup/toolchains/1.99.0-<host-triple>/bin:$PATH`. After a
`Cargo.lock` change, build once without `--locked`, then use `--locked`.

## Verification matrix

Run the narrowest relevant test first. The wrapper commands run from any working
directory because it resolves the repository root from its own path. Python
3.11+ is required for development scripts; no third-party Python packages are
needed.

| Change | Focused checks | Before completion |
|---|---|---|
| Docs, development scripts, CI | `bash scripts/verify.sh guidance` | Review links and retained constraints; script tests run automatically |
| Protocol types (`dgc-protocol`) | `cargo test --locked -p dgc-protocol` | `rust` (every crate consumes the types) |
| Daemon: store, event log, workspaces, process, server | `cargo test --locked -p dgc-daemon` (narrow with `--test store`, `--test workspace`, `--test server`, ...) | `rust`; `macos` in CI for process/socket code |
| Agent manager, provider adapters | `cargo test --locked -p dgc-daemon --test agent_manager --test claude_adapter --test codex_adapter` | `rust`; manual real-provider check recorded in PROGRESS when wire behavior changes |
| Client (`dgc-client`) | `cargo test --locked -p dgc-client` | `rust` |
| CLI commands and output | `cargo test --locked -p dgc-cli --test agent_cli --test daemon_cli --test workspace_cli` | `rust`; README for visible changes |
| TUI | `bash scripts/verify.sh test tui::` | `rust`, `tui-deps`, manual TUI run and screenshot/recording |
| Cargo, dependencies, MSRV | Affected tests and builds | `rust`, `msrv`, `tui-deps` when the TUI graph changes |

`rust` runs `cargo fmt --all --check`,
`cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`, and
`cargo test --locked --workspace`. Format with `cargo fmt --all` before running
it. `test` mode runs `cargo test --locked --workspace <filter>`. `msrv` runs
`cargo check --locked --workspace` with the workspace `rust-version` via
`rustup run`; install the matching toolchain separately when needed. A missing
toolchain is an environment blocker. `macos` runs the platform-dependent tests
(process groups, termination, the Unix socket, `hello`, autostart, `tui::`) and
works only on macOS. `tui-deps` verifies that the declared Crossterm backend is
active and legacy 0.28 metadata is inactive. `guidance` runs the guidance
checker, the sensitive-logging checker and the development-script tests.

Logs are stored outside the repository in a new system temporary directory, or
in `--log-dir <path>`. Successful commands emit compact results; failures return
the command's exit code and a bounded diagnostic tail with the full log path.
Rust test mode rejects empty filters, zero matches, and ignored-only results;
guidance also rejects Python unittest discovery with zero executed tests. The
wrapper supports standard Rust and unittest result formats; unsupported formats fail
verification rather than guessing a pass. A compile or startup failure is reported as
`FAIL (<code>)` like a test failure (the wrapper does not distinguish them) and
never counts as a pass. Do not commit logs or user/environment values.

CI owns platform-specific gates. Reuse passing checks until their inputs change;
guidance-only work does not require rebuilding unrelated Rust code. For Rust
changes, full completion gates remain mandatory. Record any blocked gate.

## Automated contracts and their limits

`python3 scripts/check-agent-guidance.py` checks local Markdown links in the
tracked guidance (README, AGENTS.md, `docs/ai/`), that `CLAUDE.md` imports
AGENTS.md, that CI runs the wrapper's Rust gate and the MSRV/toolchain versions
agree, and that this file quotes each Rust gate command.
`python3 scripts/check-sensitive-logging.py` rejects `tracing` macro calls that
use known content identifiers (prompt, output, permission text) in the provider,
agent and TUI modules; it matches identifiers in source text and cannot prove
what a runtime value contains. Behavior
belongs in Rust tests; do not add prose or source-text checks that claim
runtime correctness.

## Task scope and completion evidence

For small changes, inspect affected code/callers and verify the result. For
investigation-only requests, report reproduction, evidence, and candidate causes
without edits. For visible specification changes, identify acceptance criteria
and unresolved product choices before implementing dependent behavior.

Use official sources when provider CLI or dependency facts can drift; use the
recorded fixtures, the locked version and local callers to determine
applicability. Avoid mandatory whole-repository or whole-document reads.

When delegation is authorized, assign independent scopes and file ownership.
Return evidence, paths, and unresolved issues rather than duplicating raw logs.
Measure total worker tokens as well as elapsed time; parallelism can cost more.

For long tasks, keep the state in PROGRESS.md ("現在の作業", 決定事項, 検証記録)
rather than in agent memory. Report acceptance evidence and environment
limitations at completion.
