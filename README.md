# doge-code

An interactive TUI coding agent written in Rust (Edition 2024). It leverages OpenAI-compatible LLMs to autonomously read, analyze, search, and edit code across multiple programming languages. It features a fast, minimal TUI with streaming output, comprehensive repository analysis, and autonomous task execution capabilities.

## Features

### Core Capabilities

- **Interactive TUI Interface** - Real-time streaming output with status indicators and cancellation support
- **Multi-language Repository Analysis** - Static analysis using tree-sitter for Rust, TypeScript, JavaScript, Python, Go, C#, and Java
- **Autonomous Agent Loop** - Advanced agent capabilities with function calling, tool execution, and feedback loops.

- **OpenAI-compatible API Integration** - Streaming chat completions with tool use support
- **Comprehensive Tool System** - 15+ built-in tools for filesystem operations, code analysis, and execution

### Advanced Features

- **Session Management** - Persistent conversation history with session creation, loading, and management
- **Smart Configuration** - TOML config files, CLI arguments, and environment variables with XDG Base Directory compliance
- **Theme System** - Dark/light themes with runtime switching via `/theme` command
- **File Completion** - @-file completion for project files with intelligent path resolution
- **Editor Integration** - `/open <path>` command launches your preferred editor and safely returns to TUI
- **Project Instructions** - Automatic loading of project-specific instructions from AGENTS.md, QWEN.md, or GEMINI.md
- **Shell Mode** - Execute shell commands directly within the TUI using `!` or `/shell`.
- **File Watch Mode** - Run in a non-interactive mode to watch for file changes and execute predefined tasks. When a file is modified, Doge-Code looks for comments with the pattern `// AI!: <instruction>` and automatically executes the instruction using the LLM.
- **Emacs Integration** - Full integration with Emacs for in-editor AI assistance

### TUI Experience

- Real-time streaming output with status indicator (Idle/Streaming/Cancelled/Done/Error)
- Esc to cancel ongoing streaming operations
- Automatic log truncation with configurable limits
- Input history persistence across sessions
- Safe filesystem operations confined to project root

## Requirements

