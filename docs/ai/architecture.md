# Architecture and change routing

Read only the section relevant to the requested change. Paths are relative to the repository root.

## Module Map

| Path | Responsibility |
|---|---|
| `src/main.rs` | CLI entry point (clap), mode wiring |
| `src/execution/` | Execution foundation: policy, lifecycle, bounded output, and policy-free managed process mechanics |
| `src/execution/runner.rs` | Policy-free managed process mechanics: `ManagedProcessSpec`/`ManagedRunOptions`, bounded capture, timeout/cancellation, process-group cleanup/reap, and future-drop safety guard |
| `src/execution/process.rs` | ExecutionPolicy-aware LLM adapter: cwd/env/program policy, timeout resolution, and stable `ProcessResult` mapping |
| `src/execution/lifecycle.rs` | Unix process groups, SIGTERM/SIGKILL, explicit reap, and process-group existence checks |
| `src/exec.rs` | Non-interactive `exec` orchestration |
| `src/llm/` | OpenAI-compatible client, agent loop, tool dispatch |
| `src/llm/tool_def.rs` | Registry of tools exposed to the LLM (`default_tools_def`) |
| `src/llm/tool_execution/agent_loop.rs` | Main agent loop: iteration, loop detection, compaction triggers |
| `src/llm/tool_execution/dispatch.rs` | Tool call dispatch (one arm per tool) |
| `src/llm/tool_execution/dispatch/tools.rs` | Tool call handlers (one fn per tool) |
| `src/llm/tool_execution/history.rs` | Conversation state only (proactive + reactive compaction, stale tool-result clearing) |
| `src/llm/runtime_context.rs` | Request-scoped bootstrap hints (Recent Files / automatic memory), first-request-only overlay |
| `src/llm/context_budget.rs` | Preflight context governor: request footprint, token estimate, pressure classification (pure, no history mutation) |
| `src/config/context_budget.rs` | Preflight governor config (`[context_budget] mode = auto/observe/off`) |
| `src/llm/message_utils.rs` | Global tool-output truncation caps (see `docs/tool-output-contract.md`) |
| `src/llm/tool_runtime.rs` | Shared runtime handles; `MAX_ITERS` loop bound (256) |
| `src/llm/tool_execution/subagent.rs` | `task` sub-agent loop (read-only, isolated context) |
| `src/tools/` | Tool implementations (each file exposes a `tool_def()`); `budget.rs` for output budgets; `process.rs` is a thin `execute_process` adapter over `src/execution/` (keep policy/lifecycle logic in `src/execution/`, not in `FsTools`) |
| `src/analysis/` | tree-sitter parsing, symbol extraction, RepoMap, SQLite DAO, `loop_detector.rs`, `task_sentinel.rs` |
| `src/analysis/symbol_identity.rs` | Stable semantic IDs (`SymbolId`), content fingerprints, `SymbolIdentityIndex`, source spans |
| `src/analysis/parser.rs` | File parsing plus single-snapshot `analyze_source` for transactions |
| `src/features/semantic_edit.rs` | Transactional symbol edit engine (prepare/precondition/candidate/postcondition/shared mutation commit) |
| `src/provenance/` | Plan-to-Evidence graph: `types.rs` (v5 canonical envelope), `wire/` (`v1.rs` legacy read-only, `v2.rs` legacy read-only, `v3.rs` legacy read-only, `v4.rs` legacy read-only, `v5.rs` current), `store.rs` (v1+v2+v3+v4+v5 merged reads, v5 writes), `query.rs` (file-chain + symbol active/diverged/reverted coverage), `verification.rs` (conservative classifier), `obligations.rs` (obligation matching, binding hash, evidence states), `context.rs` (per-turn attribution, never global), `requirements.rs` (event-sourced state + requirement coverage) |
| `src/tools/requirements.rs` | `requirements_write` / `requirements_read` (directive-gated writes, budgeted reads with coverage) |
| `src/tools/mutation.rs` | Unified mutation transactions: snapshots, shared commit writer, receipts, diff/stats |
| `src/tui/` | ratatui TUI; slash commands under `src/tui/commands/` |
| `src/session/` | SQLite session persistence (SeaORM) |
| `src/jobs/` | Long-running application jobs: ownership, cancellation, task tracking, graceful shutdown |
| `src/mcp/` | MCP protocol/transport boundary (rmcp 3.x) |
| `src/mcp/client.rs` | Outbound MCP transport, negotiated connection lifecycle, timeouts, cancellation, and SDK response handling |
| `src/tools/remote_tools.rs` | Remote registry, stable aliases, and MCP-result → Doge normalized-result boundary |
| `src/mcp/server.rs` | Local listener lifecycle / graceful shutdown (`spawn_mcp_server`, `McpServerHandle`, bind-before-spawn) |
| `src/mcp/service.rs` | MCP tool/resource service (`DogeMcpService`, `McpServiceState` with shared `AppConfig`/RepoMap/build lock) |
| `src/mcp/http_security.rs` | Local HTTP Host/Origin security + loopback bind validation |
| `src/mcp/resource_path.rs` | Project resource path validation (`doge://files/`, `doge://symbols/`) |
| `src/config/` | AppConfig, `.doge/config.toml` loading |
| `src/features/evidence_report/` | Read-only session evidence collection, Git/workspace snapshots, typed schema v2, Markdown/JSON export |
| `src/features/verification_snapshot/` | Bounded project code-state endpoint acquisition and comparison for verification records; no coverage inference |
| `src/features/` | `testing.rs` (/test), `workflow.rs` (CLI run), `doc_skill/`, `worktree_manager.rs` |
| `src/watch.rs` | File watch mode (`dgc watch`) |
| `src/error_recovery/` | Autonomous error recovery hints |
| `src/hooks/` | Post-instruction hook system (repomap updates) |
| `resources/system_prompt.md` | System prompt template (Tera, rust-embed) |
| `elisp/` | Emacs integration (outside CI; see `elisp/emacs-integration.md`) |

