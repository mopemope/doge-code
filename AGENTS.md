# doge-code development

This Rust repository builds the AI coding agent `dgc`. Respond in Japanese.
User-facing behavior belongs in `README.md`; development guidance starts here.

## Work according to the request

- For investigation requests, establish reproduction and evidence before proposing
  a fix; edit only when implementation is authorized.
- For small local changes, read the affected code and callers. For cross-module
  changes, record acceptance criteria, affected boundaries, and focused checks.
- Use current official sources for external APIs, dependency upgrades, or facts
  that may have changed. Local behavior is established by code and tests.
- Use `rg` and bounded reads. Load only guidance relevant to the change.
- Clarify product decisions that affect visible behavior; continue authorized
  implementation and verification without repeated approval requests.
- Delegate only when authorized and tasks are independent. Give each worker a
  bounded scope, required evidence, and file ownership; count total token use.
- For long work, retain decisions, open issues, and completed checks in a short
  project-local work note. Do not commit `.doge/` runtime state.

## Choose the relevant guidance

Skill bodies are shared under `docs/ai/skills/`. Open the relevant `SKILL.md`
directly if the host cannot discover skills. No full-document reading is required.

| Change | Guidance |
|---|---|
| Tool schema, registration, dispatch, output | [dgc-tool-dev](docs/ai/skills/dgc-tool-dev/SKILL.md), [output contract](docs/tool-output-contract.md) |
| Verification or CI failures | [dgc-verify](docs/ai/skills/dgc-verify/SKILL.md) |
| Process policy, timeout, cancellation, jobs | [dgc-execution](docs/ai/skills/dgc-execution/SKILL.md) |
| Edit tools, undo, persistence, provenance | [dgc-mutation-provenance](docs/ai/skills/dgc-mutation-provenance/SKILL.md) |
| Context budget, history, compaction, runtime hints | [dgc-context-history](docs/ai/skills/dgc-context-history/SKILL.md) |
| TUI, MCP, RepoMap, configuration, prompt | Relevant section of [architecture](docs/ai/architecture.md) |
| Agent guidance, skill layout, development scripts | [agent workflow](docs/ai/workflow.md) |
| Comparing guidance or model configurations | [evaluation protocol](docs/ai/evaluation.md) |

## Shared constraints

- Rust Edition 2024, MSRV in `Cargo.toml`; use rustfmt and `tracing`.
- Feature implementations belong under `src/features/`.
- Use typed errors or `?` in production. Message-bearing `expect()` is fine in tests.
- Tests use `tempdir()` and explicit `AppConfig.project_root`, never real user
  config or repository-root fixtures. Cover behavior and error paths.
- dgc text mutation tools use the shared commit helper, `MutationReceipt`, and
  `FsTools::finalize_mutation`. See [contracts](docs/ai/contracts.md).
- Finite processes use `src/execution/runner.rs`; preserve policy separation.
  Register long-running user-visible TUI work through `JobManager`.
- Budget tool output at its source; return structured outcomes. Keep tool order
  deterministic. See [output contract](docs/tool-output-contract.md).
- Preserve unseen tool results and recoverable historical observations. Keep
  runtime hints request-scoped. Cached tokens still consume context capacity.
- Distinguish observed user directives from interpreted requirements.
- Keep durable session/provenance state out of rebuildable RepoMap SQLite and
  legacy `action_log`. Never commit credentials or project-local runtime state.

## Verification and completion

- First run matching tests: `bash scripts/verify.sh test <module_or_name>`.
  The wrapper rejects zero executed tests. `rg` must be available for search tests.
- Before finishing Rust changes: `cargo fmt --all`, then
  `bash scripts/verify.sh rust` (fmt check, warning-free Clippy, full locked tests).
- Docs/Skills/development-script-only changes: `bash scripts/verify.sh guidance`.
  Rust checks are required if runtime code or Rust build inputs also changed.
- Dependency/MSRV changes additionally use `bash scripts/verify.sh msrv`;
  TUI/dependency changes use `bash scripts/verify.sh tui-deps`. macOS checks and
  manual TUI verification are described in the verification skill.
- Reuse passed checks until relevant files or conditions change. Report command,
  result, and environment-blocked checks; blocked checks are not passes.
- Update README for new tools, flags, commands, and visible behavior. TUI changes
  need a screenshot or terminal recording. Commits use `type(scope): summary`.
- Finish when acceptance criteria are met and required checks pass, or report the
  concrete blocker and remaining work. Do not claim measured token savings
  without comparable evaluation runs.
