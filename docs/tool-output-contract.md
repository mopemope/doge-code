# Tool Output Contract

Single source of truth for what a tool returns to the LLM. Every tool handler
under `src/llm/tool_execution/dispatch/tools.rs` must conform to this contract.
`AGENTS.md` links here instead of duplicating the details.

## Handler return shape

Handlers return `ToolOutput { value, is_success, result_summary }` — never a
bare string (`src/llm/tool_execution/dispatch.rs`). `value` is serialized to
JSON by the agent loop and handed to the LLM.

## Global truncation

`src/llm/message_utils.rs::truncate_tool_output` caps the **serialized JSON** of
every tool result before it enters the conversation:

| Tier | Limit | Tools |
|---|---|---|
| Read tier | 40,000 chars | `fs_read`, `fs_read_many_files`, `plan_write`, `plan_read` |
| Default tier | 8,000 chars | everything else (incl. `search_repomap`, `fs_list`, `find_file`, memory tools, `edit`, `apply_patch`, `execute_bash`, `execute_shell`) |

Note: plan tools sit in the read tier even though `plan_write` echoes data back;
this is historical.

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
(`fs_read`: summary mode defaults to 400 lines / 6,000 chars). See also
`src/tools/read_many.rs`, `src/tools/list.rs`,
`src/tools/search_repomap/repomap/repomap_filter.rs`.

Tools without a `response_budget_chars` parameter must still self-budget using
the helpers in `src/tools/budget.rs` (`head_tail_truncate` for command output,
`head_truncate` for diffs/summaries; `DEFAULT_TOOL_BUDGET_CHARS` = 6,000):

| Tool | Self-budget |
|---|---|
| `execute_bash` / `execute_shell` | stdout+stderr combined 6,000 chars, head+tail preserved (`output_truncated` + `warnings`) |
| `search_text` | `response_budget_chars` (default 6,000), per-match text capped at 500 chars |
| `apply_patch` | unified diff only (no full-content echo), diff capped at 6,000 chars |
| `edit` | diff capped at 6,000 chars (`diff_truncated` flag) |
| `find_file` | 200 paths per response, `total_matches` + `truncated` for overflow |
| `fs_list` | budget 6,000 chars incl. per-entry JSON overhead; budget cuts resume at `cursor + entries.len()` (no skipped entries) |
| `read_memory` | content capped at 6,000 chars |
| `task` | sub-agent summary capped at 4,000 chars |

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
2. **`search_history` is a ghost tool.** It has a dispatch arm ("search_history"
   in `src/llm/tool_execution/dispatch.rs`) and a handler
   (`search_history` in `src/llm/tool_execution/dispatch/tools.rs`) but is not
   registered in `default_tools_def` (`src/llm/tool_def.rs`), so the LLM can
   never call it. Either register it in the schema list or delete both sides.

Resolved gaps (kept here for history): `apply_patch` now returns only the diff
plus line statistics (no `original_content`/`modified_content` echo), and
truncation is JSON-safe (`truncate_tool_output` budgets string fields in-place).

## Sub-agent (`task` tool)

The `task` tool runs an isolated agent loop
(`src/llm/tool_execution/subagent.rs`) restricted to the read-only tool subset
(`SUBAGENT_ALLOWED_TOOLS`) with its own iteration bound (40) and a 4,000-char
summary budget. Tool traffic inside the sub-agent never enters the main
conversation; only `{ok, summary, files_examined, iterations, tool_calls}` is
returned. Registration follows the same checklist as any other tool.

## Tool registration checklist

Every tool must be consistent at all sites; a missing site silently breaks the
tool:

1. `src/tools/<name>.rs` — `tool_def()` + execution entry point
2. `src/llm/tool_def.rs` — registered in `default_tools_def`
3. `src/llm/tool_execution/dispatch.rs` — match arm
4. `src/llm/tool_execution/dispatch/tools.rs` — handler
5. Tests next to the implementation (and dispatch-level if relevant)
6. `README.md` — tool list entry
