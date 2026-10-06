# Tool Output Contract

Single source of truth for what a tool returns to the LLM. Every tool handler
under `src/llm/tool_execution/dispatch/tools.rs` must conform to this contract.
`AGENTS.md` links here instead of duplicating the details.

## Tool registration

For a default LLM tool, all sites must agree:

1. Implement the execution entry point and `tool_def()` in `src/tools/<name>.rs`,
   and wire the module in `src/tools/mod.rs`.
2. Register the schema factory in `src/llm/tool_def.rs::default_tools_def`.
3. Add the dispatch arm in `src/llm/tool_execution/dispatch.rs` and the handler
   in `src/llm/tool_execution/dispatch/tools.rs` (or its owning dispatch module).
4. Return `ToolOutput { value, is_success, result_summary }` from the handler.
5. Add isolated success/error tests and dispatch regressions when routing changes.
6. Document the tool in the README tool list.

`python3 scripts/check-agent-guidance.py` checks static schema/dispatch/README
parity. Runtime discovery schemas, internal dispatch, and README-only commands
are explicitly classified in [routing exceptions](ai/tool-routing-exceptions.json).
The check does not replace behavioral dispatch/output tests.

## Handler return shape

Handlers return `ToolOutput { value, is_success, result_summary }` — never a
bare string (`src/llm/tool_execution/dispatch.rs`). `value` is serialized to
JSON by the agent loop and handed to the LLM.

## Global truncation

`src/llm/message_utils.rs::truncate_tool_output` caps the **serialized JSON** of
every tool result before it enters the conversation:

| Tier | Limit | Tools |
|---|---|---|
| Read tier | 40,000 chars | `fs_read`, `fs_read_many_files`, `plan_read` |
| Default tier | 8,000 chars | everything else (incl. `plan_write`, `search_repomap`, `fs_list`, `find_file`, memory tools, `edit`, `apply_patch`, `execute_process`, `execute_bash`, `execute_shell`) |

`plan_write` returns only a compact change summary (never the full plan); the
40,000-char read tier is reserved for explicit full-state reads such as
`plan_read`. Use `plan_read` for the full canonical plan.

Truncation in `truncate_tool_output` is **JSON-safe**: oversized outputs are
parsed and their largest string fields are shortened (head-biased) and array
tails are dropped before re-serialization, so the model always receives
parseable JSON. Non-JSON payloads are wrapped in a
`{"truncated_raw": ..., "note": ...}` envelope. Even so:

> The tool itself is responsible for keeping its output under the cap.
> The global truncator is a safety net, not the budgeting mechanism.

## Structured response conventions

Returning-tool responses should be structured JSON with:

- `warnings` — non-fatal notes the LLM should read (budget applied, results
  omitted, etc.)
- `next_cursor` — continuation token; non-`null` means "more results exist".
  The LLM fetches the next page by re-issuing the same call with `cursor=<value>`
- `applied_budget` (optional) — summary of the limits actually applied

### Budgeting

Large-output tools should accept a `response_budget_chars` parameter (an upper
limit like `5000`) and automatically downscale limit/count/snippet size to stay
under it. Reference implementation: `src/tools/read.rs`
(`fs_read`: summary mode defaults to 400 lines / 6,000 chars). Read budgets count Unicode scalar characters. `fs_read` returns complete lines and resumes at the first unread line; a first line that cannot fit fails explicitly without advancing. `fs_read_many_files` resumes from the actual unread path index and labels shortened snippets as file summaries. Both measure the complete serialized `{ok,result}` envelope, including escaping and metadata, against the shared 40,000-character read cap before dispatch; pagination metadata must survive the global fallback unchanged. See also
`src/tools/read_many.rs`, `src/tools/list.rs`,
`src/tools/search_repomap/repomap/repomap_filter.rs`.

For `fs_read`, `start_line` and `cursor` are aliases for the same 1-based position.
If both are supplied, they must be equal; conflicting values fail explicitly.
To continue a page, supply the returned `next_cursor` as `cursor` and omit the
previous `start_line` (or update both to the same value).

