# Repository Guidelines

## Project Structure & Module Organization
Doge-Code is a single-crate Rust application. The CLI entry point in `src/main.rs` wires together async orchestration from `src/exec.rs` and shared helpers in `src/utils.rs`. Agent capabilities live in modular directories: `src/analysis` for tree-sitter based repo scanning, `src/tools` for callable actions, `src/llm` for OpenAI-compatible clients, `src/tui` for the terminal UI, and `src/session` for persistence. Configuration loaders sit under `src/config`, optional feature flags under `src/features`, and assets defined in `src/assets.rs`. Developer assets include Emacs integration under `elisp/` and prompt/config templates in `resources/`.

## Build, Test, and Development Commands
- `cargo fmt --all` formats the crate using the shared `rustfmt.toml`.
- `cargo clippy --all-targets --all-features` enforces lint hygiene; treat warnings as actionable.
- `cargo test` runs the full suite, including module tests embedded with `#[cfg(test)]`.
- `cargo run --release -- <flags>` launches the TUI agent; use `--api-key` and configuration flags as described in the README.

## Coding Style & Naming Conventions
Rust Edition 2024 and four-space indentation are enforced via `rustfmt.toml`; run formatting before pushing. Follow idiomatic Rust naming: `snake_case` for functions/modules, `CamelCase` for types, `SCREAMING_SNAKE_CASE` for constants, and keep module paths short and descriptive. Prefer `tracing` spans over ad-hoc logging, and place feature-gated code behind the existing modules in `src/features`.

## Testing Guidelines
Add focused unit tests beside the code under `#[cfg(test)]` modules, and prefer integration-style assertions for cross-module flows. Use descriptive `test_<behavior>` names and cover both success and error paths. When touching async or tool execution code, include regression tests that exercise the relevant tool struct or session handler. Run `cargo test` locally before opening a pull request, and capture output for non-trivial fixes.

## Commit & Pull Request Guidelines
Git history follows `type(scope): summary` (for example, `refactor(tooling): ...`). Keep commits scoped and message bodies short but informative. Pull requests should describe the user-visible impact, enumerate testing performed, and link to tracking issues when relevant. Include screenshots or terminal recordings when altering the TUI experience. Ensure new commands or flags appear in `README.md` or this guide before requesting review.

## Configuration & Secrets
Runtime configuration prefers environment variables and XDG-compliant TOML files; see `resources/system_prompt.md` and `.doge/config.toml` for project-level overrides. Do not commit API keys; rely on `OPENAI_API_KEY` or `--api-key` when running locally. Update the bundled tree-sitter language pack in `resources/` carefully, keeping version notes in the PR description.

<!-- BACKLOG.MD MCP GUIDELINES START -->

<CRITICAL_INSTRUCTION>

## BACKLOG WORKFLOW INSTRUCTIONS

This project uses Backlog.md MCP for all task and project management activities.

**CRITICAL GUIDANCE**

- If your client supports MCP resources, read `backlog://workflow/overview` to understand when and how to use Backlog for this project.
- If your client only supports tools or the above request fails, call `backlog.get_workflow_overview()` tool to load the tool-oriented overview (it lists the matching guide tools).

- **First time working here?** Read the overview resource IMMEDIATELY to learn the workflow
- **Already familiar?** You should have the overview cached ("## Backlog.md Overview (MCP)")
- **When to read it**: BEFORE creating tasks, or when you're unsure whether to track work

These guides cover:
- Decision framework for when to create tasks
- Search-first workflow to avoid duplicates
- Links to detailed guides for task creation, execution, and completion
- MCP tools reference

You MUST read the overview resource to understand the complete workflow. The information is NOT summarized here.

</CRITICAL_INSTRUCTION>

<!-- BACKLOG.MD MCP GUIDELINES END -->
