# Architecture

dgc is a daemon that supervises Claude Code and Codex CLI processes, plus a
client binary (`dgc`) with a CLI and a terminal UI. This page maps the code. The
requirements and their reasons are in the
[design](../superpowers/specs/2026-11-25-orchestrator-design.md); decisions that
refine or depart from it (limits, timeouts, safety rules) are in
[PROGRESS "決定事項"](../orchestrator/PROGRESS.md#決定事項). Look a detail up
there or in the module docs rather than copying it here. User-visible behavior is
in the [README](../../README.md).

```
dgc (CLI / TUI) ── Unix socket, newline-delimited JSON-RPC ── dgc daemon run
                   (requests/responses + event notifications)   ├ Store, EventLog
                                                               ├ WorkspaceManager ── git
                                                               └ AgentManager ─ actor per agent
                                                                   ├ ClaudeAdapter ── claude -p (stream-json)
                                                                   └ CodexAdapter  ── codex app-server
```

## Crates

| Crate | Kind | Responsibility |
|---|---|---|
| `crates/dgc-protocol` | lib | Wire types: JSON-RPC envelopes and error codes, method names and params/results, ids (`prj_`/`ws_`/`agt_`/`perm_` + uuid v7), entities, normalized `AgentEvent`s, size limits and truncation/chunking helpers. No I/O. |
| `crates/dgc-daemon` | lib + test bin | The daemon: paths, config, lock, store, event log, workspaces, agents, provider adapters, process groups, socket server, subscriptions. `src/bin/dgc-fake-provider.rs` replays scenarios for tests. |
| `crates/dgc-client` | lib | One socket connection: `hello`, sequential `call`, subscriptions, and autostart (`connect_or_spawn` runs `dgc daemon run` in a new session). |
| `crates/dgc-cli` | bin `dgc` | Subcommands (`commands/`), human/JSON output with escaping (`output.rs`), and the TUI (`tui/`). |

Dependencies are deliberately few; adding one needs a reason in PROGRESS
"決定事項" (design §5.1).

## Daemon

Startup order (`server.rs`, `run_daemon`): instance lock → store → bind the
socket and set modes → resolve providers (`provider/resolve.rs`: configured
absolute path or the daemon's own `PATH`, `--version` with a timeout) →
`AgentManager::recover_on_startup` (agents that were live become `stopped`) →
accept. Shutdown stops accepting, flushes connections, shuts every agent down
(provider process groups included), then removes the socket and releases the
lock.

- **Paths** (`paths.rs`): XDG locations or `DGC_STATE_ROOT`; directories are
  created 0700.
- **Store** (`store.rs`): one versioned JSON file per project, workspace and
  agent; atomic writes, 0600, no symlink following. Unknown versions are
  rejected; `list_*` skips broken entries with a warning.
- **Event log** (`events_log.rs`): per agent `events.jsonl` with one rotated
  generation; `seq` is monotonic per agent and survives restarts. It holds
  prompts and agent output; besides it only the agent's `queue.json` (queued
  messages) and its record (title, by default the prompt's first line) contain
  prompt text.
- **Workspaces** (`workspace.rs`): projects, local and worktree workspaces.
  git runs as a bounded child with a sanitized environment. Removal of a
  worktree is gated by ownership, `is_managed_path`, and the archive safety
  check; destructive flags are confined to the force path.
- **Agents** (`agent.rs`, `agent/actor.rs`): one actor task per live agent owns
  its `ProviderSession` and message queue (persisted as `queue.json`). The
  manager talks to actors with commands and oneshot replies. Every event is
  appended to the log, then broadcast. Provider calls are bounded
  (`ProviderTimeouts`); a timeout fails the agent.
- **Processes** (`process.rs`): each provider leads its own process group;
  terminate is SIGTERM then SIGKILL to the group, and dropping a child kills
  it. Stdout is read line by line with a size cap; only a stderr tail is kept
  for error messages.
- **Handlers** (`handlers.rs`): parse params and map `WorkspaceError` /
  `AgentError` to RPC errors.

## Socket protocol

`server.rs` accepts connections on the Unix socket, checks the peer UID, and
requires `hello` (protocol major version) first. Messages are one JSON object per
line with a size cap; an oversized line closes the connection. Each connection
has one bounded outbound queue and a writer task; responses wait for room,
notifications do not. Methods are listed in `dgc-protocol/src/methods.rs`
(`project.*`, `workspace.*`, `agent.*`, `permission.*`, `subscribe` /
`unsubscribe`, `daemon.status` / `daemon.shutdown`).

Subscriptions (`subscription.rs`) forward `event` notifications for one agent
or all agents. A one-agent subscription can backfill from `since_seq` before
going live without gaps or duplicates. A client that cannot keep up gets one
`lagged` notification and the subscription ends; it recovers with `agent.events`
and subscribes again. `agent.events` pages are bounded by count and bytes, so
clients page until an empty page.

## Provider adapters

`provider/mod.rs` defines `ProviderAdapter` / `ProviderSession` and the contract
every adapter keeps (no text in errors, one `Exited`, nothing for an interrupted
turn after `interrupt` returns, bounded calls). Adapters translate a CLI's wire
format into normalized `ProviderEvent`s; the manager turns those into
`AgentEvent`s and state changes.

- **Claude** (`provider/claude.rs`): `claude -p` with stream-json input and
  output and `--permission-prompt-tool stdio`, so tool permission requests
  arrive as control requests on stdout and are answered on stdin. Claude cannot
  steer; the manager falls back to interrupt-and-send.
- **Codex** (`provider/codex.rs`): `codex app-server` over stdio JSON-RPC
  (`thread/start` or `thread/resume`, `turn/start`, `turn/steer`,
  `turn/interrupt`). Approval server requests become permission requests;
  unknown server requests are refused with an `error` event. The `--mode` maps
  to an approval policy and sandbox.

Wire shapes come from recordings of the tested CLI versions in
`crates/dgc-daemon/tests/fixtures/` (see its README). Tests replay recorded or
synthetic scenarios through `dgc-fake-provider`; they never start the real CLIs.

## TUI

`crates/dgc-cli/src/tui/` follows a pure model/update/render split:

- `state.rs`: `AppState` (tree, selection, streams, drafts, dialogs,
  connection, status). Nothing else holds UI state.
- `update.rs`: `update(state, AppEvent) -> Vec<Effect>`; the only place state
  changes. Key handling, history loading with generations, snapshot refresh,
  permission dialog arming and `seen_end` live here.
- `render.rs`: draws a frame from `&AppState` only.
- `layout.rs`: pure geometry, wrapping and width measurement shared by render
  and update (`permission_layout`), so what is drawn and what `a`/`s` accept
  cannot disagree.
- `input.rs`: key mapping and the line editor (paste handling).
- `backend.rs`: owns the RPC and subscription connections, executes `Effect`s,
  turns results and notifications into `AppEvent`s, reconnects after a
  disconnect, and bounds every call (reads and mutating calls separately).
- `mod.rs`: the runtime: terminal setup and restore (raw mode, alternate
  screen, bracketed paste, panic hook, signals), the input thread, ticks, and
  the loop that feeds events to `update`, hands effects to the backend and
  draws.

The permission-safety rules the TUI must keep are listed in
[AGENTS.md](../../AGENTS.md#tui-permission-safety).

## Security boundary

The trust boundary is the user's UID (design §9). Inside it every connection is
equal, including agents started by dgc. Outside it the daemon refuses: no
network listener, a 0700 socket directory and 0600 socket, and a peer UID check
on every connection. dgc never reads provider credentials; providers inherit the
daemon's environment. `tracing` (and so `daemon.log`) carries ids and states only;
prompt and agent text is stored only in each agent's 0600 state files
(`events.jsonl`, `queue.json`, the agent record's title), and
`scripts/check-sensitive-logging.py` guards the modules that handle it. dgc never
approves on the user's behalf: requests the provider raises wait for the user,
and only an interrupt or shutdown cancels them (answered deny or cancel). What
runs without a request is decided by the provider's mode and the user's provider
settings; auto-approve modes are used only when chosen explicitly and are always
shown. Every human-readable surface escapes daemon-supplied text before display.