Tools without a `response_budget_chars` parameter must still self-budget using
the helpers in `src/tools/budget.rs` (`head_tail_truncate` for command output,
`head_truncate` for diffs/summaries; `DEFAULT_TOOL_BUDGET_CHARS` = 6,000):

| Tool | Self-budget |
|---|---|
| `execute_process` | stdout+stderr combined 6,000 chars, head+tail preserved (`output_truncated` + `warnings`); bounded capture (32KB head + 32KB tail per stream) so RAM stays flat; classified verification additionally returns a compact (<1000 chars) `verification_workspace` summary, with command success unchanged |
| `execute_bash` / `execute_shell` | stdout+stderr combined 6,000 chars, head+tail preserved (`output_truncated` + `warnings`) |
| trusted `/test` / `/lint` diagnostics | raw managed capture is bounded per stream, then failure parsing runs before an internal ~32,000-character head+tail diagnostic budget; this is intentionally separate from the LLM 6,000-character tool budget |
| `search_text` | `response_budget_chars` (default 6,000), per-match text capped at 500 chars; complete JSON records within 1 MiB total stdout (newlines counted), bounded stderr diagnostic prefix, explicit process/record errors; byte cap before any requested row is an error without a resumable offset |
| `apply_patch` | unified diff only (no full-content echo), diff capped at 6,000 chars |
| `edit` | diff capped at 6,000 chars (`diff_truncated` flag) |
| `find_file` | Whole serialized JSON budget (default/cap 6,000 chars), at most 200 complete paths; stable sort/dedup, 0-based `cursor` and `next_cursor`, `returned`, `total_matches`, `truncated`, `applied_budget_chars`, `warnings`; one-path budget failure is an error. MCP emits the same compact JSON. |
| `fs_list` | budget 6,000 chars incl. per-entry JSON overhead; budget cuts resume at `cursor + entries.len()` (no skipped entries) |
| `read_memory` | content capped at 6,000 chars |
| `task` | summary capped at 4,000 chars; serialized output capped at 6,000 chars, preserving status/reason and marking omitted paths |

Git diff review collection is fail-closed: a timed-out or capture-truncated
Git command returns an error instead of presenting a partial patch for approval
or revert.

## Known gaps / follow-ups (as of this writing)

The contract above is the target. Current code deviates in the following ways —
fix these before generalizing the contract to more tools:

1. **Pagination conventions are inconsistent.** Three coexist:
   - `fs_read`: `next_cursor`, 1-based
   - `search_repomap`: `next_cursor`, 0-based
   - `search_text`: `next_offset`, 0-based (now with `warnings` and
     `response_budget_chars`)

   New tools must use `next_cursor` + `response_budget_chars`. Unifying the
   existing three is an open backlog item.
2. **`search_history` is an internal-only legacy route.** It has a dispatch arm ("search_history"
   in `src/llm/tool_execution/dispatch.rs`) and a handler
   (`search_history` in `src/llm/tool_execution/dispatch/tools.rs`) but is not
   registered in `default_tools_def` (`src/llm/tool_def.rs`), so the LLM can
   never discover it from the default schema inventory. This is recorded as a
   routing exception; exposing or removing it is a separate behavior decision.

Resolved gaps (kept here for history): `apply_patch` now returns only the diff
plus line statistics (no `original_content`/`modified_content` echo), and
truncation is JSON-safe (`truncate_tool_output` budgets string fields in-place).

## `plan_write` result contract

`plan_write` returns a compact mutation acknowledgement, never the full plan:

```json
{
  "ok": true,
  "changed": true,
  "delta": {
    "added_ids": [],
    "updated_ids": ["step-2"],
    "removed_ids": []
  },
  "item_count": 5,
  "status_counts": {
    "pending": 3,
    "in_progress": 1,
    "completed": 1
  },
  "warnings": []
}
```

- `ok`: always `true` on success.
- `changed`: top-level no-op signal (`false` when the write was a no-op).
  Loop detection and the task sentinel key off this field; never move it.
- `delta`: `added_ids` (new since the previous snapshot), `updated_ids`
  (same id, changed content/status/parent/requirement-links/obligations),
  `removed_ids` (dropped, mainly via `mode="replace"`). Empty arrays are
  omitted; a no-op yields `"delta": {}`.