## Common Change Patterns

### Adding a new tool

Use the [registration checklist](../tool-output-contract.md#tool-registration) and
[dgc-tool-dev](skills/dgc-tool-dev/SKILL.md). The output contract owns the checklist;
keep schema, dispatch, tests, and README in sync.

### Changing the system prompt

Edit `resources/system_prompt.md` (Tera template: `{{ os }}`, `{{ project_dir }}`). It is embedded at compile time via rust-embed (`src/assets.rs`) — rebuild to pick up changes. Project instructions files (`AGENTS.md` / `QWEN.md` / `GEMINI.md`, or `project_instructions_file` config key) are appended at runtime (`src/tui/commands/prompt.rs`).

### RepoMap / analysis changes

Symbol extraction lives in per-language collectors under `src/analysis/` (e.g. `rust_collector.rs`). Query-side budget/density logic is in `src/tools/search_repomap/repomap/repomap_filter.rs`. Tests for analysis live in `src/analysis/tests.rs`.

### Adding a TUI slash command

1. Implement the handler in `src/tui/commands/handlers/slash_commands/<name>.rs` (see existing files; `help.rs` owns the help listing).
2. Register the command in the slash-command dispatch there (`mod.rs` wires handlers).
3. Document it in the README slash-command table.

## Known Pitfalls

- Two workflow formats coexist: `.doge/workflows/*.yml` (shell `run:` steps and structured `program:`/`args:` steps, run by the `run_workflow` tool in `src/tools/workflow.rs`) and `.doge/workflows/*.md` (LLM-executed steps, run by the CLI `run` subcommand via `src/features/workflow.rs`). Keep both in mind; do not "fix" one to match the other without checking callers.
- `fs_read` accepts both `start_line` (legacy) and `cursor` (preferred, 1-based). New code should use `cursor`. Note `search_repomap`'s cursor is 0-based — the two conventions currently differ.
- `.doge/`, `target/`, and agent-generated files (`GEMINI.md`, `QWEN.md`, etc.) are excluded from RepoMap via `.dogeignore`.
- Workflow files and session state under `.doge/` are project-local; never commit them.
- `search_history` is a legacy internal dispatch route, deliberately absent from default schemas. Routing exceptions are classified in `docs/ai/tool-routing-exceptions.json`; new default tools require the full registration checklist.
- Tests must never write fixtures to the repo root (e.g. a `temp/` directory). Use `tempdir()` and explicit project roots as described in AGENTS.md.
- The README tool list must stay in sync with `default_tools_def` (`src/llm/tool_def.rs`); new tools require a README entry (checklist step 6).
- `.doge/repomap.sqlite` is rebuildable analysis/cache state and may be recreated during database recovery. Never store durable session/provenance state there.
- Legacy `action_log` is not the provenance store. Do not build new durable features on legacy `action_log`/repomap DB.

## Configuration & Secrets

- Config: environment variables + XDG-compliant TOML. Project overrides go in `.doge/config.toml` (top-level `project_instructions_file`, `[llm]`, `[project]`, `[mcp_server]` (local listener), `[watch]`, `[execution]`, `[[mcp_servers]]` — the MCP servers key is an array of tables for remote/outbound endpoints). Structured stdio uses `command`, `args`, and literal `[mcp_servers.env]`; `address` is HTTP or a deprecated stdio fallback.
- Never commit API keys; use `OPENAI_API_KEY` or `--api-key` locally.
- Tree-sitter language packs in `resources/tree-sitter-language-pack/` are vendored; update carefully and note version bumps in the PR description.
