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
| `src/tools/` | Tool implementations (each file exposes a `tool_def()`) |
| `src/analysis/` | tree-sitter parsing, symbol extraction, RepoMap, SQLite DAO |
| `src/tui/` | ratatui TUI; slash commands under `src/tui/commands/` |
| `src/session/` | SQLite session persistence (SeaORM) |
| `src/mcp/` | MCP server (rmcp) + client for remote MCP tools |
| `src/config/` | AppConfig, `.doge/config.toml` loading |
| `src/features/` | `testing.rs` (/test), `workflow.rs` (CLI run), `doc_skill/`, `worktree_manager.rs` |
| `resources/system_prompt.md` | System prompt template (Tera, rust-embed) |

## Common Change Patterns

### Adding a new tool (all steps required)

1. Implement `src/tools/<name>.rs` with a `tool_def()` returning `ToolDef` and an execution entry point.
2. Register the `tool_def()` in `src/llm/tool_def.rs` (`default_tools_def`).
3. Add a dispatch arm in `src/llm/tool_execution/dispatch.rs` and a handler in `src/llm/tool_execution/dispatch/tools.rs`.
4. Handler must return `ToolOutput { value, is_success, result_summary }` — never a bare string.
5. Add tests next to the implementation and, if dispatch-relevant, in `dispatch.rs::tests`.
6. Document the tool in `README.md`.

Skipping step 3 leaves a tool that the LLM can call but that fails with "unknown tool" — check every registration site.

### Changing the system prompt

Edit `resources/system_prompt.md` (Tera template: `{{ os }}`, `{{ project_dir }}`). It is embedded at compile time via rust-embed (`src/assets.rs`) — rebuild to pick up changes. Project instructions files (`AGENTS.md` / `QWEN.md` / `GEMINI.md`, or `project_instructions_file` config key) are appended at runtime (`src/tui/commands/prompt.rs`).

### RepoMap / analysis changes

Symbol extraction lives in per-language collectors under `src/analysis/` (e.g. `rust_collector.rs`). Query-side budget/density logic is in `src/tools/search_repomap/repomap/repomap_filter.rs`. Tests for analysis live in `src/analysis/tests.rs`.

## Coding Style

- Rust Edition 2024, four-space indentation (`rustfmt.toml`).
- `snake_case` functions/modules, `CamelCase` types, `SCREAMING_SNAKE_CASE` constants.
- Prefer `tracing` spans/macros over `println!`/ad-hoc logging.
- Replace `unwrap()`/`expect()` in production paths with `?`/typed errors (`anyhow` + `thiserror`); `expect()` with a message is acceptable in tests.
- Feature-gated code belongs under `src/features/`.

## Tool Output Conventions (token efficiency)

These are hard requirements — the LLM consumes tool output directly:

- Returning-tool responses are structured JSON with `warnings` and `next_cursor` for pagination; never return unbounded text.
- Large outputs (file reads, listings, repomap results) must support summary mode + `response_budget_chars` budgeting. Follow the pattern in `src/tools/read.rs`.
- Non-fs tools (bash/shell) are truncated to 8,000 chars, fs/plan tools to 40,000 chars (`src/llm/message_utils.rs`).

## Testing Guidelines

- Unit tests go beside the code under `#[cfg(test)]`; larger fixtures may use `<name>_test.rs` files (both styles exist).
- Names describe behavior: `test_<behavior>`. Cover success and error paths.
- Dispatch/agent-loop changes need regression tests in `src/llm/tool_execution/dispatch.rs` or `agent_loop.rs` test modules.
- Tests must not touch the real CWD or user config; use `tempdir()` and construct `AppConfig` with an explicit `project_root`.

## Known Pitfalls

- Two workflow formats coexist: `.doge/workflows/*.yml` (shell commands, run by the `run_workflow` tool in `src/tools/workflow.rs`) and `.doge/workflows/*.md` (LLM-executed steps, run by the CLI `run` subcommand via `src/features/workflow.rs`). Keep both in mind; do not "fix" one to match the other without checking callers.
- `fs_read` accepts both `start_line` (legacy) and `cursor` (preferred, 1-based). New code should use `cursor`.
- `.doge/`, `target/`, and agent-generated files (`GEMINI.md`, `QWEN.md`, etc.) are excluded from RepoMap via `.dogeignore`.
- Workflow files and session state under `.doge/` are project-local; never commit them.

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
