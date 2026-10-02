# Agent development workflow

## Shared instructions and discovery

Keep repository-specific constraints in [AGENTS.md](../../AGENTS.md). Put only
stable common constraints and routing there; conditional procedures belong in
Skills and the [architecture](architecture.md) or [contracts](contracts.md).
The [tool output contract](../tool-output-contract.md) owns output limits and
shapes; do not duplicate numerical caps in skill bodies.

Skill source files live in `docs/ai/skills/<name>/SKILL.md`. Each host has a relative
directory symlink to that source:

| Host | Discovery directory | Link target from that directory |
|---|---|---|
| Codex | `.agents/skills/` | `../../docs/ai/skills/<name>` |
| Claude | `.claude/skills/` | `../../docs/ai/skills/<name>` |
| OpenCode | `.opencode/skill/` | `../../docs/ai/skills/<name>` |

Add each new skill to all three locations and to the routing table in AGENTS.md.
Use a short, precise `description` containing doge-code/dgc and the triggering
change. The metadata checker accepts single-line safe plain strings or JSON-style
double-quoted strings; quote descriptions containing YAML mapping/comment syntax.
Keep general Rust advice out of skill bodies. Preserve automatic skill
selection; do not add dependencies or host metadata without a concrete need.
`CLAUDE.md` imports AGENTS.md. Other hosts can read linked SKILL.md files directly.
These paths support discovery; actual activation remains host-dependent.

Codex discovers instructions along its startup working-directory chain, while
dgc currently reads a project-root instruction file. A nested AGENTS.md is not a
portable replacement for explicit root routing. See official [instruction
discovery](https://learn.chatgpt.com/docs/agent-configuration/agents-md) and [skill
discovery](https://learn.chatgpt.com/docs/build-skills).

## Verification matrix

Run the narrowest relevant test first. The commands below run from any working
directory because the wrapper resolves the repository root from its own path.
Python 3.11+ is required for development scripts; no third-party Python packages
are needed. Rust/search checks also need Cargo and ripgrep on PATH.

| Change | Focused checks | Before completion |
|---|---|---|
| Docs, Skills, development scripts | `bash scripts/verify.sh guidance` | Review trigger scope and retained constraints; script behavior tests run automatically |
| Tool schema/dispatch/output | `bash scripts/verify.sh test llm::tool_execution::dispatch::` plus affected tool tests | Guidance parity check and `bash scripts/verify.sh rust` |
| Process/jobs | `bash scripts/verify.sh test execution::` plus affected jobs/workflow tests | Rust gate; relevant OS evidence |
| Mutation/provenance | `bash scripts/verify.sh test tools::mutation::` plus affected semantic edit/provenance tests | Rust gate; wire compatibility and undo evidence |
| Context/history | `bash scripts/verify.sh test llm::context_budget::` or history filter | Rust gate; unseen-result and recovery regressions |
| TUI | `bash scripts/verify.sh test tui::` | Rust gate, `tui-deps`, manual TUI run and screenshot/recording |
| Cargo/dependencies/MSRV | Affected tests and builds | Rust gate, `msrv`, `tui-deps` when the TUI graph changes |

`rust` runs `cargo fmt --all --check`,
`cargo clippy --locked --all-targets --all-features -- -D warnings`, and
`cargo test --locked`. Format with `cargo fmt --all` before running it.
`msrv` uses the package's `rust-version` via `rustup run`; install the matching
toolchain separately when needed. A missing toolchain is an environment blocker.
`macos` runs the CI's TUI/execution/jobs tests only on macOS. `tui-deps` verifies
that the declared Crossterm backend is active and legacy 0.28 metadata is inactive.

Logs are stored outside the repository in a new system temporary directory, or
in `--log-dir <path>`. Successful commands emit compact results; failures return
the command's exit code and a bounded diagnostic tail with the full log path.
Rust test mode rejects empty filters, zero matches, and ignored-only results;
guidance also rejects Python unittest discovery with zero executed tests. The
wrapper supports standard Rust and unittest result formats; unsupported formats fail
verification rather than guessing a pass. A compile/startup failure does not
count as a test failure or pass. Do not commit logs or user/environment values.

CI owns platform-specific gates. Reuse passing checks until their inputs change;
guidance-only work does not require rebuilding unrelated Rust code. For Rust
changes, full completion gates remain mandatory. Record any blocked gate.

## Automated contracts and their limits

`python3 scripts/check-agent-guidance.py` checks skill metadata, all three host
links, local Markdown links, default schema/dispatch/README parity, and wrapper/CI
command and MSRV agreement. Its tool
inventory reader supports the repository's current literal factory/match format
and fails on unsupported factory syntax; it is not a Rust parser. Update the
reader and its behavior tests if registry structure changes.

[Routing exceptions](tool-routing-exceptions.json) explicitly distinguish
legacy internal dispatch, runtime discovery tools, and README command entries.
Do not use an exception to conceal a new tool's missing wiring. The check detects
stale classifications but cannot prove runtime activation or handler correctness.
Use dispatch tests for those behaviors.

Existing Rust regressions own JSON output safety, compact plan acknowledgements,
MutationReceipt/undo/provenance connections, and unseen observation recovery.
Add behavior tests for uncovered failures rather than another prose assertion or
a source-text check claiming runtime correctness. CI runs the guidance checker
and development-script tests in addition to its Rust gates.

## Task scope and completion evidence

For small changes, inspect affected code/callers and verify the result. For
investigation-only requests, report reproduction, evidence, and candidate causes
without edits. For visible specification changes, identify acceptance criteria
and unresolved product choices before implementing dependent behavior.

Use official Web sources when external API/dependency facts can drift; use the
locked version and local callers to determine applicability. Avoid mandatory
whole-repository or whole-document reads for every task.

When delegation is authorized, assign independent scopes and file ownership.
Return evidence, paths, and unresolved issues rather than duplicating raw logs.
Measure total worker tokens as well as elapsed time; parallelism can cost more.

For long tasks, keep a short project-local work note with acceptance criteria,
decisions, remaining issues, and completed checks. Distinguish user directives
from interpretation. Do not persist that note as a new universal rule.
Report acceptance evidence and environment limitations at completion.
