# Audit correctness and TUI boundaries implementation plan

> For agentic workers: execute task by task using test-driven development, then request one independent whole-branch review.

**Goal:** Fix the six reproduced develop audit defects without changing billing routes, mutation ownership, execution policy, or canonical session recovery.

**Architecture:** Keep edit and provider fixes at their existing boundaries. Give input history a bounded private store independent of canonical sessions. Aggregate PTY bytes outside the general UI inbox, then decode and cap both retained output and request-scoped context.

**Tech Stack:** Rust Edition 2024, existing dependencies, MSRV 1.94, Tokio, Crossterm, portable-pty.

**Spec:** The authorized six-finding investigation and follow-up request; evidence remains in the task-local `../audit/` directory. Refresh develop/PR/CI at execution time; do not pin implementation to an old commit.

## Global constraints

- No live paid inference, real credentials, untrusted scripts, unrelated changes, develop push, force push or merge.
- Preserve atomic mutation receipts, race rejection, undo/provenance and unseen-result recovery.
- Preserve existing prompt/history/provider bindings and direct-user shell policy distinction.
- Tests use owned temporary fixtures and explicit project roots. No dependency changes.
- Publish only the feature branch with an explicit full refspec, after push configuration inspection and porcelain dry-run; verify remote head and develop before/after.

## Review focus

- Inline LF/CRLF/Unicode matches and idempotency must respect the requested starting line (Task 1).
- Parent/file symlinks, FIFO, oversized history, write failure and existing permissions must preserve unrelated bytes (Task 2).
- Arbitrary string/object error data must not become secrets in Display, Debug or logged errors; known classifications stay useful (Task 3).
- Slow subprocess startup must not turn timeout recovery coverage into a timing lottery (Task 4).
- Newline-free PTY floods, bounded draining, every UTF-8 split and EOF must preserve input responsiveness and valid text (Tasks 5–6).

### Task 1: Edit line ranges

Files: `src/tools/edit.rs` tests and both match-position calculations.
- [ ] Add real edit-with-receipt regressions for inline matches, range 1/2, LF/CRLF/Unicode, reported candidates and out-of-scope idempotency. Observe RED.
- [ ] Count preceding LF bytes plus one through one shared helper; retain existing match-start range semantics and commit path.
- [ ] Run `bash scripts/verify.sh test tools::edit::`, inspect receipts/content and commit the coherent fix.

### Task 2: Input history store

Files: new `src/features/input_history.rs`, feature registration, `src/tui/state.rs`, `src/tui/event_handlers/normal.rs`, README.
Interface: `InputHistoryStore::at(project_root)`, `load() -> Result<Vec<String>>`, `save(&[String]) -> Result<()>`; UI retains project-root identity and surfaces failures without losing submitted input.
- [ ] Add regressions reproducing linked-file overwrite, unsafe parent/FIFO, size/entry bounds, private new files and old-content preservation. Observe RED at the public store boundary.
- [ ] Cap serialized history at 1 MiB and 1,000 entries; bounded regular-file reads. On Unix hold nofollow directory descriptors, create 0600 exclusive siblings and rename relative to the held directory. Reject unsafe/foreign/multiply linked targets. On other platforms use explicit path checks and document trusted-directory limitation.
- [ ] Load/save only the selected project root, never test process CWD; report safe structural warnings, preserve in-memory input and old history on failure.
- [ ] Run focused store/input tests; document limits and failure behavior; commit.

### Task 3: Provider diagnostics

Files: `src/features/openai_subscription/mod.rs`, related provider tests.
- [ ] Add sentinel regressions for string error/top-level string and object fields; observe RED.
- [ ] Discard free-text string errors; preserve only recognized machine codes/types/params and sanitized request identifiers. Keep known quota, context-length and FreeTierError classifications.
- [ ] Run provider focused tests including mock HTTP failures and assert Display/Debug/log privacy; commit.

### Task 4: Deterministic timeout recovery coverage

Files: `src/tui/commands/handlers/slash_commands/test.rs`, `test_start_recovery_tests.rs`, narrowly scoped test support if needed.
- [ ] Preserve the observed failing 100ms regression evidence. Separate the recovery decision from OS startup timing via an internal result-processing boundary or a test-only started-process synchronization seam.
- [ ] Feed a real timed-out `TestResult` with retained marker through the same recovery path, assert staged follow-up, timeout label, marker and no success claim. Keep runner integration timeout/partial-output tests and startup-failure exclusion.
- [ ] Run test-start-recovery and runner focused checks, including repeated runs; commit without weakening assertions or raising timeouts as the fix.

### Task 5: Bounded PTY output

Files: `src/tui/shell.rs`, `state.rs`, `event_loop.rs`, `commands/agent_job.rs`, README.
Interface: shell session owns a fixed-capacity byte mailbox; UI drains it once per tick into a bounded tail buffer. General inbox is not the PTY data transport.
- [ ] Add finite 10MiB/newline-free and flood regressions; observe missing capacity invariant RED.
- [ ] Aggregate shell bytes in a mutex-protected 64 KiB mailbox (retain newest bytes with explicit truncation state), eliminate per-chunk unbounded general-channel sends, cap retained output at 64 KiB, and independently cap shell model context at 16 KiB and 50 lines including truncation notice.
- [ ] Bound general inbox drain to 128 messages per tick. Preserve shell startup/status behavior, cancellation ownership, and other job messages; avoid extending lifecycle scope speculatively.
- [ ] Run shell/context/event-loop focused tests; document retention; commit.

### Task 6: Streaming UTF-8

Files: shell output decoder at the Task 5 byte boundary and tests.
- [ ] Test Japanese/emoji at every split, invalid sequences, dropped prefix after mailbox overflow, and incomplete EOF; observe RED.
- [ ] Carry incomplete suffix bytes (at most three), replace genuinely invalid sequences, flush at EOF, and reset partial decoder state when bytes were dropped. Keep retained buffer UTF-8 boundaries intact.
- [ ] Run focused tests and commit.

### Final validation and publication

- [ ] Self-review requirement coverage and shared interfaces before runtime edits; record any rulings in task-local audit notes.
- [ ] `cargo fmt --all`; `bash scripts/verify.sh rust`, `guidance`, `tui-deps`, native `macos` focused, and MSRV if affected. Run manual isolated PTY smoke and preserve a terminal recording.
- [ ] Independent whole-branch review; reproduce and fix material findings; repeat affected gates.
- [ ] Inspect effective push URL/refspecs/upstream/defaults/pushRemote with secret-safe output. Dry-run only `refs/heads/fix/audit-correctness-and-tui-bounds:refs/heads/fix/audit-correctness-and-tui-bounds`, reject other destinations, then publish that exact mapping without `-u`/force.
- [ ] Verify exact feature head and unchanged develop; create PR against develop, attach it to this task and monitor exact-head CI to completion. No merge.

## Plan self-review

All six authorized defects map to tasks with regression gates. Tasks 5–6 share the byte mailbox/decoder boundary and run sequentially. Other tasks share no runtime interfaces. Task 2 owns persistence only, not canonical sessions; Task 4 changes testability, not production timeout policy. Existing isolation and known baseline test failure were revalidated; user explicitly authorized fixing it. Proceed without another approval gate because the follow-up request authorizes implementation and PR delivery.
