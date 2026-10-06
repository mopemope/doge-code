# Doge-Code

Doge-Code is an interactive AI coding agent that provides advanced code analysis, editing, and project management capabilities through both a terminal UI and MCP (Model Context Protocol) server. Built with Rust, its modern architecture combines tree-sitter parsing, LLM integration, and persistent sessions to deliver a powerful coding assistant experience.

## 🚀 Key Features

### Core Capabilities
- **Intelligent Code Analysis**: tree-sitter based code parsing and symbol extraction (10+ languages including Rust, JavaScript/TypeScript, Python, Go, Java, C/C++, C#, Markdown)
- **Interactive Terminal UI**: Full-featured TUI with syntax highlighting, diff review, and real-time LLM interaction
- **MCP Server**: Model Context Protocol server for integration with MCP-enabled clients like Claude Desktop
- **Persistent Sessions**: SQLite-based session storage to maintain context across runs. Manage them via `dgc session` (list/show/delete) or `--resume [SESSION_ID]` to continue where you left off
- **Multi-Mode Interaction**: Support for both interactive TUI mode and command-line execution

### Supported Languages
- **Rust** (tree-sitter-rust 0.24.0)
- **JavaScript/TypeScript** (tree-sitter-javascript/typescript 0.25.0/0.23.2)
- **Python** (tree-sitter-python 0.25.0)
- **Go** (tree-sitter-go 0.25.0)
- **Java** (tree-sitter-java 0.23.5)
- **C/C++** (tree-sitter-c/c++ 0.24.1/0.23.4)
- **C#** (tree-sitter-c-sharp 0.23.1)
- **Markdown** (tree-sitter-md 0.5.1)

### Execution Modes

#### 1. Interactive TUI Mode (Default)
```bash
cargo run --release
# or
dgc
```
Launches a full terminal interface providing code exploration/navigation, real-time LLM chat interface, diff review/approval workflow, session management, and project overview/symbol browsing.

#### 2. Command Execution Mode
```bash
dgc exec "Add error handling to database connection function"
```
Executes a single instruction and exits with results.

Successful `exec` runs exit with code 0; failed or interrupted runs exit nonzero.
With `--json`, stdout still contains `success:false` on an agent failure.
SIGINT (Ctrl-C) and SIGTERM cancel the active turn and wait for managed process
cleanup and conversation checkpointing before exit. Completed tool calls and
results survive API-key and subscription failures and `--resume`; interrupted
calls explicitly report an unknown outcome. Inspect the workspace before retrying
such a call. This is recovery evidence, not automatic rollback or replay.
Once a finite local mutation has started, cooperative cancellation waits for its commit and
tracking to finish. Cancellation can therefore wait for filesystem I/O.

Explicitly incomplete completions (`length`, `content_filter`), refusals, and
invalid tool-call batches fail before tools execute. Compatible endpoints that
omit `finish_reason` remain supported; absence is not proof of provider completion.
Batch preflight checks assistant role, unique call IDs, advertised tool names and
typed built-in mutation/process arguments. It is not a transaction that rolls
back earlier valid operations after a later runtime failure, nor a full validator
for arbitrary remote MCP schemas.
The complete project/system authority prompt is retained during compaction, even
when retaining it prevents further context reduction.

#### 3. MCP Server Mode
```bash
dgc mcp-server 127.0.0.1:8000
```
Starts MCP server for integration with Claude Desktop and other clients.

#### 4. File Watch Mode
```bash
dgc watch
```

Only one operation per file runs at a time, including its debounce delay. A model
response is committed only if the original file snapshot still matches; manual
edits made while inference is pending are preserved and a conflict is reported.
Processing-time events are coalesced, so save again after the reported conflict
and rate-limit interval to request a fresh edit. Watch requests a version-1 JSON
edit envelope: `{"version":1,"edits":[{"search":"exact original text","replace":"new text"}]}`.
Each search must match once in the original snapshot; overlapping edits, unknown
fields, unsupported versions, Markdown/prose and malformed responses are rejected.
All ranges resolve against the original and apply from the end, preserving Unicode,
CRLF and final newlines. Empty edits are a no-op. The limits are 64 edits and 8 MiB
for source, response and every intermediate candidate. Watch no longer accepts raw
whole-file/code-block responses. The protocol does not guarantee the semantic
correctness of model edits: keep reviewing Git diffs and running relevant tests.
Successful watch edits use the
normal persistent change tracking and in-process undo bookkeeping, and enabled
backups have unique names. Backup retention only removes regular files whose
complete original basename and generated timestamp/UUIDv7 suffix match the target.
Unknown names, symlinks, directories and backups of other files are retained;
`backup_keep = 0` keeps all backups. Watch has no undo command; its undo stack is not
restored by a separate exec/TUI/resumed process. Restore a watch backup manually
after inspecting the current file.
The snapshot check narrows the external-write race window; it is not an OS-level
atomic compare-and-swap or a sandbox. Watch remains a mutating mode.
Monitors file changes and automatically triggers LLM assistant.

#### 5. Code Rewrite Mode
```bash
dgc rewrite --prompt "Convert to async/await" --code-file /tmp/code.rs
```
Rewrites specific code snippets with LLM assistant.

#### 6. Session Management Mode
```bash
dgc session list
dgc session show <id>
dgc session delete <id>
```
Non-interactive session management. Sessions are listed most recently updated first, and IDs may be given as prefixes (e.g. `0198abcd`).

## 🔧 Installation

### Prerequisites
- Rust 1.94+ (Rust Edition 2024); development and CI use Rust 1.99.0
- An OpenAI-compatible API key (`OPENAI_API_KEY`), or an eligible ChatGPT account with plan usage authorized through Sign in with ChatGPT
- ripgrep (`rg` on `PATH`; required at runtime by the `search_text` tool, which shells out to `rg` with no fallback)

### Build from Source
```bash
git clone https://github.com/mopemope/doge-code.git
cd doge-code
cargo build --release
```

### Configuration
Create `.doge/config.toml` in your project directory:
```toml
model = "gpt-4o-mini"
base_url = "https://api.openai.com/v1"

# Top-level key (not under [llm])
project_instructions_file = "PROJECT.md"
```

## Use your ChatGPT plan (preview)

On macOS and Linux, sign in through the official Sign in with ChatGPT flow to
use an eligible account's plan allowance with dgc. Account/workspace eligibility,
available models, app limits and service policy are enforced by OpenAI.

```bash
dgc auth login openai-chatgpt
# Earlier consent declined: explicitly request plan usage
dgc auth login openai-chatgpt --account <account-label> --enable-plan-usage
dgc auth status openai-chatgpt
dgc models --provider openai-chatgpt
# Choose a slug from the model catalog:
dgc --provider openai-chatgpt --model <catalog-slug> exec "Explain this project"
dgc --provider openai-chatgpt --model <catalog-slug>
```

The explicit login command opens the system browser and listens on a temporary
`127.0.0.1` callback port. Use `--no-browser` to open the displayed authorization
URL manually. Do not share that URL. A successful identity sign-in without plan
usage permission is shown separately and cannot make inference requests.
Ordinary exec, watch and workflow commands never open a sign-in browser.

Login does not change your existing provider configuration. The default remains
`openai-compatible` and uses your API key. Select `openai-chatgpt` with `--provider`,
`DGC_PROVIDER`, or a top-level `provider = "openai-chatgpt"` configuration key
(CLI > environment > project > user > default). Always choose a model explicitly;
the API-key default model is not reused for this provider. `--api-key` cannot be
combined with the ChatGPT provider, and its requests always use the official
Responses endpoint even if another base URL is configured.

```bash
dgc auth list openai-chatgpt
dgc auth login openai-chatgpt --new-account
dgc auth login openai-chatgpt --account <account-label>
dgc auth use openai-chatgpt <account-label>
dgc auth logout openai-chatgpt [account-label]
```

Start a new session when changing the ChatGPT account or model. Responses
sessions retain ordered output and encrypted reasoning items locally so tool
calls and their results can be replayed after `--resume`. A saved provider/account
binding also survives history compaction. These sessions cannot be continued
through another billing provider. Existing API-key sessions remain readable.
For catalog models whose context capacity dgc does not recognize, set
`context_window_size` in the `[llm]` section to the documented model capacity.
Local context estimates are approximate; encrypted reasoning is preserved as
opaque state and its ciphertext length is not used as a token count.
ChatGPT Responses uses server-side native compaction: every `POST /responses`
request carries `context_management: [{type: "compaction", compact_threshold}]`
with `store:false` / `stream:true` preserved. `compact_threshold` reuses dgc's
effective auto-compaction limit (`min(auto_compact_prompt_token_threshold`,
`context_window_size * 0.8)`); values below the Responses minimum of 1000 are
rejected at startup without silent clamping. Compaction items are opaque and
persisted unchanged, and the pre-compaction transcript is removed from the
canonical replay state while the leading authority system prompt is retained.
`/compact` does not invoke the local text summarizer for this provider.
Incorrect or unknown model context capacity can still cause context overflow;
configure `[llm] context_window_size` when needed.

Credentials are stored in the user's platform configuration directory under
`doge-code/openai-chatgpt/`, separately from project `.doge/` state. Unix directories
use 0700 and files use 0600, with atomic writes and process-wide file locking.
Local status reports expiry and granted permission; it does not prove current
server eligibility. Refreshes replace rotating tokens together. An interrupted
or uncertain renewal requires sign-in again instead of replaying the old token.
Logout clears local tokens; a warning indicates when remote revocation could not
be confirmed, in which case disconnect the app in ChatGPT Settings.

Plan limits stop new inference and never silently switch to API-key billing.
Review limits and app access in [ChatGPT settings](https://chatgpt.com/#settings).
Reported token counts are request/session measurements, not remaining plan
allowance or a predicted reset time. Transient admission failures have bounded
retries; failed or incomplete streams never authorize tool execution.

This preview supports text and dgc's local function tools (including local MCP
adapters and local tool discovery). It uses HTTP/SSE with `store:false` and
`stream:true`. Hosted Responses tool search, hosted MCP, image generation,
WebSocket continuation and device-code sign-in are outside this integration.
Protected credential storage for Windows is not implemented yet.

See the official [OSS integration](https://developers.openai.com/siwc/token-sharing-open-source)
and [preview limitations](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations).

## Export evidence for review

After a successful or partial CLI `exec` run, stderr shows the saved session ID
and its evidence command. JSON output adds `review_handoff` containing the exact
session ID, project root and command arguments. No report is generated automatically.
In the TUI, `/evidence [session-id]` compares saved evidence with current selected
files and shows a bounded summary plus the export command in the log. Without an
ID it selects the current session at command invocation; explicit prefixes use
the existing unambiguous resolver and the summary displays the full resolved ID.
Diff Review's `e` key inspects its captured session, even if another session is
now current. Legacy reviews without an ID require explicit selection. The key
works with empty input and does not accept, reject or dismiss the diff.

Evidence inspection is a foreground read job: `/cancel` cancels it; session
transitions and conflicting operations wait for completion. Cancelling, accepting,
rejecting or dismissing a diff never triggers an export. The summary marks
incomplete/unavailable collection, unknown correspondence and unattributed files;
counts are unique observation IDs per category and may overlap across changes.
Use the full per-change report for diagnostics and exact links. A summary is an
export-time observation and can become stale after later edits. Inspect again
before review. Run the shown export command from the displayed project root;
choose an output file explicitly using shell redirection when needed.


The report starts with a per-change review handoff: explicit requirement/plan
links and linked command observation IDs. Matching successful observations require
an active change matching its recorded file, stable execution endpoints and
current selected files matching the execution start. Historical or unknown
successes appear separately; all linked failures remain visible even after later
successes. Empty evidence does not establish success or approval. JSON schema v2
adds `review_handoff`; existing fields and persisted records are unchanged.


Export a saved session's requirements, changes and recorded command outcomes
without an API key or an LLM call:

```bash
dgc session evidence <SESSION_ID>
dgc session evidence <SESSION_ID> --format json
dgc session evidence <SESSION_ID> --base main
dgc session evidence <SESSION_ID> --include-content
dgc session evidence <SESSION_ID> > /tmp/dgc-evidence.md
```

IDs accept a unique prefix. The default format is Markdown; JSON is a single
schema-versioned document. The command uses the current directory as the project
root and reads `.doge/sessions` there. It bypasses `.env`, project/user config,
normal file logging, RepoMap, and MCP startup. Output goes only to stdout;
diagnostics go to stderr. It does not change source files, the Git index, saved
sessions, or provenance. On Unix, file reads reject symlink traversal through
parent directories. Bounded saved evidence and selected text files are copied
into private temporary directories for consistent collection; these copies are
removed when export finishes. Export fails closed on platforms without these
safe read primitives. Save reports outside the repository: shell redirection
inside the project may introduce an untracked file before comparison starts.

`--base` compares the exact locally resolved commit with the current working
tree, including committed changes since that base and current staged/unstaged
results. It does **not** automatically choose a merge-base or reproduce a PR's
three-dot comparison. Without `--base`, comparison uses HEAD. Staged and unstaged
flags are also retained when they cancel out in the final file. Untracked files
are included; ignored files, `.git/`, `.doge/` runtime files, and paths outside the
current project are excluded from Git comparison. Explicit session file records
are still included in the manifest. Renames appear as deletion/addition.
Submodules and unsupported file/path formats are marked incomplete. Git capture
is bounded; incomplete capture is never used as a complete file list. An explicit
base comparison that cannot be performed fails; without a base, a report can
retain session evidence with an unavailable-comparison warning.

The report preserves historical attribution and distinguishes pending, failing,
stale, diverged, reverted, superseded and observed-passing evidence. A requirement
with a successful observation may still have pending obligations. Session linkage
on a file does not attribute all its hunks or external edits to dgc. The manifest
identifies selected files **at export time**, using exact-byte BLAKE3 hashes when
available. A before/after check retries once if saved evidence or workspace files
change, then fails if they keep changing. This is an optimistic check, not an
atomic snapshot, execution-time environment record, signature, or proof of
correctness. Test counts and historical execution environments are `null` because
existing events do not record them. No tests are re-executed by export.

New verification observations from structured `execute_process`, `/test`, and
classified `/lint` commands record project workspace endpoints before and after
execution. Report schema version 2 adds `execution_workspace` and
`current_code_state` to each command observation. The command outcome and
historical requirement/obligation coverage remain independent of these fields.
`stable_endpoints` means the bounded nonempty set was fully observed and matched
at both endpoints; `changed_between_endpoints` identifies observed changes;
`indeterminate` means correspondence could not be confirmed. Current comparison
is `matches_start`, `differs_from_start`, or `indeterminate`. Legacy v1–v4 events
show `not_recorded`, without reconstructing historical files from current ones.

Execution endpoints cover Git-tracked and nonignored untracked files inside the
project, plus validated provenance references (including referenced ignored
files). `.git/` and `.doge/` are always excluded. New files and deletions are
compared; binary regular files use exact-byte hashes and Unix execute bits are
recorded. Symlinks are never followed, including parent directory components;
submodules, special files, unsafe paths and unreadable files cannot establish a
match. Without Git, referenced files can be recorded but collection is partial.
No file bodies, environment variables, toolchain probes or remote URLs are added.

Managed verification now saves minimal execution context in provenance v6:
OS family, architecture and the primary tool's numeric executable version when
safely obtainable. This is separate from the command outcome and workspace
endpoints. Version probes cover native system `cargo`, `python3` and `node` only;
Go, wrappers (`npm`, `pytest`), home toolchains, unsupported platforms and rejected
or failed probes retain an explicit unknown reason. The selected executable must
be canonical under `/usr/bin`, root-owned and not group/world-writable, including
its ancestors, and outside the project. Linux ELF executables are supported;
other native formats remain unknown. Versions identify the selected executable,
not dependencies, test counts or a complete reproducible environment.

Probes clear inherited environment, use an empty temporary directory and fixed
arguments, retain at most 1 KiB per output stream, and have a 500 ms execution
limit (or a lower configured LLM limit), plus managed cleanup. LLM original and
probe requests must both pass existing policy: basename permission alone does
not authorize an absolute executable or the temporary directory. No policy is
expanded to obtain metadata. Trusted `/test` and `/lint` retain their separate
execution boundary. Raw probe output, errors, paths, host/user names and
environment values are never saved in execution context. Probe failure does not
change the verification outcome. Old v1–5 events stay unchanged with context
unknown; export reads saved context without probing the current machine.
Matching context never establishes correctness or reproducibility.

Explicit `execute_process` requests for `go test -json` now record bounded
structured results in provenance v7. The [Go event format](https://pkg.go.dev/cmd/test2json)
is parsed from existing stdout capture before the LLM presentation budget. This
adds no commands, result-file reads or automatic `-json` flags. Ordinary `/test`
Go commands still use their existing arguments and have no structured result.
The same verification writer can interpret eligible complete trusted output.
Other output formats and v1–6 history remain unknown, without reparsing excerpts
on export. Original process success/exit status and change/obligation links remain
separate from reported test results and human approval.

Complete results count passed/failed/skipped terminal test events (parent tests,
subtests and examples separately) and package outcomes. JSON `test_count` is this
terminal-event total, accompanied by `test_count_unit`; it is not a unique-test,
assertion or coverage count. Known zero requires a complete package event stream.
Markdown shows historical results beside execution/current-code comparisons;
later edits preserve old counts while making their workspace correspondence stale.
New summaries contain numeric counts and fixed status/reason only, without
package/test names, output, paths or parser error text. Existing content opt-in
and provenance command excerpts retain their established behavior.

Input is limited to 64 KiB, 16 KiB per line, 4,096 events, 128 packages, 512 test
keys and 256 bytes per key. Malformed, duplicate/unclosed, unsupported, truncated,
excessive or timed-out output yields unknown counts rather than zero. Package
terminal outcomes inconsistent with process success also yield unknown. Benchmark,
fuzz, list, wrapper, repeated-test/CPU modes and unknown flags/events are not
supported by this first adapter. The invocation must explicitly enable `-json`
before `-args`/`--`; an effective `-json=false` disables collection. Reports are
observations of the emitted format and do not authenticate that tests ran, prove
correctness or record a human review decision.

Each endpoint is limited to 10,000 paths, 16 MiB per file, 128 MiB of reads and

4 MiB serialized data, with a cooperative 10-second collection budget. Acquisition
failure does not change command success or failure. `/test`, `/lint` and the
process tool warn about changed or indeterminate endpoints; `provenance_read`
returns compact endpoint/current comparison summaries within its existing budget.
Export compares execution records with current project state without rerunning
commands. A successful command followed by a changed test or lockfile remains a
successful historical observation and displays changed code correspondence.
Endpoint equality does not guarantee unchanged inputs throughout execution:
changes restored between observations may be missed. These observations do not
capture all ignored dependencies, execution environments, or reproducibility.

Normal output includes requirement/plan descriptions and recorded program/argv.
`--include-content` additionally includes raw/effective directives, recorded diffs
and saved stdout/stderr excerpts (which may already be truncated). Neither mode
exports the full conversation, environment variables, API configuration, remote
URLs or Observation Store. Outside-project command cwd is replaced by
`outside_project`. Review descriptions and argv before sharing; values entered
there are not guaranteed to be free of secrets. Evidence and command text is
rendered as literal text rather than active Markdown/HTML.

A generated report exits successfully even when recorded tests failed or evidence
is missing. Invalid input, unreadable sessions, persistent concurrent changes and
explicit-base failures exit nonzero before report output. Output I/O failures
also exit nonzero; a destination may already have received some bytes.
Limits are 10,000 entries per inventory, 16 MiB per input file, 128 MiB per input
scan and 32 MiB per rendered report; exceeding a limit fails instead of silently
truncating the artifact. The shared managed runner's capture limits also apply
to Git commands, with a 10-second timeout per command.

The regular Go `/test` workflow now also runs `go test -json ./...`. Numeric
observations are parsed from the bounded runner capture before diagnostic display
is shortened. The TUI and failed-test follow-up show decoded `Output` text rather
than JSON envelopes. Failed test names, file/line and messages are matched by
package/test even when diagnostics arrive before `fail` or tests run in parallel.
Raw JSON capture remains available under the existing evidence content opt-in;
only counts/status are added to the numeric result. A truncated/warned/timed-out
capture retains unknown counts, and a cancelled run is not recorded. Compiler or
package-only failures still start the existing failure follow-up even with no
named failed tests. Legacy verbose Go diagnostics and other language commands
retain their existing behavior. This does not create a local review judgment.

### Explicit local review judgments

After inspecting `dgc session evidence <FULL_SESSION_ID>`, its JSON `review_target`
and Markdown show a `review-v1:<digest>` snapshot token when selected workspace
files and the Git comparison can be read completely and recorded changes exist.
Close the owning CLI/TUI session before recording, then inspect fresh evidence.
From the same project directory, run:

```bash
dgc session review <FULL_SESSION_ID> accept --snapshot review-v1:<digest>
dgc session review <FULL_SESSION_ID> request-changes --snapshot review-v1:<digest>
```

The command requires a TTY and asks you to type the chosen kind exactly. Any other
answer, EOF, or interruption before confirmation cancels without a judgment.
There is no `--yes` or noninteractive bypass and no agent-facing review tool.
`accept` means the operator explicitly entered a local acceptance judgment about
this inspected snapshot; `request-changes` records a local request for changes.
Neither proves the operator's identity or humanity, correctness, requirement
satisfaction, test success, GitHub approval or merge authority. No external
approval is posted and no files are reverted. Diff Review's existing `a` closes
the diff and `r` rolls back captured changes; neither records a judgment.

The target covers all recorded session changes and command observations, including
failures and legacy unknowns, saved requirement/plan/obligation data, selected
working-tree files and displayed Git comparison state. It does not approve the
index contents, ignored files, submodule contents or an atomic repository snapshot.
Explicit `--base` exports do not issue a recording token. Metadata/diff display
options and export time do not change the token; judgment history itself is excluded.
Changes to target inputs make old judgments `stale`; unavailable comparison or
corrupt/unsupported judgment history is `unknown`. Historical judgments remain.
Existing sessions with no decision are `not_recorded`, never implicitly accepted.

Decisions use a separate append-only version-one store at
`.doge/sessions/<id>/human-review/v1/decisions/`. Only IDs, hashes, kind, time and
explicit supersession are recorded; no reviewer identity or free-text comments.
Re-entering the same latest kind for the same snapshot is a no-op. Choosing a
different kind appends a record superseding the previous one. Session ownership
serializes writers; busy/unsafe/changed storage fails before publication. Files
are published without overwrite through held directories on Unix and synced.
A sync failure after publication is reported as durability unconfirmed, so inspect
evidence before retrying. An external edit between comparison and publication
may still occur; each later export rechecks correspondence. Confirmation starts
a short publication step; interruption after confirmation may leave a record.

## Diagnostic Logging

Diagnostic logs live at `.doge/logs/debug.log` with a `0700` log directory and
`0600` log file on Unix. Existing `.doge` permissions are retained.
Below the selected project root, Unix opens `.doge`,
`logs`, and `debug.log` without following symlinks; permissions and startup
truncation use held handles. Existing hardlinked or nonregular log files are
refused. The selected root and its ancestors are trusted, so root aliases remain
supported. Held directories may still be renamed, and concurrent changes to the
opened inode (including new hardlinks) are outside this guarantee. Other
platforms use symlink preflight checks without the Unix race guarantees.
Unix needs permission to open the root and child directories for reading;
if a directory cannot be opened, startup fails without repairing it through a
path. Restore access to that directory before retrying.
Normal startup still truncates the previous log; this does not add rotation.
Even with
`RUST_LOG=debug`, prompts, source/file content, tool arguments/results,
model output, raw request/response bodies, provider error bodies, refusal
bodies, encrypted Responses state, and credentials are never logged. Logs
record shape and counts instead (message counts, request/response bytes,
tool counts, token usage, retry metadata, provider codes, request IDs,
durations). JSON parsing errors in Chat Completions responses, tool-call argument
validation, and Responses usage report only their category and line/column
position in logs and propagated errors; provider values and raw parser messages
are omitted. Older versions may have written such content to a repository-root
`debug.log`; that legacy file is ignored but never auto-deleted, so inspect
and remove it yourself if present. To review content explicitly, use:

```bash
dgc session evidence <SESSION_ID> --include-content
```

## 🛠️ Tools and Commands

### File System Tools
- `fs_read`: Read files with optional summary mode for large files
- `fs_write`: Create or overwrite files
- `fs_list`: List directory contents with pagination
- `find_file`: Search files by glob or substring, with complete paths and bounded cursor pages (`cursor`, `page_size`, `response_budget_chars`)
- `execute_process`: Run a program directly without a shell (preferred for builds, tests, git)
- `execute_bash`: Shell escape hatch — use only when pipes/redirects/builtins are genuinely required (disable with `[execution] allow_shell = false`)

All finite LLM-facing process tools use the same managed process lifecycle: bounded streaming capture, timeout/cancellation, Unix process-group cleanup, and direct-child reaping.

Persistent `execute_shell` writes stdin while reading stdout and stderr. Its
`command_timeout_ms` covers write, flush and output collection together; `0`
disables the deadline while cancellation still works. Timeout, cancellation,
pipe failures and premature output EOF terminate the shell tree and reset the
session. The next call starts a fresh shell; successful commands retain cwd and
environment. Captured output preserves line endings within the output budget.


### Code Analysis Tools
- `search_repomap`: Search parsed code symbols with advanced filtering
- `search_text`: Text-based search across files
- `fs_read_many_files`: Batch file reading with budget management

`search_text` resolves the literal directory prefix of `file_glob` and checks
project scope and explicit `allowed_paths` before starting ripgrep. Parent
traversal and directory symlinks cannot select an unauthorized search root.
Exact filenames can use individually allowed files without granting access to
neighboring files. Wildcards after a parent traversal must have an authorized
literal prefix; parent components after a wildcard are rejected.
Searches preserve nested globs, project-root aliases, pagination and output
budgets. Pages account for JSON escaping and metadata; `next_offset` advances
only past the matches actually returned. Omitting a glob searches only the
project; ordinary absolute globs outside it retain their empty-result behavior.
The tool ignores ambient
`RIPGREP_CONFIG_PATH` settings and does not follow symlinks while traversing.
Ripgrep exit code 1 is a normal empty search; regex errors, other failing exit
codes, malformed/incomplete JSON records and signals return bounded errors.
Stdout is read as complete JSON records within a 1 MiB total byte cap including
newlines. If the cap arrives before any requested match, narrow the pattern or
glob; no resumable offset is claimed. When complete matches were collected,
intentional result/byte limits retain partial results and pagination metadata.
Stderr is drained concurrently with a bounded diagnostic prefix; each search
owns and reaps its process, including errors and intentional early stops.
CLI/TUI/sub-agent and MCP request routes move file reads and glob expansion to
bounded blocking workers. Cancellation is checked between read chunks, glob
entries and files; normal cancellation waits for the worker to finish before
returning, and canceled reads do not update active context. Search cancellation
also stops quiet ripgrep processes, reaps the direct child and cleans up the
owned Unix process group. MCP clients must send a request cancellation
notification. The synchronous Rust APIs remain available. A filesystem call
already blocked in the kernel cannot be interrupted immediately; a dropped
read future requests cooperative stop without publishing a late result.

`fs_read` and `fs_read_many_files` scan input in fixed-size UTF-8 chunks and
retain only budgeted page/snippet candidates. Large lines and small output
requests do not require keeping the entire file or an index of every line.
Both still scan through EOF to report exact line counts and reject invalid
UTF-8 anywhere in the file; output budgets do not limit total input I/O.
`fs_read` and multi-file summary normalize CRLF to LF between returned lines;
multi-file full mode preserves the original separators and final newline.

### Editing Tools
- `apply_patch`: Apply a unified diff patch to a file (single `file_path`; read the file first)
- `edit`: Replace specific code blocks
- `edit_symbol`: TUI slash command (`/edit-symbol`) to edit entire symbols (functions, structs, etc.)

### Session Management
- `plan_write`/`plan_read`: Save and read task/execution plans (tied to sessions). `plan_write` returns compact update metadata; `plan_read` retrieves full state
- `session`: Automatic session persistence and resume
- `dgc session list|show|delete`: CLI session management (ID prefixes supported)
- `--resume` / `--resume=<SESSION_ID>`: Resume the latest or a specific session (TUI and `exec`)

### Memory Tools
- `read_memory`: Read content from persistent memory (markdown files)
- `write_memory`: Write content to persistent memory
- `list_memories`: List all available memory keys
- `search_memory`: Search across memory files

### Advanced Tools
- `undo`: Safe LIFO rollback of the last tracked mutation (fails closed on conflict, deletes created files, no redo yet)
- `execute_shell`: Persistent shell session for stateful command execution (escape hatch for persistent cwd/env/builtins; disable with `[execution] allow_shell = false`)
- `doc_generate`: Generate documentation for a symbol or file via LLM
- `run_workflow`: Run a predefined workflow from `.doge/workflows/`
- `task`: Delegate focused research to an isolated read-only sub-agent with iteration, tool-call, elapsed-time, cumulative-token, and request-context budgets. Budget exhaustion returns `ok=true`, `status="partial"`, a stable `stop_reason`, and bounded evidence/files; cancellation and research provider failures remain errors.
- `tool_search`: Discover and activate deferred built-in/MCP tools on demand (see Tool Search below)
- `provenance_read`: Read plan/change/verification provenance (which plan step was active, what changed, which checks observed it, where evidence is incomplete)
- `requirements_write`/`requirements_read`: Structure explicit user requirements from the observed directive and read them with plan/change/verification coverage
- `observation_read`: Retrieve an offloaded historical tool result (`obs-*`) without re-running the original tool; byte pages may be shortened to fit their serialized JSON envelope, and `next_cursor` identifies the first unread byte.

### Read-only sub-agent budgets

`[subagent]` merges field by field: runtime defaults, user configuration, then
project configuration. Explicit zero values are startup errors. Example policy:

```toml
[subagent]
max_iterations = 40
max_tool_calls = 64
max_elapsed_ms = 180000
# Optional: omit to derive a finite budget from the selected model's effective
# compaction limit, including configured context windows and model overrides.
# max_total_tokens = 100000
```

These example values match the current defaults. `task` inputs remain
`description` and `prompt`; the model cannot raise these limits. The request
context ceiling also uses the model's effective compaction limit and applies
even with `[context_budget] mode = "off"`. Each transport attempt, including
failed requests and internal retries, reserves its estimated prompt tokens before
send. Reported usage raises that attempt's charge to
`max(reported total tokens, estimated prompt tokens)`; missing usage retains the
estimate without inventing provider usage or billing amounts. Retry attempts
pass the remaining-token and elapsed-time gates. Charges belong to the worker,
so another worker's reported usage cannot consume its local budget. Parent
provider totals still include only reported usage. Cached tokens count in full.
A response may exceed the remaining budget; no further attempt starts once
exhausted. Research iterations count logical requests, not internal retries.
Cancellation before send reserves nothing; an attempt entered before cancellation
keeps its estimate because provider processing may already have started.

Elapsed time uses a monotonic clock at safe boundaries before requests and tool
dispatch. An operation already running is allowed to finish, so this is not a
hard wall-clock timeout. Over-budget tool batches are skipped in full. At most
one tools-free finalization request runs if context, remaining tokens, elapsed
time, and cancellation permit it; otherwise bounded local evidence provides the
partial summary. Finalization is charged but does not increment research
iterations. Reports contain at most 32 paths and a 4,000-character summary,
with `files_examined_truncated` indicating omitted or shortened paths. The entire
serialized `task` output stays within 6,000 characters, accounting for JSON
escaping; paths may be omitted and the summary shortened further to fit.

### Main-agent run budgets

`[agent_budget]` merges field by field: defaults, user configuration, then
project configuration. Explicit zero values are startup errors.

```toml
[agent_budget]
max_iterations = 256
# Optional run-wide limits:
# max_tool_calls = 256
# max_elapsed_ms = 900000
# max_total_tokens = 1000000
```

`max_iterations` defaults to 256 (the previous hard limit). The other three
limits are unset by default and only enforce when configured. Budget stops are
`partial` results, distinct from cancellation/errors. Token accounting uses
provider total usage plus bounded estimates, never double-counts cached or
reasoning subtotals, and includes `task` subagent usage via the shared client.
One in-flight request may finish before the next-operation stop applies, so
this is not a hard billing limit. Cached tokens are never subtracted.

## Directive-to-Evidence Traceability

Doge-Code connects observed work as:

```text
User Directive
      ↓
Requirement
      ↓
Plan
      ↓
Mutation
      ↓
Verification
```

```text
D1 "Add cache, keep token validation"
 ├─ R1 "cache authentication" (derived from D1)
 │   └─ P1 "Optimize TokenCache lookup" -> [R1]
 │       ├─ change C8 src/auth.rs
 │       └─ verification V3 cargo test [passed]
 └─ R2 "preserve token validation" (derived from D1)
```

Terminology (none of these imply formal correctness proof):

```text
Directive:
  observed instruction (raw user input + effective instruction handed to the agent)

Requirement:
  structured agent interpretation derived from a directive

Verification:
  observed command result against a workspace snapshot
```

- A `DirectiveObserved` event stores `raw_input` (what the user typed) and `effective_instruction` (what the agent received) with BLAKE3 hashes. The envelope `event_id` is the canonical directive id. Directive text is never logged; only ids, origin, and hashes appear in logs. `provenance_read` returns a preview + hashes by default and full text only with `include_content=true`.
- A `RequirementChanged` event batches `before -> after` transitions for one directive. Statuses are only `Active` / `Withdrawn` — there is intentionally no `Satisfied`/`Verified` (a passing test never proves a requirement). Current state is rebuilt from history; there is no separate `requirements.json`.
- `PlanItem.requirement_ids` links steps to requirements (unknown ids fail `plan_write`; withdrawn links warn but do not break). Requirement-only link edits still emit `PlanChanged`.
- `ChangeCommitted` freezes `directive_id` / `plan_item_id` / `requirement_ids` at commit time; later plan remaps never rewrite history. Unplanned mutations still carry the turn directive when one exists.
- `VerificationObserved` freezes `directive_id` / `requirement_ids` (union of active change ids, falling back to current plan links) at capture time; later changes never leak into a running verification. It also freezes `matched_obligations` (id + binding hash) for structured runs.
- Requirement coverage (`requirements_read`) reports `no_linked_work` / `planned_no_active_change` / `active_unverified` / `observed_passing` (at least one successful observation of an active change — not a correctness proof) / `diverged` / `reverted` / `mixed`, plus compact `verification_obligations` per requirement (id, plan_item_id, kind, state).
- Storage: `.doge/sessions/<id>/provenance/v7/events/<uuid>.json` for new writes (one sibling-temp + no-clobber file per event); legacy `provenance/v1/events/`, `provenance/v2/events/`, `provenance/v3/events/` and `provenance/v4/events/` and `provenance/v5/events/` and `provenance/v6/events/` remain readable but are never written or migrated. Deleting the session removes its provenance.

## Provenance & Evidence

Doge-Code observes its own work and links it as:

```text
Plan Item
 -> Semantic Change
 -> Verification Observation
```

```text
step-2
 ├─ change chg-...
 │   └─ src/auth.rs / sym-v1-...
 └─ verification
     └─ cargo test [passed]
```

- A verification observation records only that a command was started and finished against a workspace snapshot; it never claims the implementation is proven or guaranteed correct.
- A `ChangeCommitted` event is a workspace mutation Doge-Code actually committed (observed `before -> after` transaction), not an LLM self-report.
- Tracked mutations (v7 writes, legacy v1–v6 reads): `fs_write`, `edit`, `apply_patch`, transactional `/edit-symbol`, `undo`.
- Automatically tracked verification: `execute_process` classified commands, `/test`, `/lint`.
- Not tracked (reported honestly, never inferred): `execute_bash`, `execute_shell`, workflow runs, remote MCP verification, external/manual edits. Session `changed_files` are agent-write scoped, so not all workspace modifications are tracked.
- `undo` is safe LIFO mutation rollback: current-state guard, created-file deletion, fail-closed conflicts, no redo yet.
- Storage: `.doge/sessions/<id>/provenance/v7/events/<uuid>.json` for new writes (one sibling-temp + no-clobber file per event); legacy `provenance/v1/events/`, `provenance/v2/events/`, `provenance/v3/events/` and `provenance/v4/events/` and `provenance/v5/events/` and `provenance/v6/events/` remain readable but are never written or migrated. Deleting the session removes its provenance. The repomap SQLite DB is a rebuildable cache and is never used for durable provenance.
- Use `provenance_read` to inspect events with pagination (`cursor` 0-based, `page_size` max 100) and coverage (`tracked_active`, `verified_active` = observed by at least one successful verification command, `unverified_active`, `diverged`, `unlinked`, `untracked_changed_files`, `reverted`). Filter by `verification_obligation_id` to see obligation definition transitions + matched verifications.

## Verification Obligations

A plan item can declare what should be observed for that step:

```json
{
  "id": "step-cache",
  "content": "Implement authentication cache",
  "status": "in_progress",
  "requirement_ids": ["req-cache"],
  "verification_obligations": [
    {
      "id": "vo-cache-tests",
      "description": "Cache unit tests pass",
      "kind": "test",
      "command": {"program": "cargo", "args_prefix": ["test", "auth::cache"]}
    },
    {
      "id": "vo-clippy",
      "description": "Lint passes",
      "kind": "lint",
      "command": {"program": "cargo", "args_prefix": ["clippy"]}
    }
  ]
}
```

- Kind-only obligations (no `command`) match by kind + plan scope. With `command`, matching requires kind + executable basename + argv prefix (`actual argv starts_with(args_prefix)` per-token prefix, e.g. `["test", "provenance::"]` matches `["test", "provenance::traceability_tests"]`). No regex, glob, or shell parsing.
- Each obligation has a stable binding hash (`plan_item_id` + sorted requirement ids + definition). Later plan edits never rewrite historical attribution.
- Evidence states: `no_linked_change` / `pending` / `observed_passing` / `observed_failing` / `stale` / `diverged` / `reverted` / `mixed`. `observed_passing` requires id + binding match, success, and current active changes ⊆ observed changes. A later mutation can make earlier evidence `stale`.
- `observed_passing` is evidence, not a correctness proof. Failing runs never count as passing.
- `requirements_read` shows compact obligation states per requirement; full definitions live in `plan_read`.

## Evidence-aware Diff Review

The Diff Review panel shows diff + evidence:

```text
Requirements: req-cache
Plan: step-cache
✓ vo-cache-tests  observed passing
? vo-validation-test pending
```

- Per-file evidence comes from current active changes (superseded history excluded). Unrelated user files are never mixed in.
- Evidence is bounded (truncated descriptions/commands, capped obligations) and never breaks the diff view; failures become warnings.
- Status labels never say `verified`: `✓ observed passing`, `✗ observed failing`, `? pending`, `! stale`, `! diverged`, `↩ reverted`, `- no linked change`.
- Small terminals hide the evidence pane and show a compact `Evidence 1/3 observed passing` summary instead.

## 🎯 Usage Examples

### Basic Interactive Usage
```bash
# Start interactive session
dgc

# Resume the most recently updated session
dgc --resume

# Resume a specific session (ID prefix allowed)
dgc --resume=0198abcd

# Skip repomap generation for faster startup
dgc --no-repomap
```

### Command Line Operations
```bash
# Execute single instruction
dgc exec "Add unit tests to user authentication module"

# Continue the most recently updated session
dgc exec --resume "Add error handling to the login flow"

# Continue a specific session (ID prefix allowed)
dgc exec --resume=0198abcd "Add error handling to the login flow"

# JSON output for programmatic use (includes tools_called, token usage, etc.)
dgc exec --json "Refactor database layer"

# Rewrite specific code
dgc rewrite --prompt "Optimize this function for performance" \
    --code-file /tmp/algorithm.rs \
    --json
```

### Session Management
```bash
# List sessions (most recently updated first)
dgc session list

# Show details of a session (ID prefix allowed)
dgc session show 0198abcd

# Delete a session (confirmation prompt; `--yes` skips it)
dgc session delete 0198abcd
```

In the TUI, `/session list` shows the same table with the current session marked, and `/session switch <id>` accepts ID prefixes as well.

Session checkpoints replace `session.json` atomically using a private sibling
temporary file, with a shared 16 MiB save/read limit. A failure before replacement
keeps the old file intact; a directory-sync failure after replacement is reported
as such. IDs must be single path components, stored IDs must match their directory,
and session/metadata symlinks are rejected. Malformed sessions are preserved and
reported rather than silently skipped during listing or latest-session selection.
The retention limit is 100 sessions and excludes the checkpoint being saved. If
safe retention inventory fails, the checkpoint succeeds with a warning and no
sessions are removed. Keep the store in a trusted directory: session leases do not
guarantee protection against concurrent adversarial directory replacement.

TUI startup resolves `--resume` before creating a session, so retention cannot
remove the requested target during startup. Deleting the active session clears
its runtime conversation and retry inputs; the next prompt starts a fresh session.
Deleting another session leaves the active conversation intact. `/session clear`
keeps the session identity and metrics: a failure before checkpoint replacement
preserves the live conversation, observations and unseen results. After replacement,
a directory-sync warning adopts the cleared checkpoint in memory and reports the
unconfirmed durability.

While a foreground job owns the session, TUI session creation, switching, deletion,
resume and clearing wait for the job to release it, including its final checkpoint.
A final save failure marks the job failed and keeps the complete in-memory checkpoint
(messages, observations, unseen results and usage) available for `/session save`.
`/session current` shows an unsaved checkpoint. Saving waits for foreground ownership
to be released and retries the existing payload without charging usage again. New
sessions and switches first flush pending changes; a failure leaves the current
conversation and retry inputs intact. `/quit` and the second Ctrl+C cancel active
jobs and wait for cleanup before flushing. A failed flush keeps the UI open with a
retry hint; stalled cleanup also keeps the UI open. The final error-path flush runs
after shutdown and reports failure with a nonzero exit. Explicit clear/delete retain
their intentional discard behavior. Capacity errors remain unsaved; this does not
remove capacity limits or add power-loss guarantees.

A session takes a nonblocking exclusive OS lease before use and retains it through
its final checkpoint and shutdown. Concurrent use or deletion of the same session
is refused explicitly; different sessions can run concurrently, and read-only
list/show remains available. Retention skips active sessions and may temporarily
exceed the session limit. This protection requires participating binaries and a
filesystem supporting OS locks; old binaries, external manual edits and unsupported
network filesystems are outside this guarantee.

For an oversized unsaved checkpoint, `/session export` writes the complete current
in-memory session (conversation, provider-bound state, tool calls/results,
observations, unseen results and usage) into a version-1 JSON recovery envelope
under `.doge/sessions/.recovery/<UUID>.json` (or the configured session store).
The recovery limit is 64 MiB; the normal save/read limit remains 16 MiB. Both use
bounded streaming serialization. Recovery files use private file permissions and
are published without overwriting existing files. A post-publication directory-sync
warning means the file exists but its durability is unconfirmed.

Export requires an idle foreground and retains the current conversation, usage,
lease and unsaved state. It does not enable new/switch/quit by itself. Inspect the
reported absolute path and JSON first; the file is a recovery artifact, **not** a
normal `--resume` input. You can read or extract the conversation and tool results
from its `session` field. After verifying the export, explicitly clear or delete
the conversation if you choose to discard it, then continue in a session fitting
the normal limit. Import, oversized resume, automatic offload and automatic
summarization are not provided. A recovery exceeding 64 MiB fails explicitly.

After CLI completion or an irreversible TUI error exit, a final capacity failure
alone triggers one recovery attempt after checkpoint owners stop. The path and
byte count go to stderr, and the original failure remains nonzero. Recovery
failure reports both errors. Ordinary permission/disk errors do not trigger an
automatic export, and recoverable TUI failures only guide you to `/session export`.
Recovery files are excluded from normal session listing, ID resolution and
retention cleanup; read-only stores cannot create recovery files.

`exec --json` includes a `usage` object with inference attempts, usage-report count,
provider-reported prompt/completion/total subtotals, and optional reasoning/cache
subtotals. The legacy `tokens_used` field remains the reported prompt subtotal.
`unknown_usage_attempts` counts sent attempts without usage, including failed
retries; a subtotal of zero does not assert zero billing. Missing optional metrics
remain `null`, while explicitly reported zero remains zero. Session usage adds each
checkpoint delta once and survives resume; legacy sessions mark their historical
usage unknown. Persisted session usage includes agent turns, retries, automatic
summaries/compactions, task subagents, nested `doc_generate` requests using the
shared client, manual local `/compact` requests, and `/edit-symbol` and `/fix`
LLM requests. Still outside durable accounting: external MCP services' internal
model usage, provider usage that is never returned/observed before hard process
termination, external/manual model calls, and independent programs outside dgc.
`all_tracked_attempts_reported` refers only to the tracked attempts, never
complete provider billing or remaining plan quota.

`find_file` sorts and deduplicates complete paths and budgets the entire serialized
JSON to at most 6,000 characters (at most 200 paths). Use `next_cursor` with the same
query to continue; `returned`, `total_matches` and `truncated` describe the page.
An insufficient budget for even one path is an error. A filesystem change between
pages can shift offsets; the directory walk itself has no hard memory/time cap.


### Claude Desktop Integration
1. Start MCP server: `dgc mcp-server 127.0.0.1:8000`
2. Configure Claude Desktop to connect to `http://127.0.0.1:8000`
3. Access project tools directly from Claude Desktop chat interface

### Emacs Integration
Doge-Code provides a powerful Emacs integration. Setup is simple:

1. Add the `elisp` directory to your `load-path`.
2. Run `(doge-code-setup)` in your `init.el`.

```elisp
(add-to-list 'load-path "/path/to/doge-code/elisp")
(require 'doge-code)
(setq doge-code-executable "dgc") ; Ensure dgc is in PATH or specify full path
(doge-code-setup)
```

This enables:
- **Analyze/Refactor**: `C-c d a` (Analyze), `C-c d r` (Rewrite snippet)
- **MCP Tools**: `C-c d m s` (Symbol Search), `C-c d m f` (Read File)
- **Auto-Fix**: `C-c d f` (Fix Flymake error), Compilation auto-fix
- **HUD**: Semantic info overlays (optional, set `doge-code-enable-hud` to t)

See [elisp/emacs-integration.md](elisp/emacs-integration.md) for detailed configuration options.

## 📝 TUI Slash Commands

The TUI provides various slash commands for quick operations:

| Command | Description |
|---------|-------------|
| `/help` | Display available commands and help |
| `/quit` | Exit the application |
| `/clear` | Clear the screen |
| `/cancel [job-id]` | Cancel the current foreground job or a specific job (`/cancel job-12`) |
| `/jobs` | List running and recent jobs |
| `/evidence [id]` | Inspect saved evidence and show the session-specific export command |
| `/compact` | Compact conversation history (OpenAI-compatible: LLM summarization; openai-chatgpt: automatic Responses native compaction, no local summarizer) |
| `/edit-symbol` | Edit symbols (functions/classes) at current diff position |
| `/lint` | Run linters and apply auto-fixes |
| `/test` | Run tests for the project |
| `/map` | Display RepoMap |
| `/rebuild-repomap` | Rebuild the RepoMap |
| `/open` | Open a file |
| `/git-worktree` | Create an isolated linked worktree and branch (does not switch the current session into it) |
| `/theme` | Change color theme |
| `/tokens` | Display token usage (prompt/cache/reasoning usage when reported by the provider) |
| `/tools` | List available tools |
| `/plan show` | Display current plan |
| `/session <sub>` | Manage sessions: `new`, `list`, `show`, `switch`, `save`, `delete`, `current`, `clear` |
| `/stack [add <task>|next|list]` | Manage ephemeral task stack (prompts waiting for manual execution) |

`/stack` holds prompts waiting for manual execution; `/jobs` shows work
currently executing or recently finished. They are different lists: a busy
foreground job is rejected explicitly (use `/jobs` to inspect it or
`/cancel` to stop it) and is never auto-queued into `/stack`.

`/jobs` and `/cancel [job-id]` submitted with Enter are handled immediately,
even while a job is running; Esc also cancels the current foreground job.
Other prompts keep FIFO queue order and are consumed only after the foreground owner releases its slot, including cancellation cleanup, diff collection and session saves. Cosmetic done/error messages cannot release the queue. Busy reservation races retain accepted prompts for retry. `/compact` is a foreground
`compact` job visible in `/jobs`, cancellable with `/cancel` or Esc, and drained
on shutdown. While it owns the session, another foreground job, session switch,
or `/clear` is rejected. It summarizes only a safe history prefix and preserves
unseen tool-call batches and recoverable observations; when no safe prefix exists,
it leaves the conversation unchanged without contacting the provider.

Manual compaction checks that the starting session and history are still current
before saving, and adopts the summary only after the save succeeds. Provider,
validation, and pre-commit save failures retain the original conversation and are
reported as failed jobs, without a success notice. Cancellation before commit
leaves history unchanged; cancellation after a saved summary does not undo it.
If replacement succeeds but directory sync fails, disk and memory both adopt
the summary and a warning reports that crash durability is unconfirmed; this
produces neither a normal success notice nor a claim that history was unchanged.
Shutdown waits for an already-started synchronous commit to finish before
terminalizing that job. Manual compaction remains outside session agent-turn
usage totals.

## 🔍 search_repomap Cheat Sheet

- `result_density`: Default `"compact"` returns no snippets and compresses to 5 symbols per file. Switch to `"full"` only for files where you need details to save context.
- `response_budget_chars`: Pass an upper limit like "5,000 characters" to automatically trim limit/symbol count/snippet length and prevent results from getting too large. If within budget, `warnings` and `next_cursor` allow fetching continuation.
- `cursor` / `page_size`: Paginate sorted results. `cursor` is 0-based next position, `page_size` is fetch count. If `next_cursor` is `Some(x)`, fetch next page with same query + `cursor=x`.
- Response is `SearchRepomapResponse`, returning `results` (conventional `RepomapSearchResult` collection) plus `warnings` and `applied_budget` (summary of actual limits applied).

Combining these enables maximum code exploration effectiveness without overwhelming LLM context.

## 📂 File Tools Lightweight Mode

- `fs_read`: Default `mode="summary"` returns up to 400 complete lines and 6,000 Unicode scalar characters including line separators. `next_cursor` is the first unread 1-based line; `end_line` describes only returned lines. A first line too large for the budget is an explicit error: retry the same line with a larger budget. EOF cursors return empty content. `mode="full"` removes the default line cap but still obeys character and serialized JSON limits.
- `fs_read_many_files`: Resolved paths retain their existing order, with 5 paths per page and up to 40 lines / 1,200 Unicode scalar characters per file in `mode="summary"`. The combined snippet budget defaults to 8,000 characters. `next_cursor` is the actual first unread 0-based path index, including skipped directories. If the first snippet exceeds the total budget, a smaller file summary is returned with `truncated=true`; use `fs_read` for its remaining content. Truncation guidance belongs to metadata, not snippet text.
- For both read tools, explicit line/page/snippet/budget limits must be positive; the batch path cursor may be zero. Character budgets count Unicode scalars, not bytes or grapheme clusters, and are capped at 40,000. The entire serialized result must also fit the existing 40,000-character read safety limit, so escaping and metadata may reduce complete lines/files further. Oversized single lines have no offset pagination; increase the budget within these limits or extract smaller lines outside the tool. `full` is still bounded.
- `fs_list`: Directory listings also return as `FsListResponse`, with `entries` containing only `path` and `is_dir` for compactness. Use `cursor`/`page_size`/`response_budget_chars` to progressively fetch deep tree structures.

## 🎯 Symbol-Specific Editing /edit-symbol

- Running `/edit-symbol` identifies symbols (functions/impl/struct etc.) from the most recent `@path:line`/`@path#Lline` file/line specification.
- Each symbol carries a stable ID (`sym-v1-...`, returned by `search_repomap` as `symbol_id`). IDs are deterministic from project-relative path, kind, parent, and name — never absolute paths, line numbers, or tree-sitter node IDs — so the same symbol in another worktree has the same ID.
- After target selection the edit no longer depends on line numbers: the transaction prepares a `SymbolId` + content fingerprint, asks the LLM for the complete symbol replacement only (never a diff), then re-resolves the ID at the current location, checks the fingerprint, validates the candidate in memory, race-checks immediately before write, and commits atomically.
- Fail-closed: if the target changed (`StaleTarget`), disappeared (`TargetNotFound`), was renamed (`IdentityChanged`), or the file raced (`ConcurrentModification`), nothing is written — re-run `/edit-symbol`. Unrelated same-file edits are preserved and only the target file is shown in diff review.
- Runs as a `semantic_edit` foreground job: visible in `/jobs`, cancellable via `/cancel` (cancel never mutates the file).
- v1 limitations: `rename`, `move`, and intent-scoped undo are not supported; `Variable`/`Comment` kinds are rejected; same-name overloads use a collision ordinal that can shift if a sibling is inserted above.

### Stable Symbol IDs

`search_repomap` returns `symbol_id` for every symbol (no fingerprint in search results; fingerprints are computed only for the edit target to avoid extra I/O and token cost).

Text mutation tools resolve an authorized canonical target before reading or
writing. Editing through a file, directory or project-root symlink preserves the
alias and changes its target; undo and captured review receipts refer to that
canonical target. Existing scope and temporary-directory allowances still apply.
Dangling links, link cycles and non-regular files are rejected. Before publishing,
tools recheck the requested path, scope, file identity and contents; detected
retargeting or replacement conflicts leave tracking unchanged. Undo refuses a
changed canonical parent or target, even when its contents match.

These checks are optimistic: there is still a gap between the final check and
rename or unlink. They do not provide an operating-system compare-and-swap or a
filesystem sandbox against concurrent directory replacement.

Tool results from one assistant batch remain contiguous before loop, stalled
progress or recovery hints. Interrupted calls are saved as unknown outcomes;
resume preserves completed results and does not replay pending side effects.
Malformed legacy tool-result blocks are refused with an `invalid history`
diagnostic. Their saved conversation is retained for inspection rather than
silently moving user instructions or removing messages.

## 📋 Diff Review Panel

After file modifications (`fs_write`, `edit`, `apply_patch`), the TUI automatically shows an inline diff review panel (enabled by default via `show_diff = true`):

- **Scoped to agent changes**: the TUI diff covers captured text mutations from this turn, from the first pre-edit contents to the last committed contents; existing staged, unstaged and untracked user content is the baseline
- **Split view**: log on the left, diff preview on the right with per-file tabs showing addition/deletion counts
- **Syntax highlighting**: additions in green, removals in red, hunk headers in yellow, etc.
- **Keyboard controls** (active while the input box is empty):
  - `a` — accept changes (keep them applied)
  - `r` — restore this review’s captured pre-edit contents, preserving Git’s index. Only files captured as missing before creation are removed. The agent is notified of restored changes on your next instruction
  - `q` / `Esc` — dismiss the panel (changes remain applied). During rollback, Esc requests cancellation and the panel stays open until the result; accept/dismiss are disabled
  - `←`/`→` — switch between changed files
  - `↑`/`↓` — scroll; `PgUp`/`PgDn` — fast scroll; `Home`/`End` — jump to top/bottom

Reject runs as a foreground workspace-write job. Before any restoration, every
captured target must still match its reviewed contents and have the expected file
type, with no symlink traversal or paths outside the project. An initial conflict
leaves every file untouched. Each restore rechecks its target; later conflicts,
I/O failures or cancellation report restored mutations and remaining mutations
separately, and retries skip completed restores. These checks are optimistic:
uncooperative external writers can still race filesystem operations.

Rollback evidence is independent of `/undo` and bounded to 128 receipts and 8 MiB
of captured text/diffs per turn. Overflow, unknown baselines, mixed external edits
between agent edits, and unsupported or legacy reviews are view only, with a
reason. Shell, remote and other edits without mutation receipts are outside this
rollback scope. Accept and dismiss only discard review evidence; they do not edit
files. Review evidence is memory-local and expires when a new turn replaces it or
the session ends.

Set `show_diff = false` in `.doge/config.toml` to disable the panel.

## 🛡️ Linter and Auto-Fix /lint

- `/lint` command auto-detects Go, Rust, TypeScript files in the project and runs configured linters (`cargo clippy`, `golangci-lint`, `npm run lint`, etc.).
- Attempts auto-fix (`--fix`) for detected issues, and for complex issues that can't be resolved, delegates analysis to LLM to propose fixes.
- Projects with multiple languages can be checked all at once.
- Linter commands are trusted internal commands and use the shared managed process lifecycle, but do not inherit the LLM execution allowlist.

## 🧪 Testing /test

- `/test` command automatically detects the project type and runs appropriate test commands:
  - Rust: `cargo test`
  - Go: `go test -json ./...`
  - Node.js: `npm test`
- Test output is captured and can be analyzed by LLM for failure diagnosis.
- `/test` uses the configured `command_timeout_ms` (where `0` means unlimited) and the same managed process lifecycle as finite LLM commands, without applying the LLM execution allowlist.

## 🔄 Advanced Features

### RepoMap System
Doge-Code builds a comprehensive symbol map of the project:
- Automatic language detection
- Symbol extraction (functions, structs, classes, etc.)
- Cross-reference analysis
- Incremental updates on file changes

### Smart Editing
- **Symbol-aware editing**: Edit entire functions, classes, modules
- **Diff review**: Preview changes before applying
- **Context preservation**: Maintain code style and patterns
- **Multi-file coordination**: Apply related changes across files

### LLM Integration
- **OpenAI Compatible**: Works with OpenAI, Anthropic, and other APIs
- **Streaming Responses**: Real-time output during LLM generation
- **Tool Use**: LLM autonomously calls tools
- **Conversation History**: Maintain context across interactions

### Automatic Verification
After file edits (`fs_write`, `edit`, `apply_patch`), the agent is instructed via system notes to verify changes:
- **Rust**: Run `cargo check` / `cargo test`
- **Python**: Syntax check (`python -m py_compile`)
- **Go**: `go build`
- **TypeScript**: `tsc --noEmit`

Verification failures are returned to LLM for automatic correction.


### Remote MCP Tools
- Connect to remote MCP servers for additional tool capabilities
- Unified tool interface for local and remote tools
- Automatic paginated tool discovery and registration
- MCP protocol negotiation is delegated to rmcp; Streamable HTTP probes the
  2026-07-28 lifecycle when supported and falls back to legacy `initialize`,
  while stdio uses the established initialize lifecycle
- A completed tool result with `isError = true` is reported as a failed Doge
  tool call; protocol/transport failures are reported separately
- Structured MCP content and `structuredContent` are retained in a bounded
  JSON result. Input-required and MCP Task responses are surfaced as explicit
  unsupported results until their UI/lifecycle integrations exist.

### Tool Search / Deferred Tools
- Doge-Code can keep large tool catalogs out of the initial LLM context.
  Core tools (`search_repomap`, `fs_read`, `search_text`, `task`,
  `execute_process`, `observation_read`) are loaded eagerly; other built-in
  and MCP tools are found and activated on demand via `tool_search`.
- Activated tools appear in the next LLM request and stay active for the run.
- Already-active matches never consume activation capacity: the search limit
  bounds inactive activation, and active matches are reported separately.
- Guessing a deferred tool name never executes it: dispatch fails closed
  with `tool_not_active` until `tool_search` activates it.
- Once every real deferred tool is active, `tool_search` retires from the
  next tool list; a stale `tool_search` call then fails closed with
  `tool_not_active` and no side effects.
- Provider-independent (works with OpenAI / Anthropic / OpenRouter / any
  OpenAI-compatible provider) and deterministic: lexical ranking only, no
  embeddings.
- Provider wiring differs:
  - OpenAI-compatible: activated schemas move into the next request's
    top-level tools.
  - openai-chatgpt / Responses: initial top-level namespace stays stable;
    activated functions are appended as additional_tools input items.
- Doge-Code `tool_search` is a client-side local function. It is not the
  OpenAI Responses native `type = "tool_search"`; the ChatGPT plan route
  never emits the latter.
- Config:

```toml
[tool_routing]
mode = "auto"  # auto / eager / deferred
search_result_limit = 5  # 1-10
```

- `auto` defers when the catalog holds 10+ tools, otherwise exposes
  everything. `eager` is legacy compatibility (all tools exposed,
  `tool_search` hidden). `deferred` always uses deferred routing.

### Preflight Context Governor

- Before each request Doge-Code measures the current footprint
  (messages + active tool schemas + runtime overlay + overhead) and reduces
  pressure as overlay drop, then recoverable Observation Store offload, then
  unseen-safe compaction as a last resort.
- Config:

```toml
[context_budget]
mode = "auto"  # auto / observe / off
```

- `auto` reduces automatically, `observe` logs the estimate but keeps legacy
  behavior, `off` disables the governor.

### Conversation History Compaction
- Automatic compaction when token threshold is exceeded
- Historical tool results are offloaded recoverably to the Observation Store
  (`obs-*` + `observation_read`) before full compaction; unseen results are
  never offloaded or summarized until the model has seen them once
- LLM-based summarization preserves essential context
- Recent-file / automatic memory hints are injected only as bootstrap context for the first request; they are never stored in durable conversation history
- Recent conversation tail is retained across compaction (tool-call pairs kept intact)
- Structured format for files accessed, actions taken, and outcomes

### Git Worktree Management
- Create isolated worktrees for parallel development
- Branch-based worktree creation
- Automatic cleanup

### Error Recovery System
- Autonomous error detection and diagnosis
- Recovery strategy selection
- Self-debugging capabilities

### Hook System
- Execute custom processing after each instruction
- Extensible hook interface
- Built-in hooks for repomap updates

### Custom Commands
- Define custom slash commands in `.doge/commands/`
- Template-based command definitions
- Parameter support for dynamic commands

## 📚 Configuration

### Environment Variables
- `OPENAI_API_KEY`: API key
- `OPENAI_BASE_URL`: API base URL (default: `https://api.openai.com/v1`)
- `OPENAI_MODEL`: Model name (default: `gpt-4o-mini`)
- `DOGE_CODE_CONFIG`: Explicit global config path. When set, that path alone
  is authoritative: a missing, unreadable, or invalid file is a startup error
  and no other config candidate is consulted.

### Configuration File (`.doge/config.toml`)
```toml
model = "gpt-4o-mini"
base_url = "https://api.openai.com/v1"
project_instructions_file = "PROJECT.md"
resume = false
auto_compact_prompt_token_threshold = 250000

[llm]
# Context window size (auto-detected if not specified)
context_window_size = 128000
# Retry policy (single source of truth for all LLM requests):
# max_retries = additional retries after the first attempt (max_attempts = max_retries + 1)
max_retries = 3
retry_base_ms = 1000
retry_jitter_ms = 500
respect_retry_after = true
# Chat and subscription Responses honour integer Retry-After up to 300 seconds
# exactly; longer hints decline retry. Missing/invalid hints or respect=false
# use local exponential backoff + jitter. Responses keeps its cap of 4 attempts,
# with request_timeout_ms covering the whole request, including all retry waits.

[context_budget]
mode = "auto"  # auto / observe / off

[agent_budget]
max_iterations = 256
# max_tool_calls = 256
# max_elapsed_ms = 900000
# max_total_tokens = 1000000

# Local MCP HTTP listener (Doge-Code's own server)
[mcp_server]
enabled = false
address = "127.0.0.1:8000"

[watch]
include_patterns = ["*.rs", "*.go", "*.ts", "*.py"]
exclude_patterns = []
debounce_delay_ms = 500
ai_comment_pattern = "// AI!:"

[[mcp_servers]]
# Structured stdio: command is executed directly (never through a shell).
name = "filesystem"
enabled = true
transport = "stdio"
command = "/usr/local/bin/mcp-filesystem"
args = ["--root", "/workspace"]

# Milliseconds; 0 means unlimited.
connect_timeout_ms = 30000
list_timeout_ms = 10000
call_timeout_ms = 30000

# [mcp_servers.env] contains literal child-process environment values.
# DOGE_LOG_LEVEL = "info"

[[mcp_servers]]
# Streamable HTTP uses address as its URL.
name = "remote"
enabled = true
transport = "http"
address = "https://example.com/mcp"
connect_timeout_ms = 30000
list_timeout_ms = 10000
call_timeout_ms = 120000
```

Configuration is fail-closed. Unknown top-level or nested keys are startup
errors, as are invalid enum/policy values (for example `[tool_routing]`,
`[context_budget]`, and `[reasoning]` modes/efforts) and
`[tool_routing] search_result_limit` values outside `1..=10`. The config
loader is read-only: invalid files are never auto-repaired or rewritten,
and typos never silently fall back to defaults.

### MCP Servers (Local vs Remote)

See [`docs/mcp-3x-migration.md`](docs/mcp-3x-migration.md) for the rmcp 3.x
migration and remote-result semantics.

Doge-Code distinguishes two MCP configurations:

```toml
# Local HTTP listener (Doge-Code itself serves MCP over HTTP)
[mcp_server]
enabled = false
address = "127.0.0.1:8000"

# Remote MCP servers Doge-Code connects to as a client
[[mcp_servers]]
name = "filesystem"
enabled = true
transport = "stdio"
command = "/usr/local/bin/mcp-filesystem"
args = ["--root", "/workspace"]

# Legacy stdio form (deprecated; whitespace splitting is retained temporarily)
# [[mcp_servers]]
# name = "legacy"
# enabled = true
# transport = "stdio"
# address = "server --foo bar"
```

- `[mcp_server]` is the local listener. `dgc mcp-server [address]`
  starts it in the foreground (CLI address > `[mcp_server].address` >
  `127.0.0.1:8000`); the TUI starts it in the background only when
  `enabled = true`.
- `[[mcp_servers]]` are outbound/remote endpoints used by
  `RemoteToolManager`/`McpClient`. They never start a local listener.
- Structured stdio `command` and `args` are passed as an executable/argv pair;
  no shell parser or `bash -c` is involved. `env` values are literal and are
  merged per key with project values taking precedence over global values.
- Do not specify both structured `command` and legacy `address`; ambiguous
  stdio configuration is rejected. HTTP endpoints reject stdio-only fields.
  Enabled server names must be unique.
- `connect_timeout_ms`, `list_timeout_ms`, and `call_timeout_ms` default to
  30,000/10,000/30,000 milliseconds. `0` means unlimited for that operation;
  unlimited HTTP connections use legacy initialization to avoid the SDK's
  fixed modern-discovery probe deadline. An unlimited server that does not
  complete within the registry's five-second registration grace period is
  recorded as failed without blocking other tools.
- Remote tool arguments, results, environment values, and credentials are not
  logged verbatim. A failed remote server does not prevent other servers or
  local tools from loading.

Doge-Code's built-in MCP HTTP listener is currently loopback-only.

Allowed bind targets:
- 127.0.0.1
- ::1
- localhost

Remote/public MCP hosting requires a future spec-compliant authorization
implementation.

DNS rebinding protection: even on loopback, the local listener validates
the `Host` header (`localhost`/`127.0.0.1`/`::1` only, any port) and the
`Origin` header when present (`http://localhost:*`,
`http://127.0.0.1:*`, `http://[::1]:*` only; missing `Origin` is allowed
for non-browser MCP clients). Requests with an untrusted `Host`/`Origin`
are rejected with `403` before reaching the MCP handler. No wildcard CORS
is configured and `X-Forwarded-*` headers are never trusted.

### Execution Policy (`[execution]`)

Structured execution keeps normal commands out of the shell, so shell
injection strings can never become a security boundary:

```toml
[execution]
# unrestricted / allowlist / deny
mode = "allowlist"
allowed_programs = ["cargo", "rustc", "git", "rg"]
# Arbitrary shell syntax is substantially more powerful than execute_process.
allow_shell = false
allowed_env = ["RUST_BACKTRACE", "RUST_LOG", "CARGO_TERM_COLOR"]
```

- `execute_process` takes `program` + `args` separately and spawns the
  program directly (never `bash -c`). `args: ["hello; touch /tmp/x"]` is one
  literal argument — `touch` never runs.
- `mode = "allowlist"` matches executables exactly: `allowed_programs =
  ["cargo"]` allows `program = "cargo"` only — not `./cargo`, `/tmp/cargo`,
  or `cargo;rm`. Absolute paths must be listed explicitly to be allowed.
- `execute_process.cwd` must resolve under the project root or
  `allowed_paths` (canonicalized, symlink-safe).
- In allowlist mode only `allowed_env` keys can be overridden; `PATH` and
  friends (`LD_PRELOAD`, `GIT_SSH_COMMAND`, …) cannot be swapped by the LLM.
  Env values are never logged.
- `execute_process.timeout_ms` can only shrink the run; the effective timeout
  is `min(request, command_timeout_ms)`. `command_timeout_ms = 0` means no
  configured process timeout (unlimited, while cancellation remains available).
- `allow_shell = false` denies both `execute_bash` and `execute_shell` with a
  structured `policy_denied` result.
- Legacy `allowed_commands` (deprecated) is used only when `[execution]` is
  absent (a bare `[execution]` table with no fields does not count as
  configured). Empty means unrestricted-legacy (with a one-time warning);
  non-empty entries are parsed as simple `program + arg-prefix` commands —
  shell operators (`;`, `&&`, `||`, `|`, `>`, `$()`, backticks, newlines, …)
  are denied for both `execute_bash` and `execute_shell`. Safe simple
  `execute_bash` commands run shell-free via the new runner; safe simple
  `execute_shell` commands still run in the persistent session (so
  `cd`/`export` state is preserved) but never with shell syntax attached.
- Adding any `[execution]` field makes it authoritative and disables the
  legacy fallback entirely (with a warning when both are set).
- Timeouts and agent cancellation terminate the whole process tree on
  Unix (SIGTERM → grace period → SIGKILL to the process group, then reap);
  other platforms kill and reap the direct child. The same mechanics are
  reused by trusted `/test` and `/lint` commands, with an internal diagnostic
  output budget rather than the LLM 6,000-character tool budget.

### Workflow Files

`.doge/workflows/*.yml` steps support two shapes — exactly one per step:

```yaml
steps:
  # Structured step (process policy applies, no shell)
  - name: Test
    program: cargo
    args: [test, --workspace]
    # optional: cwd, env, timeout_ms
  # Shell step (allow_shell policy applies)
  - name: Lint
    run: cargo clippy --all-targets | head -50
```

## 🧪 Development

### Build and Test

Start with [AGENTS.md](AGENTS.md) for change-specific guidance. Shared development
Skills live in `docs/ai/skills/` and are linked for Codex, Claude, and OpenCode.
Development check scripts require Python 3.11+. Search tests require ripgrep
(`rg`) on PATH. Check logs are retained in a temporary directory. Rust tests
require loopback socket binding for local HTTP fixtures.

```bash
# Code formatting
cargo fmt --all

# Focused Rust tests (zero matching/executed tests is an error)
bash scripts/verify.sh test llm::tool_execution::dispatch::

# Complete Rust gates: fmt check, locked Clippy (-D warnings), locked tests
bash scripts/verify.sh rust

# Docs/Skills/development scripts only: layout, links, routing and script tests
bash scripts/verify.sh guidance

# Build release version
cargo build --locked --release
```

Use `bash scripts/verify.sh msrv` for the installed MSRV toolchain,
`bash scripts/verify.sh tui-deps` for the TUI dependency graph, and
`bash scripts/verify.sh macos` for focused tests on macOS. See the
[verification matrix](docs/ai/workflow.md) for required checks and
[agent evaluation protocol](docs/ai/evaluation.md) for measuring guidance changes.
The evaluation comparer requires explicit complete usage coverage for schema-v2
records before reporting token totals or tokens per accepted run. Missing or
inconsistent attempt/report counters leave these metrics unknown, even if the
record supplies numeric token subtotals; `known_*` fields retain those subtotals.
Optional cache/reasoning/cache-write totals also require item-specific report
coverage; reported subtotals and item counts are retained in schema-v2 records.
Older schema-v2 cache values without item counters contribute to the known
subtotal, while their complete cache total stays unknown. Ordinary input/output
totals and legacy-v1 measured comparisons retain their existing behavior.

### Adding New Languages
1. Add tree-sitter parser dependency to `Cargo.toml`
2. Implement `LanguageSpecificExtractor` trait
3. Add to language detection in `src/analysis/mod.rs`
4. Add tests in `src/analysis/tests/`

### Adding New Tools

Follow the [tool registration checklist](docs/tool-output-contract.md#tool-registration)
and [dgc-tool-dev skill](docs/ai/skills/dgc-tool-dev/SKILL.md). A default tool needs
its implementation, module wiring, schema registration, dispatch handler, behavior
tests, and a README entry. Run the guidance check to detect missing wiring.

### Adding Custom Commands
Create a TOML file in `.doge/commands/` directory:
```toml
name = "my-command"
description = "Description of my command"
template = "Execute the following task: {args}"
```

## 📖 Documentation

- **System Prompt**: `resources/system_prompt.md` - AI behavior guidelines
- **Agent Guidelines**: `AGENTS.md` - Integration procedures
- **Agent Development Workflow**: [docs/ai/workflow.md](docs/ai/workflow.md) - Shared Skills and verification matrix
- **Architecture Routing**: [docs/ai/architecture.md](docs/ai/architecture.md) - Module responsibilities and change entry points
- **Development Contracts**: [docs/ai/contracts.md](docs/ai/contracts.md) - Mutation, execution, context, and provenance boundaries
- **Agent Evaluation**: [docs/ai/evaluation.md](docs/ai/evaluation.md) - Representative tasks and measured comparisons
- **Tool Output Contract**: `docs/tool-output-contract.md` - Tool response/truncation spec
- **Emacs Integration**: `elisp/emacs-integration.md`
- **API Documentation**: Generate with `cargo doc`

## 🏗️ Architecture

### Module Structure
- **`src/main.rs`**: CLI entry point and application orchestration
- **`src/analysis/`**: tree-sitter based code analysis and symbol extraction
- **`src/tools/`**: File system and code manipulation tools
- **`src/tui/`**: ratatui-based terminal user interface
- **`src/llm/`**: OpenAI-compatible LLM client and tool execution
- **`src/session/`**: SQLite-based session persistence
- **`src/mcp/`**: Model Context Protocol server implementation
- **`src/config/`**: Configuration management and TOML parsing
- **`src/features/`**: Additional feature modules (verification, worktree)
- **`src/error_recovery/`**: Autonomous error recovery system
- **`src/hooks/`**: Instruction hook system

### Key Characteristics
- **Async Architecture**: Tokio-based for high performance
- **Memory Efficient**: Lazy loading and pagination for large codebases
- **Extensible**: Plugin system for additional languages and tools
- **Type Safe**: Rust's strong typing prevents common errors
- **Cross-Platform**: Works on Linux, macOS, Windows

## 🤝 Contributing

1. Fork the repository
2. Create a feature branch
3. Implement changes with tests
4. Ensure `cargo fmt` and `cargo clippy` pass
5. Submit a pull request

## 📄 License

This project is licensed under the MIT License.

## 🙏 Acknowledgments

- tree-sitter for excellent parsing capabilities
- Claude Desktop team for MCP specification
- Rust community for excellent tools
- All contributors and testers

---

**Note**: This is an active research project. Features may change as we explore the boundaries of AI-supported development.