- Rust toolchain (Edition 2024, stable)
- Network access to an OpenAI-compatible endpoint (default: https://api.openai.com/v1)
- An API key in `OPENAI_API_KEY` or provided via `--api-key`
- *For Emacs integration*: Emacs 27.1+ with `json`, `async`, `request`, and `popup` packages

## Installation

```bash
cargo build --release
```

The resulting binary will be at:

```
target/release/doge-code
```

### Emacs Integration Setup

To use the Emacs integration:

1. Copy the Emacs Lisp files from the `elisp/` directory to your Emacs load path
2. Add the following to your Emacs configuration:

```elisp
(require 'doge-code)
(require 'doge-mcp)
(add-hook 'prog-mode-hook 'doge-code-mode)
```

3. Set the path to the Doge-Code binary:

```elisp
(setq doge-code-executable "/path/to/doge-code/target/release/dgc")
```

See `elisp/emacs-integration.md` for detailed installation instructions and usage.

## Configuration

You can configure via TOML config file, CLI flags, or environment variables (dotenv is supported).

**Priority**: CLI > Environment > Project config file > Global config file > Defaults

### Config File Locations (XDG Base Directory spec)
1. `$DOGE_CODE_CONFIG` (explicit file path, highest priority)
2. `$XDG_CONFIG_HOME/doge-code/config.toml`
3. `~/.config/doge-code/config.toml`
4. Each dir in `$XDG_CONFIG_DIRS` (colon-separated): `dir/doge-code/config.toml`
5. Project-specific config: `<project-root>/.doge/config.toml` (loaded first with highest precedence among config files)

### Sample Configuration

```toml
# ~/.config/doge-code/config.toml
base_url = "https://api.openai.com/v1"
model = "gpt-5-mini"
api_key = "sk-..."
log_level = "info"
theme = "dark"
# Auto-compact threshold (prompt tokens). When the prompt token count reaches this number, the TUI will automatically trigger a conversation compaction (/compact).
auto_compact_prompt_token_threshold = 250000
# Show diff when applying patches
show_diff = true

# Allowed commands for execute_bash tool
# You can specify commands that are allowed to be executed
# Only exact matches or prefix matches (with a space) are allowed
# For example, if you allow "cargo", then "cargo build" and "cargo test" will be allowed
# but "carg" or "cargox" will not be allowed
# Example:
# allowed_commands = [
#   "cargo",
#   "git",
#   "ls"
# ]
allowed_commands = []

```

### Environment Variables and CLI Options

- `--base-url` or `OPENAI_BASE_URL` (default: `https://api.openai.com/v1`)
- `--model` or `OPENAI_MODEL` (default: `gpt-4o-mini`)
- `--api-key` or `OPENAI_API_KEY`
- `--log-level` or `DOGE_LOG` (default: `info`)
- `--no-repomap` - Disable repomap creation at startup
- `--resume` - Resume the latest session

Example:

```bash
OPENAI_API_KEY=sk-...
OPENAI_BASE_URL=https://api.openai.com/v1 \
OPENAI_MODEL=gpt-4o-mini \
DOGE_LOG=debug \
./target/release/doge-code --no-repomap --resume
```

## .dogeignore File

The `.dogeignore` file specifies files and directories that should be ignored by the Doge-Code agent. This file uses the same syntax as `.gitignore`.

### Purpose

- **Exclude files from LLM context**: Files and directories listed in `.dogeignore` will not be included in the context sent to the LLM. This helps to reduce the size of the context and prevent sensitive or irrelevant information from being sent to the LLM.
- **Exclude files from analysis**: The agent will not analyze files and directories listed in `.dogeignore`. This can improve performance by reducing the amount of code that needs to be processed.

## Usage

Run without flags to launch the TUI:

```bash
./target/release/doge-code
```

You can also start in watch mode to react to file system changes:
```bash
./target/release/doge-code --watch
```

### TUI Commands

- **Plain prompts** (no leading slash) - Talk to the LLM.
- **`! <command>` or `/shell <command>`** - Execute a shell command directly in the project root.
- `/help` - Show this help message.
- `/map` - Show repository analysis (functions, classes, etc.).
- `/tools` - List available tools.
- `/session` - Manage sessions (e.g., `/session list`, `/session new <title>`, `/session switch <id>`, `/session delete <id>`).
- `/clear` - Clear the conversation and log area.
- `/open <path>` - Open a file in your editor (respects `$EDITOR`, `$VISUAL`).
- `/theme <name>` - Switch theme (dark/light).
- `/git-worktree` - Create a new git worktree for parallel processing.
- `/cancel` - Cancel the current operation.
- `/quit` - Exit the application.

### Custom Slash Commands

Custom slash commands allow you to define frequently used prompts as Markdown files that Doge-Code can execute. Commands are organized by scope (project-specific or personal) and support directory structure for namespacing.

#### Syntax

```
/<command-name> [arguments]
```

#### Parameters

| Parameter | Description |
|-----------|-------------|
| `<command-name>` | Name derived from the Markdown filename (without .md extension) |
| `[arguments]` | Optional arguments passed to the command |

#### Command Types

##### Project Commands

Commands stored in the repository and shared with the team.
When listed with /help, these commands display "(project)" after their description.

Location: `.doge/commands/`

Example:
```shell
# Create a project command
mkdir -p .doge/commands
echo "Analyze this code for performance issues and suggest optimizations:" > .doge/commands/optimize.md
```

##### Personal Commands

Commands available across all projects.
When listed with /help, these commands display "(user)" after their description.

Location: `~/.config/doge-code/commands/`

Example:
```shell
# Create a personal command
mkdir -p ~/.config/doge-code/commands
echo "Review this code for security vulnerabilities:" > ~/.config/doge-code/commands/security-review.md
```

#### Features

##### Namespacing

Organize commands in subdirectories.
Subdirectories are used for organization and displayed in the command description, but they don't affect the command name itself.
The description will show which directory the command comes from (either the project directory `.doge/commands` or the user-level directory `~/.config/doge-code/commands`) along with the subdirectory name.

Conflicts between user-level and project-level commands are not supported. Otherwise, multiple commands with the same base filename can coexist.

For example, a file at `.doge/commands/frontend/component.md` would create a `/component` command with a description showing "(project:frontend)". Meanwhile, a file at `~/.config/doge-code/commands/component.md` would create a `/component` command with a description showing "(user)".

##### Arguments

Use argument placeholders to pass dynamic values to commands:

`$ARGUMENTS` for all arguments
The `$ARGUMENTS` placeholder captures all arguments passed to the command:

```shell
# Command definition
echo 'Fix issue #$ARGUMENTS following our coding standards' > .doge/commands/fix-issue.md

# Usage
> /fix-issue 123 high-priority
# $ARGUMENTS becomes "123 high-priority"
```

`$1`, `$2`, etc. for individual arguments
Use positional parameters to access specific arguments individually (like shell scripts):

```shell
# Command definition  
echo 'Review PR #$1 with priority $2 and assign to $3' > .doge/commands/review-pr.md

# Usage
> /review-pr 456 high alice
# $1 becomes "456", $2 becomes "high", $3 becomes "alice"
```

Use positional arguments when:

- You need to access arguments individually in different parts of the command
- You want to provide defaults for missing arguments
- You want to build more structured commands with specific parameter roles

### Key Bindings

- **Esc** (or `Ctrl+C` once) - Cancel streaming
- **Double `Ctrl+C`** within 3s - Exit application
- **Up/Down** - Navigate input history
- **@** - Trigger file completion for project files

### File Completion

Type `@` to trigger file completion based on the current project root:
- Navigate with Up/Down arrows
- Enter to insert selected path
- Recent selections are prioritized
- Works with relative paths resolved against project root

### Emacs Integration

Doge-Code offers comprehensive integration with Emacs through two complementary approaches:

### 1. CLI-based Integration (MVI)
Direct integration with the Doge-Code CLI for:
- Code analysis and suggestions (`C-c d a`)
- Code refactoring (`C-c d r`)
- Code explanations (`C-c d e`)
- Buffer-wide analysis (`C-c d b`)

### 2. MCP Server Integration
Run Doge-Code as an MCP HTTP server for real-time tool access:
- Symbol search with `search_repomap` (`C-c d m s`)
- File reading with `fs_read` (`C-c d m f`)
- Direct tool calling from Emacs

See `elisp/emacs-integration.md` for detailed installation and usage instructions.

## Available Tools

The LLM has access to comprehensive tools for autonomous operation:

### Filesystem Tools
- `fs_read` - Read files with optional line range specification
- `fs_write` - Write files with automatic parent directory creation
- `search_text` - Search files using regex patterns with glob filtering
- `fs_list` - List directory contents with configurable depth
- `find_file` - Find files by name pattern

### Code Analysis Tools

- `search_repomap` - Search analyzed code symbols with filtering

### Development Tools
- `execute_bash` - Execute shell commands safely in the project root directory
- `apply_patch` - Apply patches to files
- `edit` - Advanced file editing with search/replace operations
- `todo_write` - Create and manage a structured task list for the current coding session
- `todo_read` - Read the todo list for the current session

### Multi-file Operations
- `fs_read_many_files` - Read multiple files efficiently in parallel

## Multi-language Support

Repository analysis supports:

- **Rust** (.rs) - Functions, structs, enums, traits, impl blocks
- **TypeScript** (.ts, .tsx) - Functions, classes, interfaces, types
- **JavaScript** (.js, .mjs, .cjs) - Functions, classes, objects
- **Python** (.py) - Functions, classes, methods
- **Go** (.go) - Functions, types, methods, interfaces
- **C#** (.cs) - Namespaces, classes/structs, interfaces, enums, methods/constructors, properties/fields
- **Java** (.java) - Classes, methods, interfaces
- **Markdown** (.md) - Headings as symbols (sections, subsections)

The analysis extracts:
- Symbol names and types
- File locations and line numbers
- Function signatures and documentation
- Hierarchical relationships

## Session Management

- **Automatic Persistence** - Conversation history saved automatically
- **Session Creation** - Each conversation creates a unique session
- **History Management** - Input history persists across sessions
- **XDG Compliance** - Sessions stored in `~/.config/doge-code/sessions/`

## Safety Features

- **Project Root Confinement** - All file operations restricted to project directory
- **Binary File Detection** - Automatic skipping of binary files in search operations
- **Path Normalization** - Safe resolution of relative and absolute paths
- **Graceful Error Handling** - Comprehensive error reporting and recovery

## Developer Notes

### Technical Stack
- **Edition**: Rust 2024
- **Concurrency**: tokio for async operations, reqwest for HTTP with streaming
- **Parsing**: tree-sitter with language-specific parsers for multi-language analysis
- **UI**: crossterm for cross-platform terminal handling
- **Logging**: tracing framework with file output to `./debug.log`
- **Configuration**: TOML with XDG Base Directory specification compliance
- **Testing**: Comprehensive test suite with 119+ passing tests

### Architecture
- **Modular Design**: Separate modules for analysis, LLM client, tools, TUI, and configuration
- **Async-First**: Non-blocking operations with tokio runtime
- **Tool System**: Extensible tool architecture with LLM function calling integration
- **Static Analysis**: tree-sitter based parsing with language-specific extractors
- **Streaming**: Real-time LLM response streaming with cancellation support

### Key Modules
- `src/analysis/` - Multi-language static analysis and repository mapping
- `src/llm/` - OpenAI-compatible client with streaming and tool execution
- `src/tools/` - Comprehensive tool system for autonomous operations
- `src/tui/` - Terminal user interface with event handling and rendering
- `src/config/` - Configuration management with multiple sources
- `src/session/` - Session persistence and history management

### Testing
Run the test suite:
```bash
cargo test
```

### Logging
Application logs are written to `./debug.log` for debugging and troubleshooting.

## Roadmap

### Planned Enhancements
- **Extended Language Support** - Additional programming languages and frameworks
- **Advanced Tool Integration** - More sophisticated development tools and integrations
- **Enhanced UI/UX** - Improved theming, customizable layouts, and visual enhancements
- **Performance Optimizations** - Faster analysis, caching improvements, and memory optimization
- **Plugin System** - Extensible architecture for custom tools and integrations



## License

MIT/Apache-2.0
