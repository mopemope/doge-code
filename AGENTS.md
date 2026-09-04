# Repository Guidelines

Guidelines for AI coding agents working on doge-code (an AI coding agent itself, binary `dgc`). User-facing feature docs live in `README.md`; this file focuses on how to change the codebase correctly.

## Verification Commands

Run the narrowest check first, then broaden:

- `cargo test <module_or_name>` — run matching tests only (fast feedback). Full `cargo test` (370+ tests) is slow; run it only before finishing.
- `cargo clippy --all-targets --all-features` — must produce zero warnings. This is a merge gate (CI enforces `-D warnings`).
- `cargo fmt --all` — format before finishing; CI runs `cargo fmt --check`.
- `cargo run --release -- <flags>` — launch the TUI agent for manual verification of TUI changes.

## Module Map

| Path | Responsibility |
|---|---|
| `src/main.rs` | CLI entry point (clap), mode wiring |
| `src/exec.rs` | Non-interactive `exec` orchestration |
| `src/llm/` | OpenAI-compatible client, agent loop, tool dispatch |
| `src/llm/tool_def.rs` | Registry of tools exposed to the LLM (`default_tools_def`) |
| `src/llm/tool_execution/agent_loop.rs` | Main agent loop: iteration, loop detection, compaction triggers |
| `src/llm/tool_execution/dispatch.rs` | Tool call dispatch (one arm per tool) |
| `src/llm/tool_execution/dispatch/tools.rs` | Tool call handlers (one fn per tool) |
| `src/llm/tool_execution/history.rs` | Conversation compaction (proactive + reactive), stale tool-result clearing |
| `src/llm/message_utils.rs` | Global tool-output truncation caps (see Tool Output Conventions) |
| `src/llm/tool_runtime.rs` | Shared runtime handles; `MAX_ITERS` loop bound (256) |
| `src/llm/tool_execution/subagent.rs` | `task` sub-agent loop (read-only, isolated context) |
| `src/tools/` | Tool implementations (each file exposes a `tool_def()`); `budget.rs` for output budgets |
| `src/analysis/` | tree-sitter parsing, symbol extraction, RepoMap, SQLite DAO, `loop_detector.rs`, `task_sentinel.rs` |
| `src/tui/` | ratatui TUI; slash commands under `src/tui/commands/` |
| `src/session/` | SQLite session persistence (SeaORM) |
| `src/mcp/` | MCP server (rmcp) + client for remote MCP tools |
| `src/config/` | AppConfig, `.doge/config.toml` loading |
| `src/features/` | `testing.rs` (/test), `workflow.rs` (CLI run), `doc_skill/`, `worktree_manager.rs` |
| `src/watch.rs` | File watch mode (`dgc watch`) |
| `src/error_recovery/` | Autonomous error recovery hints |
| `src/hooks/` | Post-instruction hook system (repomap updates) |
| `resources/system_prompt.md` | System prompt template (Tera, rust-embed) |
| `elisp/` | Emacs integration (outside CI; see `elisp/emacs-integration.md`) |

## Common Change Patterns

### Adding a new tool (all steps required)

1. Implement `src/tools/<name>.rs` with a `tool_def()` returning `ToolDef` and an execution entry point.
2. Register the `tool_def()` in `src/llm/tool_def.rs` (`default_tools_def`).
3. Add a dispatch arm in `src/llm/tool_execution/dispatch.rs` and a handler in `src/llm/tool_execution/dispatch/tools.rs`.
4. Handler must return `ToolOutput { value, is_success, result_summary }` — never a bare string.
5. Add tests next to the implementation and, if dispatch-relevant, in `dispatch.rs::tests`.
6. Document the tool in `README.md`.

Skipping step 3 leaves a tool that the LLM can call but that fails with "unknown tool" — check every registration site. The full checklist lives in `docs/tool-output-contract.md`.

### Changing the system prompt

Edit `resources/system_prompt.md` (Tera template: `{{ os }}`, `{{ project_dir }}`). It is embedded at compile time via rust-embed (`src/assets.rs`) — rebuild to pick up changes. Project instructions files (`AGENTS.md` / `QWEN.md` / `GEMINI.md`, or `project_instructions_file` config key) are appended at runtime (`src/tui/commands/prompt.rs`).

### RepoMap / analysis changes

Symbol extraction lives in per-language collectors under `src/analysis/` (e.g. `rust_collector.rs`). Query-side budget/density logic is in `src/tools/search_repomap/repomap/repomap_filter.rs`. Tests for analysis live in `src/analysis/tests.rs`.

### Adding a TUI slash command

