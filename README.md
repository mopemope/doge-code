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

### CLI mode

Run with `--no-tui` to use the plain CLI:

```bash
./target/release/doge-code --no-tui
```

Type plain text lines to query the LLM. Supported commands:

- `/help` – show help
- `/clear` – clear screen
- `/quit` or `/exit` – exit
- `/tools` – list available tools
- `<plain text>` – send a prompt to the LLM (prints assistant reply)
- `/read <path> [offset limit]` – print file content (line-range optional)
- `/write <path> <text>` – write text to a file (creates parents, guards project root)
- `/search <regex> [include_glob]` – grep-like search with regex; optional glob filter
- `/map` – build and print a simple repo map (Rust fns only)

### TUI mode

Run without flags to launch the TUI:

```bash
./target/release/doge-code
```

Key points:

- Type commands into the input at the bottom (same commands as CLI)
- `/ask <text>` streams tokens to the log area in real time
- Status is shown in the header (e.g., `doge-code — [Streaming]`)
- Press `Esc` to cancel an ongoing `/ask` (mapped to `/cancel`)
- Type `/quit` to exit TUI

Status and system messages:

- `[Done]` – response finished
- `[Cancelled]` – request was cancelled
- `[Error] stream error` – a streaming error occurred

Separators:

- When you hit Enter with `/ask ...`, a timestamped separator like
  `[12:34:56] --------------------------------`
  is inserted to visually delineate sessions.

### Safety notes for Tools

- All file operations are restricted to the current project root directory
- Absolute paths are rejected; binary writes are not allowed
- Search skips common binary file types; use a glob include to narrow scope

#### Tools module layout (after refactor)

The filesystem tools are split per operation for maintainability:

- `src/tools/mod.rs` — module wiring and re-exports
- `src/tools/common.rs` — `FsTools` struct (holds project root)
- `src/tools/read.rs` — `fs_read` implementation and path normalization
- `src/tools/write.rs` — `fs_write` implementation (guards project root, creates parents)
- `src/tools/search.rs` — `fs_search` implementation (regex over globbed files, skips binaries)

Public API remains the same via `pub use` in `tools::mod`.

## Developer Notes

- Edition: Rust 2024
- Concurrency: tokio, `reqwest` for HTTP, SSE-like parsing for streaming tokens
- Parsing: `tree-sitter` + `tree-sitter-rust` for repo map (Rust functions)
- Logging: `tracing` to `./debug.log` (set `DOGE_LOG`) – currently the program writes a single log file per run
- Tests: `cargo test`

## Roadmap

- Map: extract structs/enums/traits/impl methods; add filters
- Config file (e.g., `doge.toml`) for model, base-url, temperature, max log lines
- Richer tool use via LLM (structured function calling)
- Multi-request handling with IDs and targeted cancel (`/cancel <id>`)
- Theming and color toggle

## License

MIT/Apache-2.0