- `item_count` / `status_counts`: final totals after the write.
- `warnings`: provenance / verification / requirement-link warnings the LLM
  must read. Budgeted to ~6,000 chars total (warnings only are reduced;
  `delta` and counts are never truncated). When reduced, the result carries
  `"warnings_truncated": true` plus the original `"warning_count"`.
- Never present: `plan`, `items`, `session_id`, full item content,
  obligations, or requirement links. Call `plan_read` for full state.

## `execute_process` result shape

`execute_process` returns a structured `ProcessResult` (all in the default
8,000-char tier):

```json
{
  "ok": true,
  "success": true,
  "status": "completed",
  "exit_code": 0,
  "stdout": "...",
  "stderr": "...",
  "output_truncated": false,
  "warnings": []
}
```

- `status`: `completed` | `timed_out` | `policy_denied` | `spawn_failed`
  (snake_case). Exit non-zero is `completed` with `success: false`;
  only policy/timeout/spawn failures change `status`.
- Invariant: `ok == success`. Policy denials, timeouts, and spawn failures
  all return `ok: false` / `success: false` / `is_success: false` with a
  structured `error` message (never a bare `Err` with no LLM-visible reason).
- Cancellation is NOT a normal result: it propagates as
  `LlmErrorKind::Cancelled` to the agent loop after terminating the process
  tree.
- Environment variable values are never echoed in results, summaries, or logs.

## Managed process diagnostics (`/test`, `/lint`)

`execute_process`, `execute_bash`, and `execute_shell` are LLM-facing tools.
They apply `ExecutionPolicy` where appropriate and self-budget their returned
JSON to the 6,000-character command budget. Their low-level mechanics are
provided by the policy-free `src/execution/runner.rs`.

The user-invoked `/test` and `/lint` slash commands are trusted internal
commands. They reuse the same runner for streaming bounded capture,
timeout/cancellation, process-group cleanup, and reaping, but they do not
inherit the LLM program's allowlist. Their raw capture is parsed first so
failure detection is not degraded by the LLM budget; only the diagnostic text
sent to the LLM/UI is then capped at the internal diagnostic budget.

## Sub-agent (`task` tool)

The `task` tool runs an isolated agent loop
(`src/llm/tool_execution/subagent.rs`) restricted to the read-only tool subset
(`SUBAGENT_ALLOWED_TOOLS`) with its own iteration bound (40) and a 4,000-char
summary budget. Tool traffic inside the sub-agent never enters the main
conversation; only `{ok, summary, files_examined, iterations, tool_calls}` is
returned. Registration follows the same checklist as any other tool.

## Remote MCP result contract

Remote MCP calls are normalized in `src/tools/remote_tools.rs` after the
`src/mcp/client.rs` protocol boundary. A JSON-RPC/transport success is not a
tool success: for a completed `CallToolResult`, `is_error == Some(true)` maps
to `ok: false` and `ToolOutput.is_success == false`; `Some(false)` and the
legacy absent field (`None`) map to success.

A normalized result has this shape (available fields are retained):

```json
{
  "ok": true,
  "server": "github",
  "tool": "create_issue",
  "is_error": false,
  "content": [],
  "structured_content": { "id": 123 },
  "warnings": []
}
```

`content` preserves MCP text/image/audio/resource blocks as JSON;
`structured_content` preserves the server's arbitrary structured JSON. The
raw `CallToolResult` envelope is not embedded a second time. Remote output is
self-budgeted with JSON-safe truncation before it enters the LLM context, and
`result_summary` is independently bounded. Input-required and Task responses
are explicit unsupported tool results; protocol, transport, timeout, and
cancellation errors do not masquerade as completed tool results.

Remote MCP arguments, full results, environment values, and credentials are
never logged verbatim. Structured stdio passes `command` and `args` directly
to the child process; it never invokes a shell or parses a new command line.

## `tool_search` result contract
`tool_search` discovers deferred tools without ever echoing full schemas:

```json
{
  "ok": true,
  "query": "github pull request review",
  "activated": [
    {"name": "mcp_github_get_pull_request", "source": "mcp", "server": "github", "description": "..."}
  ],
  "already_active": [],
  "remaining_deferred": 42,
  "warnings": []
}
```

