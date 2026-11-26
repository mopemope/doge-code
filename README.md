# Doge-Code (`dgc`)

`dgc` is a local orchestrator for coding agents. A small daemon starts and
supervises [Claude Code](https://github.com/anthropics/claude-code) and
[Codex](https://github.com/openai/codex) CLI sessions in your projects, keeps
them running when no client is attached, and relays every permission request
the agent asks for to you. The `dgc` command line and its terminal UI talk to
the daemon.

dgc does not call LLM APIs itself; each provider CLI does its own model calls
and its own authentication.

## Why

An orchestrator holds the keys to everything its agents can touch: files,
commands, credentials. dgc exists so that layer is small enough to audit and
closed by default:

- **Local only.** The daemon listens on a Unix socket that only your UID can
  connect to. It opens no TCP or UDP port and has no remote access.
- **No credential handling.** dgc never reads, stores or forwards API keys or
  login tokens. Claude Code and Codex keep their own logins; they inherit the
  daemon's environment, including any keys in it (see
  [Security properties and limits](#security-properties-and-limits)).
- **No network activity of its own.** No telemetry, no self-update, no
  downloading of code.
- **Permissions go to you.** Unless you choose an auto-approve mode for an
  agent, dgc adds no approvals of its own; requests the provider raises wait
  for your answer. An interrupt or a daemon stop cancels pending requests
  (answered deny or cancel). Without `--mode`, the provider's own default mode
  decides what runs without asking; see
  [Security properties and limits](#security-properties-and-limits).
- **Few dependencies.** The crates use tokio, serde, clap, ratatui/crossterm and
  a handful of small libraries.

Design: [orchestrator design](docs/superpowers/specs/2026-11-25-orchestrator-design.md).
Status and decisions: [PROGRESS](docs/orchestrator/PROGRESS.md).

## Requirements

- Linux or macOS (Unix sockets; Windows is not supported).
- Rust 1.94 or newer to build. The repository pins 1.99.0 in
  `rust-toolchain.toml`, which rustup uses automatically.
- The `claude` and/or `codex` CLI, installed and logged in. Tested versions:
  Claude Code 2.1.332 and codex-cli 0.172.0. Other versions work but the daemon
  logs a warning and `dgc daemon status` marks them `untested`.

## Install

```sh
cargo install --locked --path crates/dgc-cli    # installs ~/.cargo/bin/dgc
# or
cargo build --locked --release -p dgc-cli       # builds target/release/dgc
```

## Quick start

```sh
dgc daemon status                          # starts the daemon, shows the providers it found
dgc project add ~/src/app                  # prints the project line: ID  NAME  PATH
dgc workspace create --project PRJ --worktree --branch fix-bug
dgc run --workspace WS --provider claude --follow "Fix the failing test"
dgc permit ls                              # from a second terminal
dgc permit allow PERM
dgc send AGENT --behavior queue "Then update the changelog"
dgc                                        # the terminal UI
```

With the global `--json` flag the commands print machine-readable JSON (except
`daemon start` / `stop`; the TUI refuses it). The CLI starts the daemon on demand, so you rarely need the daemon
commands.

## Daemon

`dgc daemon {run [--detach] | start | stop | status}`:

- `run` runs the daemon in the foreground, logging to stderr; `--detach` starts
  it in the background in a new session and returns once it answers.
- `start` starts it in the background unless it is already running; `stop` asks
  it to shut down and waits until it has released its socket and lock.
- `status` (starting the daemon if needed) shows its pid, protocol version,
  start time, the `PATH` it runs with, and each provider's path and version
  (`tested` / `untested`, or `not found`).

A daemon started in the background (by `start`, `--detach`, or any other
command) logs to `<state>/daemon.log`. `DGC_LOG=error|warn|info|debug|trace`
sets the level (default `info`). Only one daemon runs per state directory.

Providers: at start the daemon uses `claude_path` / `codex_path` from
`<config>/config.toml` (absolute paths), or else finds `claude` / `codex` on its
own `PATH`, and runs `--version` once. A provider that is not found cannot run
agents. Providers inherit the daemon's environment, and a daemon started on
demand inherits the environment of the command that started it; to change the
environment, restart the daemon.

When the daemon stops it ends every agent's provider process; after a restart
those agents show as `stopped` and `dgc resume` continues them from their
provider session. Permission requests pending at shutdown are lost.

## Projects and workspaces

`dgc project {add [PATH] | ls | rm ID}` registers a directory (default: the
current one), lists projects, or removes a project that has no active
workspaces.

`dgc workspace {create --project ID [--worktree --branch NAME [--base REF]
[--slug S]] | ls [--project ID] | archive ID [--force]}`:

- A local workspace (the default) uses the project directory and is never
  deleted.
- A worktree workspace is a git worktree dgc creates on a new branch (from
  `--base`, default `HEAD`) under `<worktrees>/<repo>-<id>/<slug>`.
- `ls` lists active workspaces.

Archiving a workspace first archives its agents. For a worktree workspace it
removes the worktree (`git worktree remove`) and safely deletes its branch
(`git branch -d`), but only for paths inside that managed directory. Archive is
refused, and nothing (agents included) is changed, while the worktree has
uncommitted or untracked changes or commits not reachable from the commit it
was created from (the error names which); `--force` removes it anyway and
force-deletes the branch. The check is reachability from that base commit only,
so a branch already merged into main that still carries commits beyond its base
also needs `--force`. Untracked files count as work regardless of git's
`status.showUntrackedFiles`, but gitignored files in the worktree (build output,
`.env`, ...) are removed by a non-force archive, since git treats them as
disposable. When git refuses the safe branch delete the branch is kept and
`archive` prints `branch kept: NAME` (`--json`: `"branch_kept": true`, and
`"worktree_removed"` tells whether the directory was removed). If the project
directory was deleted or moved, archiving one of its worktree workspaces (no
`--force` needed) only archives the record and prints
`worktree kept on disk: PATH`: delete the leftover directory under
`<worktrees>/` yourself. The project can then be removed.

## Agents

- `dgc run --workspace ID [--provider claude|codex] [--profile NAME] [--model M] [--mode MODE]
  [--title T] [--follow] PROMPT` starts an agent and prints its id (`--json`:
  the agent record). Supply a provider or a profile. The title defaults to the
  prompt's first line (60 chars).
  `--follow` then prints its events until it is idle, stopped, failed or
  archived; the exit status is 0 only when it ended idle.
- `--mode` is the provider's permission mode. Claude: `acceptEdits`, `auto`,
  `bypassPermissions`, `manual`, `dontAsk`, `plan`. Codex: `read-only`,
  `workspace-write` (default; sandboxed commands inside the workspace run
  without asking), `untrusted` (asks before running commands), `full-access`.
  Auto-approve modes (Claude `bypassPermissions`, `auto`, `acceptEdits`; Codex
  `full-access`) let the provider act without asking: `run` always prints a
  `warning: ... auto-approve ...` line on stderr for them (also with `--json`),
  and `ls` marks such agents `auto-approve`.
- `dgc ls [--all]` (archived agents only with `--all`), `dgc show ID`,
  `dgc interrupt ID`, `dgc archive ID`, `dgc resume ID` (restarts a stopped or
  failed agent, reusing its provider session when available). If the provider
  failed to start, resume sends the original prompt from the queue.
- `dgc send ID [--behavior queue|steer|interrupt] TEXT` prints the outcome:
  `sent` (the agent was idle), `queued` (sent when the turn ends), `steered`,
  or `interrupted_and_sent` (also used when a provider cannot steer). An
  interrupt (`dgc interrupt`, `send --behavior interrupt`, or a steer that
  falls back to one) discards the queued messages instead of sending them
  later; each shows in `logs` as `queued message discarded: TEXT`.

### Launch profiles

Define named launch settings in the daemon's `config.toml` and restart the
daemon after editing the file.

```toml
[profiles.review]
provider = "claude"
model = "haiku"
mode = "plan"
notes = "Review changes before editing"

[profiles.quick-edit]
provider = "codex"
mode = "full-access"
notes = "Runs commands and edits files without asking"
```

Names contain 1–32 ASCII letters, digits, underscores, or hyphens. `provider`
must be `claude` or `codex`. `model`, `mode`, and `notes` are optional
strings. The daemon checks profile names, fields, and provider modes at startup,
even when a provider CLI is unavailable.

- `dgc profile ls [--json]` lists profiles by name, including their notes.
- `dgc run --workspace ID --profile review "Review the latest changes"` uses
  the profile's settings. `--model` and `--mode` override those settings.
  If you also supply `--provider`, it must match the profile's provider.
- Choosing a profile with an auto-approve mode enables that mode when you
  launch the agent. `profile ls` marks such profiles `auto-approve`, and
  `run` prints the usual warning if the effective mode auto-approves.

### Permissions

`dgc permit ls [--agent ID]` lists pending requests
(`PERM_ID  AGENT_ID  KIND  SUMMARY`, oldest first); `dgc permit allow ID
[--session]` and `dgc permit deny ID` answer one. Requests stay pending until
answered, interrupted, or the daemon stops. A summary longer than 4 KiB is cut
and ends with `…(truncated, N bytes total)`; the provider's payload is in
`permit ls --json` (`raw`, up to 64 KiB; larger payloads are replaced by
`{"truncated":true}`). A Codex request for extra sandbox permissions (network
access, file system paths) is listed with kind `other` and a
`permissions: ...` summary; `allow` grants them for the turn,
`allow --session` for the session, `deny` grants none.

### Logs

`dgc logs ID [--since SEQ] [--tail N] [--follow]` prints the stored events
after `SEQ` (all events by default). `--tail N` prints the newest `N` events in
sequence order and overrides `--since`. The tail page holds up to 1000 events
within 768 KiB. If the newest event alone exceeds that budget, a positive `N`
still returns that event. `--follow` keeps streaming until Ctrl-C or the daemon stops.
`--json` prints one event JSON object per line. Each human line is `[seq] <event>`:
`user (queue): TEXT`, `assistant: TEXT`, `tool: NAME SUMMARY`,
`tool result: SUMMARY` / `tool error: SUMMARY`,
`permission requested PERM_ID (KIND): SUMMARY`,
`permission resolved PERM_ID: allow|allow_for_session|deny|cancelled`,
`turn started`, `turn completed`, `rate limited: DETAIL`, `error: MESSAGE`,
`state: STATE`, `queued message discarded: TEXT`. Continuation lines of
multi-line text are indented by four spaces. A prompt or reply longer than
256 KiB is stored as several events (reply parts before the last show as
`assistant (partial)`). Human-readable output shows control characters in
daemon-supplied text (agent output, summaries, errors) escaped (`\r`, `\t`,
`\x1b`, `\u{202e}`; newlines are kept only where text may span lines), so a
terminal escape cannot hide or rewrite what is displayed.

### Handoff between providers

You can hand off an unarchived agent to the other provider before or after
it reaches a usage limit. The new agent starts in the same workspace and the
source agent remains available. Both agents can work in the same checkout
concurrently.

```sh
dgc handoff ID --to codex --dry-run
dgc handoff ID --to codex --follow
```

- `dgc handoff ID [--to claude|codex] [--profile NAME] [--model M] [--mode MODE]
  [--dry-run] [--follow] [--json]` defaults to the other provider when you omit
  both `--to` and `--profile`. `--to` must match the selected profile.
  `--model` and `--mode` override the profile. The new agent uses the target
  provider's default mode unless you choose a mode or a profile that sets one.
  Auto-approve modes print a warning on stderr.
- `--dry-run` prints the prompt without starting an agent. A launch prints
  the new agent's ID and optionally follows it. A CLI launch uses the
  current state and sends no expected preview, even after a separate dry run.
  With `--json`, the first result contains `prompt` and `agent` (`null` for
  a dry run).
- `run --follow` and `logs --follow` print one handoff suggestion when they
  encounter a rate limit. In JSON mode they emit the event as usual.

dgc assembles the prompt locally without calling an LLM. It uses the source
title, the first 16 KiB of the initial instruction, up to three recent
instructions, the last answer from a turn without a rate limit or error
(tail up to 4 KiB), `git status --short` and `git diff --stat` (up to
4 KiB apiece), and the absolute path to the source event log. A truncated
initial instruction points to the full text in the log. A missing or
rotated-out initial instruction is marked unavailable.

Environment variables, configuration and credential files, account or plan
metadata, and rate-limit details are outside the prompt's sources. Values
already present in conversation text and filenames can appear. The prompt is
capped at 32 KiB. If the title and log path leave too little space for the
required headings and preface, handoff returns an error.

dgc records at most one `rate limited` event per turn. Claude Code triggers
it on a rejected or unknown `rate_limit_event` status string or an HTTP 429 in a
result or assistant error. It ignores `allowed` and `allowed_warning`.
Codex triggers it when `account/rateLimits/updated` reports a non-null
`rateLimitReachedType`, or when an error reports `usageLimitExceeded` or
`rateLimitExceeded` with `willRetry` false or absent. The event carries the
provider, a reason of up to 48 characters, and a Unix reset time when
available. The detection conditions come from recorded provider traffic and
schemas and are tested with synthetic scenarios. The real-provider handoff
was tested with Claude Haiku and Codex GPT-6 Luna. Real usage-limit exhaustion
remains unobserved.

## Terminal UI

`dgc tui` (or `dgc` with no arguments) opens a full-screen terminal UI for all
projects, workspaces and agents. It needs a terminal on stdout (and no
`--json`), starts the daemon if it is not running, and works against the same
state as the other commands. Quitting leaves the daemon and every agent
running; the next `dgc tui` shows them again. Recorded sessions cover the
[general TUI](docs/orchestrator/m2-tui-screen.txt) and
[profiles and handoff](docs/orchestrator/m4-tui-screen.txt).

Layout: on the left the tree (project, workspace, agent) with a state symbol per
agent (`●` starting/running, `○` idle, `!` waiting for a permission, `✗`
failed, `■` stopped; `[auto]` marks an auto-approve mode). On the right the
selected agent's stream (a tail page on selection, then live events, with the last
2000 displayed lines kept),
below it the input line, and at the bottom a status bar (connection, agent
count, agents waiting for a permission, errors, key hints). In a narrow tree
(under 50 columns) provider names are dropped. Agents and permissions update
live. Projects and workspaces added or archived by other commands appear on the
next reload. The TUI requests one after 5 s with no reload pending, or on demand
with `R`. With no project the tree says
`no projects — add one with: dgc project add <path>`.

Keys in the tree:

| Key | Action |
|---|---|
| `↑` `↓` / `k` `j` | move; `←` collapse or go to the parent; `→` expand or enter |
| `Enter` | expand/collapse a project or workspace; on an agent, start typing |
| `i` | type to the selected agent |
| `n` | new agent in the selected workspace (form: profile, provider, model, mode, title, prompt) |
| `w` | new workspace in the selected project (local, or worktree with branch and optional base) |
| `H` | preview and confirm a handoff of the selected agent |
| `x` | interrupt the selected agent (discards its queued messages) |
| `r` | resume a stopped or failed agent |
| `A` | archive the selected agent (asks first: `y`/`Enter` or `n`/`Esc`) |
| `p` | jump to the next agent with a pending permission and open its dialog |
| `R` | reload the tree from the daemon now (it also reloads every 5 s) |
| `Tab` / `Shift-Tab` | change the send mode: queue, steer, interrupt |
| `PgUp` / `PgDn` | scroll the stream |
| `q`, `Ctrl-C` | quit (`Ctrl-C` works everywhere) |

In forms `↑` `↓` move between fields, `Tab` switches the provider (or the
workspace isolation), `Enter` creates, and `Esc` cancels. The new-agent form
loads profiles when it opens. On the profile field, `Tab` and `Shift-Tab` cycle
through `(none)` and the profiles by name. Choosing a profile fills provider,
model and mode. You can edit these fields before creating the agent. Choosing
`(none)` resets these three fields to their initial values. Clearing model
or mode uses the provider default, and the form warns in red whenever the
effective mode auto-approves. The new-agent form needs a prompt, and a worktree
workspace needs a branch. Failures stay in the form. A created agent or
workspace is selected only if its form is still open when the daemon answers;
after `Esc` it is still created, but the cursor stays where you left it.

When the TUI receives a usage-limit event live or loads it from the selected
agent's history, it marks that agent with `⇄` in the tree and shows a handoff
hint above its stream. The hint clears after the agent completes a later turn
without a limit, or is archived. Select any agent in the tree and press `H` for a
handoff. In the dialog, use `↑`/`↓` to select the target, profile or mode
field. `Tab` switches the target on the target field, and `Tab`/`Shift-Tab`
cycle profiles on the profile field. The mode field accepts text or paste
and shows a red warning for auto-approve modes. Use `PgUp`/`PgDn` to scroll
the escaped dry-run preview. Once the preview is visible and the brief
confirmation delay has passed, press `Enter` to start or `Esc` to cancel.
The daemon compares the rebuilt prompt with the preview. If they differ,
the TUI displays a fresh preview and asks you to confirm again. Pending
permission dialogs wait while the handoff dialog is open.

Sending: `i` focuses the input; `Enter` sends the line to the agent that was
selected when you pressed `i` (the input title shows it: `→ agt_…xxxxxx TITLE`).
An unsent draft belongs to the agent it was typed for: after `Esc`, selecting
another agent shows that agent's own draft (empty if none) and never sends the
first agent's text; going back restores it. `Tab` changes the mode (the active
one is highlighted in the input title), `PgUp`/`PgDn` scroll, `Esc` goes back to
the tree. Pasting (bracketed paste) inserts the text into the input or the
focused form field as one line: line breaks and tabs become spaces, so a paste
never sends, never changes the mode and never submits a form. A paste in the
tree, the permission dialog or the archive confirmation is ignored. The modes
are the `dgc send` behaviors: `queue` (default; sent when the current turn
ends), `steer` and `interrupt`. Claude agents cannot steer, so there `steer`
interrupts and then sends, which also discards the queue. A successful send
with `steer` or `interrupt` returns the input to `queue`. When the daemon
accepts a message the status bar says what happened to it for a few seconds:
`queued — delivered when the turn ends`, `sent`, `steered` or
`interrupted and sent`. A queued message appears in the stream only when it is
delivered (or shown as discarded). If the send fails, the mode stays selected,
the text returns to an empty input, and the error shows in the status bar. If the
agent you were typing to is archived or disappears, the input keeps its text and shows
`no target — select an agent`; leave with `Esc` and pick another agent. The
text stays with that removed agent and is not shown again.

### Permission dialog

A pending permission of the selected agent opens a dialog automatically when
you are in the tree and the input is empty. While you type in the input it never
opens by itself; the input title and the status bar show
`permission waiting — Esc then p to review` instead (`p` opens it, also from a
non-empty input after `Esc`). The dialog shows the agent, the kind and dgc's
one-line summary of the request, not the raw request payload. For Claude, Bash
shows the command, file tools (Write, Edit, ...) show only the path and never
the content to be written, and other tools show the first of `command`,
`file_path`, `notebook_path` or `path`, else the input as compact JSON. For
Codex it is the command, a summary of the file change, or of the requested
permissions. The provider's payload is in `dgc permit ls --json` (`raw`, up to
64 KiB; larger payloads are replaced by `{"truncated":true}`). Long summaries
wrap; a `↪` in the left margin marks a wrapped continuation, not a new line.
Rules, all there so a keystroke you typed for something else cannot approve a
summary you did not read (reading the summary is not the same as reading
everything the agent will do, for example the content of a file it writes):

- `a` (allow once), `s` (allow for the session) and `d` (deny) are ignored for
  0.5-0.75 s after the dialog opens, after it switches to another request, and
  after the terminal is resized ("keys enabled shortly").
- `a` and `s` work only after the whole summary has been on screen: scroll from
  the top with `↑` `↓` / `PgDn` until the footer stops saying `scroll to the
  end to enable [a]/[s]`. Jumping with `End` does not count unless the rows
  between were visible too. A window too small to show the request keeps `a`
  and `s` off.
- `d` is available once the keys are enabled. `Esc` always closes the dialog
  without answering: the request stays pending and `p` reopens it.
- A summary that dgc had to shorten (the daemon cuts summaries at 4 KiB) shows
  `this request was shortened by dgc — the full command is not shown`. That is
  one case where the summary is incomplete; allow only if you are content to
  approve something you cannot see in full.

### Disconnects

If the daemon goes away (`dgc daemon stop`, a crash, an old daemon that cannot
answer) the status bar shows `daemon disconnected: REASON` and the last tree
stays on screen, but nothing can be sent. The TUI retries every second, spawns
the daemon only on its first connection attempt (never again after a
disconnect), and reloads everything when `dgc daemon start` brings it back
(agents the restart ended show as stopped; resume them with `r`). A request that
gets no answer in time counts as a disconnect: 15 s for reads, 90 s for
requests that change something (send, create, interrupt, resume, archive,
permission answers), which the daemon may spend waiting on the provider or git.

## Storage and environment

Locations follow XDG. `DGC_STATE_ROOT=<dir>` replaces all of them with
`<dir>/run`, `<dir>/state`, `<dir>/config` and `<dir>/data` (for tests and
experiments).

| What | Location |
|---|---|
| Socket | `$XDG_RUNTIME_DIR/dgc/daemon.sock`, else `~/.local/state/dgc/run/daemon.sock` |
| Projects, workspaces, agents, events, lock, `daemon.log` | `$XDG_STATE_HOME/dgc` (`~/.local/state/dgc`) |
| Config (`claude_path`, `codex_path`, `events_max_bytes`, `profiles`) | `$XDG_CONFIG_HOME/dgc/config.toml` |
| Managed worktrees | `$XDG_DATA_HOME/dgc/worktrees` (`~/.local/share/dgc/worktrees`) |

Each agent's events are in `agents/<id>/events.jsonl` under the state
directory, rotated at `events_max_bytes` (default 32 MiB). The socket path must
stay under 100 bytes (`sockaddr_un` limit), so keep `DGC_STATE_ROOT` short.

| Variable | Effect |
|---|---|
| `DGC_STATE_ROOT` | put socket, state, config and worktrees under one directory |
| `DGC_LOG` | daemon log level: `error`, `warn`, `info` (default), `debug`, `trace` |

## Security properties and limits

- The daemon listens only on a Unix socket (0600, in a 0700 directory) and
  checks the peer's UID on every connection; other users are refused. It opens
  no TCP or UDP port. All state files are 0600 and directories 0700.
- The trust boundary is your UID: any process running as you can drive agents,
  including the agents themselves (they can answer permission requests through
  the socket). Agents started by dgc are trusted to the extent their provider
  mode allows.
- dgc never reads provider credential files, and the daemon log never contains
  prompt or agent text. Provider children inherit the daemon's environment
  (including any API keys in it). Prompts and agent output are stored as-is in
  the agent's 0600 state files (`events.jsonl`, `queue.json` for queued
  messages, and the agent record, whose title defaults to the prompt's first
  line), so they can contain secrets the agent printed.
