# doge-code development

This Rust repository builds `dgc`, a local orchestrator (daemon + CLI/TUI) that
supervises Claude Code and Codex CLI agents. Respond in Japanese.
User-facing behavior belongs in `README.md`; development guidance starts here.

## Work according to the request

- For investigation requests, establish reproduction and evidence before proposing
  a fix; edit only when implementation is authorized.
- For small local changes, read the affected code and callers. For cross-module
  changes, record acceptance criteria, affected boundaries, and focused checks.
- Use current official sources for external APIs, provider CLI behavior, or
  dependency upgrades. Local behavior is established by code, tests and the
  recorded fixtures.
- Use `rg` and bounded reads. Clarify product decisions that affect visible
  behavior; otherwise continue authorized work without repeated approval requests.
- Delegate only when authorized and tasks are independent. Give each worker a
  bounded scope, required evidence, and file ownership; count total token use.
- Distinguish observed user directives from interpreted requirements.

## Product priority

dgc is a trustworthy local orchestrator that safely supervises Claude Code and
Codex agents. In order: keep the security invariants below, relay permission
requests correctly (nothing approved that the user did not approve), and recover
cleanly (daemon restart, disconnects, crashed providers). Adding providers, UI
breadth or features is not a goal in itself; new work follows the roadmap in
the [design](docs/superpowers/specs/2026-11-25-orchestrator-design.md).

## Status and next work

[PROGRESS.md](docs/orchestrator/PROGRESS.md) is the hand-off file for Claude
Code and Codex: read "現在の作業" first and resume there; "決定事項" records
decisions that depart from the design. Update it in the same commit as each
finished task, and before stopping.

## Guidance

| Topic | Read |
|---|---|
| Crates, daemon, protocol, adapters, TUI, security boundary | [architecture](docs/ai/architecture.md) |
| Which checks to run, CI, development scripts | [workflow](docs/ai/workflow.md) |
| Requirements and decisions | [design](docs/superpowers/specs/2026-11-25-orchestrator-design.md), PROGRESS "決定事項" |
| Provider wire formats | [fixtures README](crates/dgc-daemon/tests/fixtures/README.md) |

## Crates and dependencies

- `dgc-protocol`: protocol types, error codes, normalized events. No I/O.
- `dgc-daemon`: store, event log, workspaces, agent manager, provider adapters,
  process groups, socket server, subscriptions.
- `dgc-client`: socket connection, calls, subscriptions, autostart.
- `dgc-cli`: the `dgc` binary (CLI subcommands, `daemon run`, TUI).
- Keep dependencies minimal (design §5.1). Add one only with a reason recorded
  in PROGRESS "決定事項", declared in root `[workspace.dependencies]` with only
  the features the crate uses.

## Security invariants

Do not weaken these.

- The daemon opens no network listener. Socket directory 0700, socket 0600,
  peer UID checked on every connection. State files 0600, never written
  through symlinks.
- The trust boundary is the peer UID. Parent/child checks constrain agent
  creation within that UID.
- dgc never reads, stores or forwards provider credentials. Providers inherit
  the daemon's environment (an on-demand daemon inherits the environment of the
  command that started it); client environments are never sent over the socket.
- `tracing` (and so `daemon.log`) never carries prompts, agent or permission
  text, `dgc.json` contents, script commands or script output. Use IDs, script
  names, states and error kinds. Agent and permission text is stored in the
  agent's 0600 state files (`events.jsonl`, `queue.json`, and the agent record,
  whose title defaults to the prompt's first line). Schedule prompts and names
  are stored in 0600 `<state>/schedules/<id>.json` records. Trusted script
  configurations are stored in 0600 `<state>/script-trust/<project_id>.json`
  records. Script output is stored in 0600
  `<state>/scripts/<workspace_id>/<name>.log` and its rotated `.log.1`.
  `check-sensitive-logging.py` enforces this for every `crates/*/src` directory.
