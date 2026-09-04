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

Truncation is a character slice of the serialized JSON inside
`truncate_tool_output` (`src/llm/message_utils.rs`). Because it cuts the JSON
string itself, the LLM can receive **malformed JSON** when a result exceeds the
cap. Therefore:

> The tool itself is responsible for keeping its output under the cap.
> Never rely on the global truncator to make an output fit.

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

## Known gaps / follow-ups (as of this writing)

The contract above is the target. Current code deviates in the following ways —
fix these before generalizing the contract to more tools:

1. **Pagination conventions are inconsistent.** Three coexist:
   - `fs_read`: `next_cursor`, 1-based
   - `search_repomap`: `next_cursor`, 0-based
   - `search_text`: `next_offset`, 0-based, no `warnings`/budget param

   New tools must use `next_cursor` + `response_budget_chars`. Unifying the
   existing three is an open backlog item.
2. **`apply_patch` echoes both `original_content` and `modified_content`**
   (`ApplyPatchResult` in `src/tools/apply_patch.rs`), roughly 2x file size per
   successful patch, and sits in the 8k tier so its JSON is usually truncated
   mid-field. It should return only the diff and metadata.
3. **Truncation is not JSON-safe.** The character slice in
   `truncate_tool_output` (`src/llm/message_utils.rs`) can break JSON
   structure. It should truncate the largest field or emit a well-formed JSON
   envelope.
4. **`search_history` is a ghost tool.** It has a dispatch arm ("search_history"
   in `src/llm/tool_execution/dispatch.rs`) and a handler
   (`search_history` in `src/llm/tool_execution/dispatch/tools.rs`) but is not
   registered in `default_tools_def` (`src/llm/tool_def.rs`), so the LLM can
   never call it. Either register it in the schema list or delete both sides.

## Tool registration checklist

Every tool must be consistent at all sites; a missing site silently breaks the
tool:

1. `src/tools/<name>.rs` — `tool_def()` + execution entry point
2. `src/llm/tool_def.rs` — registered in `default_tools_def`
3. `src/llm/tool_execution/dispatch.rs` — match arm
4. `src/llm/tool_execution/dispatch/tools.rs` — handler
5. Tests next to the implementation (and dispatch-level if relevant)
6. `README.md` — tool list entry