- The permission dialog and `permit ls` show dgc's summary of a request, not
  everything the agent will do (for example not the content of a file it
  writes), and a summary longer than 4 KiB is cut.
- Outside the listed auto-approve modes dgc adds no approvals of its own, but
  the provider's default mode and your provider settings still decide what
  runs without asking: Codex's default `workspace-write` runs sandboxed
  commands in the workspace, and without `--mode` Claude applies your own
  Claude settings (allow rules, default mode). `--mode untrusted` (Codex) and
  `--mode manual` (Claude) are the modes that ask; requests then wait for
  `permit allow|deny` (the provider's own built-in safe-command and allow-rule
  exceptions still apply).
- dgc deletes a worktree only when it created it, inside its managed
  directory, and (without `--force`) only when no work would be lost; see
  [Projects and workspaces](#projects-and-workspaces).

## Running as a service

Optional: `status`, `run` and the other commands start the daemon on demand.
Examples only; dgc installs neither file. Use an absolute
`ExecStart`/`ProgramArguments` path and make sure `claude` / `codex` are on the
service's `PATH` (or set `claude_path` / `codex_path` in the config). The
service's environment is the one the providers get.

```ini
# ~/.config/systemd/user/dgc.service  (systemctl --user enable --now dgc)
[Unit]
Description=dgc orchestrator daemon

[Service]
ExecStart=%h/.cargo/bin/dgc daemon run
Environment=PATH=%h/.local/bin:/usr/local/bin:/usr/bin
Restart=on-failure

[Install]
WantedBy=default.target
```

```xml
<!-- ~/Library/LaunchAgents/dev.dgc.daemon.plist  (launchctl load -w <file>) -->
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>dev.dgc.daemon</string>
  <key>ProgramArguments</key>
  <array><string>/Users/you/.cargo/bin/dgc</string><string>daemon</string><string>run</string></array>
  <key>EnvironmentVariables</key>
  <dict><key>PATH</key><string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict></plist>
```

## Development

Start with [AGENTS.md](AGENTS.md); the crate layout is in
[architecture](docs/ai/architecture.md) and the required checks in the
[verification matrix](docs/ai/workflow.md). Development scripts need
Python 3.11+.

```bash
cargo fmt --all
bash scripts/verify.sh test tui::      # focused tests; a filter matching zero tests fails
bash scripts/verify.sh rust            # fmt check, warning-free Clippy, all tests (locked)
bash scripts/verify.sh guidance        # docs links, CI/wrapper agreement, script tests
bash scripts/verify.sh msrv            # cargo check with the MSRV toolchain (1.94)
bash scripts/verify.sh tui-deps        # the active Crossterm generation
bash scripts/verify.sh macos           # platform-dependent tests, macOS only
cargo build --locked --release -p dgc-cli
```

The checks expect the pinned toolchain (1.99.0, `rust-toolchain.toml`); newer
Clippy versions can report different warnings. If `cargo --version` in the
repository shows another version (for example because a non-rustup `cargo` comes
first on `PATH`), put the pinned toolchain first:
`PATH=$HOME/.rustup/toolchains/1.99.0-<host-triple>/bin:$PATH`.

Tests never run the real providers: they use the `dgc-fake-provider` binary
replaying recorded or hand-written scenarios
([fixtures](crates/dgc-daemon/tests/fixtures/README.md)).

## Roadmap

- **M4:** launch profiles and a suggested handoff to the other provider when
  an agent hits a usage limit.
- **M5:** schedules (start agents on a cron schedule), heartbeats (periodic
  follow-ups to an agent), and supervised workspace scripts.
- **M6:** an MCP server for agents, so an agent can start and drive sub-agents
  through dgc.
- **Phase B:** remote use (another machine or a phone) over the same socket
  protocol, under a separate design. Until then dgc stays local only.

## License

MIT.