- Worktree deletion only for dgc-owned workspaces whose path is inside the
  managed worktree directory, with the archive safety check (no uncommitted or
  untracked work, no commits outside the base) unless `--force`; destructive
  git flags stay in the force-only path. Branches are deleted only when the
  worktree entry still checks out the recorded branch.
- dgc never approves on the user's behalf. Requests the provider raises wait
  for the user; only an interrupt or shutdown cancels them (answered deny or
  cancel). What runs without a request is decided by the provider's mode and
  the user's provider settings (Codex `workspace-write` runs sandboxed commands,
  Claude applies the user's allow rules). Auto-approve modes are used only when
  chosen explicitly and are always shown (`warning:` line, `auto-approve`, `[auto]`).
- Human output escapes control and bidi characters (`output::escape_text`/`escape_line`).

## TUI permission safety

These stop a keystroke meant for something else from approving an unread
request; changing one needs an explicit decision in PROGRESS.

- The dialog never opens by itself in Input focus or over another dialog; only
  in the tree with an empty input, or by `p`.
- `a`/`s`/`d` are ignored for `PERMISSION_ARM_TICKS` after it opens, switches
  request, or the terminal is resized.
- `a`/`s` require `seen_end`: every summary row was on screen, read
  contiguously from the top; `End` alone does not count. A window too small
  for the layout keeps them off. Render and update share `permission_layout`.
- Wrapped continuation rows carry the `↪` gutter; a literal `↪` in the summary
  is escaped. Widths are measured per grapheme with ratatui's cell width.
- Summaries and agent names are escaped before display.
- Drafts belong to the agent they were typed for and are sent only to it.
- Bracketed paste goes only into a text field as one line; it never reaches
  the tree, the permission dialog or the archive confirmation.

## Provider adapters

Follow the contract in `crates/dgc-daemon/src/provider/mod.rs`: no prompt or
output text in errors; `Exited` once on unexpected exit; no `TurnCompleted` or
`PermissionRequested` for a turn after `interrupt` returns; bounded time for
every method. Base wire behavior on recorded fixtures and record the tested CLI
version.
Add the dgc MCP launch arguments only when `AgentSpec.mcp` is set and keep other user MCP servers available.

## Tests

- Use `tempdir()` and `DGC_STATE_ROOT`, never the user's real state, config or
  provider CLIs. Never leave daemons or fake processes running after a test.
- Drive providers with `dgc-fake-provider` and the recorded or hand-written
  (marked synthetic) scenarios under `crates/*/tests/fixtures/`.
- Typed errors or `?` in production; message-bearing `expect()` is fine in
  tests. Cover behavior and error paths.

## Verification and completion

- Use the pinned toolchain 1.99.0 (`rust-toolchain.toml`). The `cargo` on
  `PATH` may be newer and report different Clippy warnings; if so prefix
  commands with `PATH=$HOME/.rustup/toolchains/1.99.0-<host>/bin:$PATH`.
- First run matching tests: `bash scripts/verify.sh test <module_or_name>`.
  The wrapper rejects zero executed tests.
- Before finishing Rust changes: `cargo fmt --all`, then
  `bash scripts/verify.sh rust` (fmt check, warning-free Clippy, locked tests).
- Docs and development-script changes: `bash scripts/verify.sh guidance`.
- Dependency/MSRV changes also `msrv`; TUI or dependency graph changes also
  `tui-deps`; `macos` (CI, or on a Mac) runs the whole workspace suite on macOS.
- Reuse passed checks until relevant files change. Report command, result and
  blocked checks; blocked checks are not passes. Real-provider checks are
  manual (login, tokens) and recorded in PROGRESS.
- Update README for new commands, flags, keys and visible behavior. TUI changes
  need a screenshot or terminal recording.
- Commits use `type(scope): summary`. Never commit credentials, runtime state
  or unrelated untracked files; stage paths explicitly.