1. Implement the handler in `src/tui/commands/handlers/slash_commands/<name>.rs` (see existing files; `help.rs` owns the help listing).
2. Register the command in the slash-command dispatch there (`mod.rs` wires handlers).
3. Document it in the README slash-command table.

## Coding Style

- Rust Edition 2024, four-space indentation (`rustfmt.toml`).
- `snake_case` functions/modules, `CamelCase` types, `SCREAMING_SNAKE_CASE` constants.
- Prefer `tracing` spans/macros over `println!`/ad-hoc logging.
- Replace `unwrap()`/`expect()` in production paths with `?`/typed errors (`anyhow` + `thiserror`); `expect()` with a message is acceptable in tests.
- Feature-gated code belongs under `src/features/`.

## Tool Output Conventions (token efficiency)

These are hard requirements — the LLM consumes tool output directly. Full spec: `docs/tool-output-contract.md`.

- Returning-tool responses are structured JSON with `warnings` and `next_cursor` for pagination; never return unbounded text.
- Large outputs (file reads, listings, repomap results) must support summary mode + `response_budget_chars` budgeting. Follow the pattern in `src/tools/read.rs`.
- Global caps (`src/llm/message_utils.rs`): 8,000 chars default, 40,000 chars only for `fs_read`, `fs_read_many_files`, `plan_write`, `plan_read`. Everything else — including `search_repomap`, `fs_list`, `find_file`, memory tools, `edit`, `apply_patch`, bash/shell — is 8,000.
- The global truncator is JSON-safe (it budgets string fields in-place rather than slicing the serialized payload), but it is a safety net only: tools must keep their own output under the cap using `src/tools/budget.rs` helpers (bash/shell 6k head+tail, `apply_patch` diff-only, `find_file` 200 paths, `read_memory` 6k) or `response_budget_chars`.

## Testing Guidelines

- Unit tests go beside the code under `#[cfg(test)]`; larger fixtures may use `<name>_test.rs` files (both styles exist).
- Names describe behavior: `test_<behavior>`. Cover success and error paths.
- Dispatch/agent-loop changes need regression tests in `src/llm/tool_execution/dispatch.rs` or `agent_loop.rs` test modules.
- Tests must not touch the real CWD or user config; use `tempdir()` and construct `AppConfig` with an explicit `project_root`.

## Known Pitfalls

- Two workflow formats coexist: `.doge/workflows/*.yml` (shell commands, run by the `run_workflow` tool in `src/tools/workflow.rs`) and `.doge/workflows/*.md` (LLM-executed steps, run by the CLI `run` subcommand via `src/features/workflow.rs`). Keep both in mind; do not "fix" one to match the other without checking callers.
- `fs_read` accepts both `start_line` (legacy) and `cursor` (preferred, 1-based). New code should use `cursor`. Note `search_repomap`'s cursor is 0-based — the two conventions currently differ.
- `.doge/`, `target/`, and agent-generated files (`GEMINI.md`, `QWEN.md`, etc.) are excluded from RepoMap via `.dogeignore`.
- Workflow files and session state under `.doge/` are project-local; never commit them.
- `search_history` has a dispatch arm but is not registered in `default_tools_def` — a live example of the registration-gap pitfall. Check every site listed in `docs/tool-output-contract.md` when adding tools.
- Tests must never write to the repo root (e.g. a `temp/` directory). Leftover test artifacts used to accumulate there. Always use `tempdir()` per the Testing Guidelines.
- The README tool list must stay in sync with `default_tools_def` (`src/llm/tool_def.rs`); new tools require a README entry (checklist step 6).

## Commit & Pull Request Guidelines

- Format: `type(scope): summary` (e.g. `feat(tooling): ...`, `refactor(core): ...`). Keep bodies short.
- PRs must describe user-visible impact and list verification performed (`cargo fmt/clippy/test` output for non-trivial fixes).
- TUI changes: include a screenshot or terminal recording.
- New flags/commands must be documented in `README.md` before review.

## Configuration & Secrets

- Config: environment variables + XDG-compliant TOML. Project overrides go in `.doge/config.toml` (top-level `project_instructions_file`, `[llm]`, `[project]`, `[mcp]`, `[watch]`, `[[mcp_servers]]` — the MCP servers key is an array of tables).
- Never commit API keys; use `OPENAI_API_KEY` or `--api-key` locally.
- Tree-sitter language packs in `resources/tree-sitter-language-pack/` are vendored; update carefully and note version bumps in the PR description.

## CI

GitHub Actions (`.github/workflows/ci.yml`) runs on push/PR: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`. Keep it green.
