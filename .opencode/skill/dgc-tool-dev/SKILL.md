---
name: dgc-tool-dev
description: Use when adding or modifying dgc tools in this Rust repo (src/tools/, tool registration, dispatch arms, tool output budgeting) or when debugging "unknown tool" / unregistered-tool issues. Covers the full registration checklist and output contract.
---

# dgc Tool Development

A dgc tool is callable by the LLM only if it exists at **all** sites below.
A missing site fails silently (tool registered but "unknown tool" at dispatch,
or handler unreachable because unregistered).

## Registration checklist (all steps required)

1. **Implement** `src/tools/<name>.rs`: `tool_def()` returning `ToolDef` + execution entry point.
2. **Register schema** in `default_tools_def` — `src/llm/tool_def.rs`.
3. **Dispatch arm** in `src/llm/tool_execution/dispatch.rs` + **handler** in `src/llm/tool_execution/dispatch/tools.rs`.
4. **Return shape**: handler returns `ToolOutput { value, is_success, result_summary }` — never a bare string.
5. **Tests** next to the implementation; dispatch-relevant changes also in `dispatch.rs::tests`.
6. **README.md** tool list entry (keep in sync with `default_tools_def`).

Live example of a missed step 2: `search_history` has a dispatch arm in `src/llm/tool_execution/dispatch.rs` but no schema registration, so the LLM can never call it.

## Output contract (token efficiency)

Full spec: `docs/tool-output-contract.md`. Hard rules:

- Structured JSON with `warnings` and `next_cursor`; never unbounded text.
- Large outputs need summary mode + `response_budget_chars` budgeting. Reference implementation: `src/tools/read.rs`.
- Global caps (`src/llm/message_utils.rs`): 8,000 chars default; 40,000 only for `fs_read`, `fs_read_many_files`, `plan_write`, `plan_read`. `search_repomap`, `fs_list`, `find_file`, memory tools, `edit`, `apply_patch`, bash/shell are all 8,000.
- The cap slices serialized JSON by characters → over-cap output reaches the LLM as malformed JSON. **Keep your tool's own output under the cap**; never rely on the global truncator.
- New pagination must use `next_cursor` (state the base, 0 or 1, in the schema description). Existing code has mixed conventions (fs_read 1-based, search_repomap 0-based, search_text `next_offset`) — do not copy those without checking.

## Test conventions

- `#[cfg(test)]` beside the code, or `<name>_test.rs` for larger fixtures.
- Name tests `test_<behavior>`; cover success and error paths.
- Tests must use `tempdir()` and an `AppConfig` with explicit `project_root`. Never write to the repo root (`temp/` there is leftover artifact from violations).
