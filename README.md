# doge-code

An interactive CLI/TUI coding agent written in Rust (Edition 2024). It leverages OpenAI-compatible LLMs to read, search, and edit code, and provides a fast, minimal TUI with streaming output and basic repository analysis.

## Features

- CLI and TUI modes
- OpenAI-compatible Chat Completions API
  - Streaming output (TUI) and tool-use agent loop
  - Basic error handling and cancelation
- Safe filesystem tools confined to the project root
  - `/read`, `/write`, `/search` with path normalization and binary file guards
- Repository map (Rust only for now) using tree-sitter
  - `/map` lists Rust functions; planned: structs/enums/traits
- TUI UX
  - Real-time streaming output with status indicator (Idle/Streaming/Cancelled/Done/Error)
  - Esc to cancel ongoing streaming
  - Max log size with automatic truncation
  - @-file completion for project files in the input field
  - New: `/open <path>` launches your editor from TUI and safely returns

## Requirements

- Rust toolchain (stable)
- Network access to an OpenAI-compatible endpoint (default: https://api.openai.com/v1)
- An API key in `OPENAI_API_KEY` or provided via `--api-key`

## Install

```bash
cargo build --release
```

The resulting binary will be at:

```
target/release/doge-code
```

## Configuration

You can configure via a TOML config file, CLI flags, or environment variables (dotenv is supported).

Priority: CLI > Environment > Config file > Defaults.

Config file search order (XDG Base Directory spec):
1) `$DOGE_CODE_CONFIG` (explicit file path, highest priority)
2) `$XDG_CONFIG_HOME/doge-code/config.toml`
3) `~/.config/doge-code/config.toml`
4) Each dir in `$XDG_CONFIG_DIRS` (colon-separated), checked in order: `dir/doge-code/config.toml`

Sample `config.toml`:

```toml
# ~/.config/doge-code/config.toml
base_url = "https://api.openai.com/v1"
model = "gpt-4o-mini"
api_key = "sk-..."
log_level = "info"
# Optional project root override (absolute path)
# project_root = "/path/to/project"
```

Notes:
- When a config file is successfully loaded, a log line is written: `loaded config file` with the resolved path (API keys are never logged).
- If parsing fails, a warning is logged and execution continues with env/CLI/default values.

CLI and environment variables (examples):

- `--base-url` or `OPENAI_BASE_URL` (default: `https://api.openai.com/v1`)
- `--model` or `OPENAI_MODEL` (default: `gpt-4o-mini`)
- `--api-key` or `OPENAI_API_KEY`
- `--log-level` or `DOGE_LOG` (default: `info`)

Example:

```bash
OPENAI_API_KEY=sk-... \
OPENAI_BASE_URL=https://api.openai.com/v1 \
OPENAI_MODEL=gpt-4o-mini \
DOGE_LOG=debug \
./target/release/doge-code
```

## Usage

### TUI mode (recommended)

Run without flags to launch the TUI:

```bash
./target/release/doge-code
```

Key points:

- Type plain prompts (no leading slash) to talk to the LLM.
- Commands (recognized in TUI):
  - `/help` – list commands
  - `/map` – show a simple repo map (Rust fns only)
  - `/tools` – list available tools
  - `/clear` – clear the log area
  - `/open <path>` – open a file in your editor (see below)
  - `/retry` – resend your previous non-command input to the LLM
  - `/cancel` – cancel an ongoing LLM streaming
  - `/quit` – exit TUI
- Status is shown in the header (Idle/Streaming/Cancelled/Done/Error)
- Press `Esc` (or `Ctrl+C` once) to cancel streaming. Double `Ctrl+C` within 3s to exit.
- Input history is persisted under `~/.config/doge-code/...` (XDG paths respected).

@-file completion:

- Type `@` to trigger file completion based on the current project root.
- Navigate the popup with Up/Down; Enter to insert the selected path into input.
- Recent selections are prioritized.

New: `/open <path>` (TUI only)

- Launches your editor and temporarily suspends the TUI screen; upon editor exit, TUI is restored.
- Editor selection order: `$EDITOR` → `$VISUAL` → `vi`.
- Path resolution:
  - Relative paths are resolved against the configured `project_root`.
  - Absolute paths are allowed.
  - If the path does not exist, an error is shown in the log.
- Recommended workflow: type `/open @src/tui/view.rs` and use `@` completion to pick files.

### CLI mode

Run with `--no-tui` to use the plain CLI:

```bash
./target/release/doge-code --no-tui
```

Supported commands mirror TUI for file tools and map building. Note: `/open` is implemented in TUI; CLI behavior may differ or be unavailable depending on your version.

- `/help`, `/clear`, `/quit` or `/exit`, `/tools`
- `/read <path> [offset limit]`, `/write <path> <text>`, `/search <regex> [include_glob]`
- `/map`
- Plain text lines are sent to the LLM and the assistant reply is printed.

### Safety notes for Tools

- All file operations are restricted to the current project root directory.
- Search skips common binary/big files; use a glob include to narrow scope.

#### Tools module layout

The filesystem tools are split per operation for maintainability:

- `src/tools/mod.rs` — module wiring and re-exports
- `src/tools/common.rs` — `FsTools` struct (holds project root)
- `src/tools/read.rs` — `fs_read` implementation and path normalization
- `src/tools/write.rs` — `fs_write` implementation (guards project root, creates parents)
- `src/tools/search.rs` — `fs_search` implementation (regex over globbed files, skips binaries)

Public API remains the same via `pub use` in `tools::mod`.

## Developer Notes

- Edition: Rust 2024
- Concurrency: tokio, `reqwest` for HTTP, streaming token handling
- Parsing: `tree-sitter` + `tree-sitter-rust` for repo map (currently Rust functions)
- Logging: `tracing` to `./debug.log` (set `DOGE_LOG`)
- Tests: `cargo test`

## Roadmap

- Map: extract structs/enums/traits/impl methods; add filters
- Richer tool use via LLM (structured function calling)
- Theming, color toggle, and configurable max-log lines
- Session management enhancements (TUI) and better persistence UX

## License

MIT/Apache-2.0