- The result is structured JSON (`ok`, `query`, `activated`,
  `already_active`, `remaining_deferred`, `warnings`).
- Full tool schemas are never echoed; schemas ship only in the next LLM
  request's `tools` array.
- Default result limit = configured `search_result_limit` (default 5);
  hard max = 10 (`limit == 0` or omitted falls back to the default).
- Per-hit descriptions are bounded (~250 chars); the whole response is
  budgeted (<= 4,000 chars, far below the default 8,000-char tier).
- Ranking always uses the full query, but only a bounded prefix (~500 chars)
  of the query is echoed in the response so a pathological query cannot blow
  the response budget.
- Activated tools become available on the next LLM iteration; activation
  is sticky for the current `ToolRuntime` / agent run.
- Inactive matches receive activation capacity independently of
  already-active matches: the `limit` bounds inactive activation, while
  already-active matches are reported separately under the same independent
  bound and can never crowd out a relevant inactive match.
- `remaining_deferred` counts only real inactive tools and always excludes
  managed `tool_search`, in both eager and deferred modes.
- The final real-tool activation retires `tool_search` from the next
  LLM-visible schema set. A stale or guessed `tool_search` call after
  retirement fails closed with `error.kind == "tool_not_active"`
  (`is_success: false`) and no side effects.
- A search that activates nothing (no matches, or only already-active
  matches) is reported with a warning and counts as no task progress; only
  real activation counts as progress.
- A guessed deferred tool name never executes: dispatch fails closed with
  `error.kind == "tool_not_active"` (`is_success: false`) before any side
  effect, including remote MCP calls. Unknown tools remain "unknown tool"
  errors.

## Observation Store (`obs-*`)

Large historical tool results are offloaded recoverably via
`src/llm/observation.rs` (`HistoryManager::offload_stale_tool_results`).
The stub `(offloaded tool result: <tool>, <N> B, observation obs-000001; use
observation_read if needed)` is recoverable with `observation_read`; the
legacy `[cleared tool result; ...]` stub is only a store-full fallback and is
never used for preflight reductions.

- Unseen results are protected from both offloading and conversation
  compaction until a successful provider request has consumed them
  (`HistoryManager::mark_sent_tool_results_seen`). Failed requests
  (timeout, rate limit, server error, disconnect, cancellation,
  `context_length_exceeded`, deserialize failure) leave results unseen.
- Conversation compaction is always unseen-safe: the oldest assistant
  `tool_calls` message holding an unseen result (parallel batches kept as a
  unit) starts an exact protected suffix that the summarizer never sees.
  Unresolvable unseen ids fail closed (no compaction).
- Preflight order is overlay drop, then recoverable offload, then
  unseen-safe compaction as a last resort. Preflight never evicts `obs-*`
  entries or deactivates tools.

- `observation_read` budgets its complete serialized response, including escaping,
  metadata and warnings. A shorter page ends at a UTF-8 boundary and its
  `end_byte`/`next_cursor` track the bytes actually returned. Search dispatch
  likewise budgets its final envelope and advances `next_offset` only past
  returned rows. Neither paged output relies on global truncation for a valid
  continuation. If metadata plus one character/row cannot fit, return an error
  without claiming progress.

## Tool registration checklist

Every tool must be consistent at all sites; a missing site silently breaks the
tool:

1. `src/tools/<name>.rs` — `tool_def()` + execution entry point
2. `src/llm/tool_def.rs` — registered in `default_tools_def`
3. `src/llm/tool_execution/dispatch.rs` — match arm
4. `src/llm/tool_execution/dispatch/tools.rs` — handler
5. Tests next to the implementation (and dispatch-level if relevant)
6. `README.md` — tool list entry

`tool_search` is the deliberate exception to step 2: its schema is
catalog-managed (`src/llm/tool_catalog.rs`) and only advertised while
deferred tools remain, so eager mode stays byte-identical to the legacy
inventory. All other steps apply to it.

`provenance_read` verification rows include compact `execution_workspace` and
`current_code_state` summaries. Full endpoint file arrays belong in session
evidence reports, never tool responses. Historical coverage does not establish
current code correspondence.
