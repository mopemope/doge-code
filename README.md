# Doge-Code

Doge-Code is an interactive AI coding agent that provides advanced code analysis, editing, and project management capabilities through both a terminal UI and MCP (Model Context Protocol) server. Built with Rust, its modern architecture combines tree-sitter parsing, LLM integration, and persistent sessions to deliver a powerful coding assistant experience.

## 🚀 Key Features

### Core Capabilities
- **Intelligent Code Analysis**: tree-sitter based code parsing and symbol extraction (10+ languages including Rust, JavaScript/TypeScript, Python, Go, Java, C/C++, C#, Markdown)
- **Interactive Terminal UI**: Full-featured TUI with syntax highlighting, diff review, and real-time LLM interaction
- **MCP Server**: Model Context Protocol server for integration with MCP-enabled clients like Claude Desktop
- **Persistent Sessions**: SQLite-based session storage to maintain context across runs
- **Multi-Mode Interaction**: Support for both interactive TUI mode and command-line execution

### Supported Languages
- **Rust** (tree-sitter-rust 0.24.0)
- **JavaScript/TypeScript** (tree-sitter-javascript/typescript 0.25.0/0.23.2)
- **Python** (tree-sitter-python 0.25.0)
- **Go** (tree-sitter-go 0.25.0)
- **Java** (tree-sitter-java 0.23.5)
- **C/C++** (tree-sitter-c/c++ 0.24.1/0.23.4)
- **C#** (tree-sitter-c-sharp 0.23.1)
- **Markdown** (tree-sitter-md 0.5.1)

### Execution Modes

#### 1. Interactive TUI Mode (Default)
```bash
cargo run --release
# or
dgc
```
Launches a full terminal interface providing code exploration/navigation, real-time LLM chat interface, diff review/approval workflow, session management, and project overview/symbol browsing.

#### 2. Command Execution Mode
```bash
dgc exec "Add error handling to database connection function"
```
Executes a single instruction and exits with results.

#### 3. MCP Server Mode
```bash
dgc mcp-server 127.0.0.1:8000
```
Starts MCP server for integration with Claude Desktop and other clients.

#### 4. File Watch Mode
```bash
dgc watch
```
Monitors file changes and automatically triggers LLM assistant.

#### 5. Code Rewrite Mode
```bash
dgc rewrite --prompt "Convert to async/await" --code-file /tmp/code.rs
```
Rewrites specific code snippets with LLM assistant.

## 🔧 Installation

### Prerequisites
- Rust 1.70+ (Rust Edition 2024)
- OpenAI compatible API key (set via `OPENAI_API_KEY` environment variable)

### Build from Source
```bash
git clone https://github.com/mopemope/doge-code.git
cd doge-code
cargo build --release
```

### Configuration
Create `.doge/config.toml` in your project directory:
```toml
[llm]
model = "claude-3-5-sonnet-20241022"
base_url = "https://api.anthropic.com"

[project]
instructions_file = "PROJECT.md"
```

## 🛠️ Tools and Commands

### File System Tools
- `fs_read`: Read files with optional summary mode for large files
- `fs_write`: Create or overwrite files
- `fs_list`: List directory contents with pagination
- `find_file`: Search files by glob pattern
- `execute_bash`: Execute shell commands (with safety warnings)

### Code Analysis Tools
- `search_repomap`: Search parsed code symbols with advanced filtering
- `search_text`: Text-based search across files
- `fs_read_many_files`: Batch file reading with budget management

### Editing Tools
- `apply_patch`: Apply unified diff patches (multi-file support)
- `edit`: Replace specific code blocks
- `edit_symbol`: Edit entire symbols (functions, structs, etc.)

### Session Management
- `plan_write`/`plan_read`: Save and read task/execution plans (tied to sessions)
- `session`: Automatic session persistence and resume

### Memory Tools
- `read_memory`: Read content from persistent memory (markdown files)
- `write_memory`: Write content to persistent memory
- `list_memories`: List all available memory keys

### Advanced Tools
- `undo`: Revert the last file modification (edit or write)
- `search_history`: Semantic search of agent action history (RAG-based)
- `shell`: Persistent shell session for command execution

## 🎯 Usage Examples

### Basic Interactive Usage
```bash
# Start interactive session
dgc

# Resume last session
dgc --resume

# Skip repomap generation for faster startup
dgc --no-repomap
```

### Command Line Operations
```bash
# Execute single instruction
dgc exec "Add unit tests to user authentication module"

# JSON output for programmatic use (includes tools_called, token usage, etc.)
dgc exec --json "Refactor database layer"

# Rewrite specific code
dgc rewrite --prompt "Optimize this function for performance" \
    --code-file /tmp/algorithm.rs \
    --json
```

### Claude Desktop Integration
1. Start MCP server: `dgc mcp-server 127.0.0.1:8000`
2. Configure Claude Desktop to connect to `http://127.0.0.1:8000`
3. Access project tools directly from Claude Desktop chat interface

### Emacs Integration
Doge-Code includes Elisp-based Emacs integration:
```elisp
;; Load doge-code.el and doge-mcp.el
;; M-x doge-mcp-connect to connect to MCP server
;; M-x doge-chat to open chat interface
```

## 📝 TUI Slash Commands

The TUI provides various slash commands for quick operations:

| Command | Description |
|---------|-------------|
| `/help` | Display available commands and help |
| `/quit` | Exit the application |
| `/clear` | Clear the screen |
| `/cancel` | Cancel current processing |
| `/compact` | Compact conversation history using LLM summarization |
| `/edit-symbol` | Edit symbols (functions/classes) at current diff position |
| `/lint` | Run linters and apply auto-fixes |
| `/test` | Run tests for the project |
| `/map` | Display RepoMap |
| `/rebuild-repomap` | Rebuild the RepoMap |
| `/open` | Open a file |
| `/git-worktree` | Git worktree operations |
| `/theme` | Change color theme |
| `/tokens` | Display token usage |
| `/tools` | List available tools |

## 🔍 search_repomap Cheat Sheet

- `result_density`: Default `"compact"` returns no snippets and compresses to 5 symbols per file. Switch to `"full"` only for files where you need details to save context.
- `response_budget_chars`: Pass an upper limit like "5,000 characters" to automatically trim limit/symbol count/snippet length and prevent results from getting too large. If within budget, `warnings` and `next_cursor` allow fetching continuation.
- `cursor` / `page_size`: Paginate sorted results. `cursor` is 0-based next position, `page_size` is fetch count. If `next_cursor` is `Some(x)`, fetch next page with same query + `cursor=x`.
- Response is `SearchRepomapResponse`, returning `results` (conventional `RepomapSearchResult` collection) plus `warnings` and `applied_budget` (summary of actual limits applied).

Combining these enables maximum code exploration effectiveness without overwhelming LLM context.

## 📂 File Tools Lightweight Mode

- `fs_read`: Default mode is `mode="summary"` returning up to 400 lines & 6,000 characters, with remaining tracked via `next_cursor`. Only specify `mode="full"` or `page_size`/`cursor` when full text is needed.
- `fs_read_many_files`: Files resolved from `paths` are returned 5 per page with up to 40 lines each in `mode="summary"`, automatically returning `warnings` + `next_cursor` if `response_budget_chars` would be exceeded.
- `fs_list`: Directory listings also return as `FsListResponse`, with `entries` containing only `path` and `is_dir` for compactness. Use `cursor`/`page_size`/`response_budget_chars` to progressively fetch deep tree structures.

## 🎯 Symbol-Specific Editing /edit-symbol

- Running `/edit-symbol` identifies symbols (functions/impl/struct etc.) from the currently displayed diff review or most recent `@path:line`/`@path#Lline` file/line specification. If diff review is open, scroll position becomes the target, and file specification is not needed.
- Recognized symbols are passed to LLM, receiving diff or full symbol replacement applied via `apply_patch`. Results can be confirmed in `diff-review` pane, with `a` to approve and `r` to revert.
- On failure (no patch, broken parser, file updated), raw response is output to log, so modify instructions and call `/edit-symbol` again.

## 🛡️ Linter and Auto-Fix /lint

- `/lint` command auto-detects Go, Rust, TypeScript files in the project and runs configured linters (`cargo clippy`, `golangci-lint`, `npm run lint`, etc.).
- Attempts auto-fix (`--fix`) for detected issues, and for complex issues that can't be resolved, delegates analysis to LLM to propose fixes.
- Projects with multiple languages can be checked all at once.

## 🧪 Testing /test

- `/test` command automatically detects the project type and runs appropriate test commands:
  - Rust: `cargo test`
  - Go: `go test ./...`
  - Node.js: `npm test`
- Test output is captured and can be analyzed by LLM for failure diagnosis.

## 🔄 Advanced Features

### RepoMap System
Doge-Code builds a comprehensive symbol map of the project:
- Automatic language detection
- Symbol extraction (functions, structs, classes, etc.)
- Cross-reference analysis
- Incremental updates on file changes

### Smart Editing
- **Symbol-aware editing**: Edit entire functions, classes, modules
- **Diff review**: Preview changes before applying
- **Context preservation**: Maintain code style and patterns
- **Multi-file coordination**: Apply related changes across files

### LLM Integration
- **OpenAI Compatible**: Works with OpenAI, Anthropic, and other APIs
- **Streaming Responses**: Real-time output during LLM generation
- **Tool Use**: LLM autonomously calls tools
- **Conversation History**: Maintain context across interactions

### Automatic Verification
After file edits (`fs_write`, `edit`, `apply_patch`), automatic verification can run:
- **Rust**: Runs `cargo check` (configurable)
- **Python**: Syntax check (configurable)
- **Go**: `go build` (configurable)
- **TypeScript**: `tsc --noEmit` (configurable)

Verification failures are returned to LLM for automatic correction.

### Semantic Search (RAG)
- **Embedding-based search**: Search code symbols semantically using FastEmbed
- **Action history**: Track and search past agent actions
- **Auto-update**: Embeddings update automatically on file changes

### Remote MCP Tools
- Connect to remote MCP servers for additional tool capabilities
- Unified tool interface for local and remote tools
- Automatic tool discovery and registration

### Conversation History Compaction
- Automatic compaction when token threshold is exceeded
- LLM-based summarization preserves essential context
- Structured format for files accessed, actions taken, and outcomes

### Git Worktree Management
- Create isolated worktrees for parallel development
- Branch-based worktree creation
- Automatic cleanup

### Error Recovery System
- Autonomous error detection and diagnosis
- Recovery strategy selection
- Self-debugging capabilities

### Hook System
- Execute custom processing after each instruction
- Extensible hook interface
- Built-in hooks for repomap updates

### Custom Commands
- Define custom slash commands in `.doge/commands/`
- Template-based command definitions
- Parameter support for dynamic commands

## 📚 Configuration

### Environment Variables
- `OPENAI_API_KEY`: API key
- `OPENAI_BASE_URL`: API base URL (default: OpenAI)
- `OPENAI_MODEL`: Model name (default: gpt-4)
- `DOGECODE_CONFIG`: Configuration file path

### Configuration File (`.doge/config.toml`)
```toml
[llm]
model = "claude-3-5-sonnet-20241022"
base_url = "https://api.anthropic.com"
max_tokens = 4000
temperature = 0.1
# Context window size (auto-detected if not specified)
context_window_size = 200000
# Token threshold for auto compaction
auto_compact_prompt_token_threshold = 250000

[project]
instructions_file = "PROJECT.md"
exclude_patterns = ["target/", "node_modules/", "*.log"]

[mcp]
address = "127.0.0.1:8000"

[rag]
enabled = true
auto_update = true

[verification]
enabled = true
timeout_ms = 30000

[verification.commands]
rust = ["cargo", "check"]
python = ["python", "-m", "py_compile", "{path}"]
go = ["go", "build", "-o", "/dev/null", "{path}"]
typescript = ["npx", "tsc", "--noEmit", "-p", "{project_root}"]
node = ["node", "--check", "{path}"]

[watch]
enabled = true
debounce_ms = 500
patterns = ["*.rs", "*.go", "*.ts", "*.py"]

[mcp_servers]
# Define remote MCP servers to connect to
# name = { command = ["path/to/server"], args = ["--arg1"] }
```

## 🧪 Development

### Build and Test
```bash
# Code formatting
cargo fmt --all

# Run linting
cargo clippy --all-targets --all-features

# Run tests
cargo test

# Build release version
cargo build --release
```

### Adding New Languages
1. Add tree-sitter parser dependency to `Cargo.toml`
2. Implement `LanguageSpecificExtractor` trait
3. Add to language detection in `src/analysis/mod.rs`
4. Add tests in `src/analysis/tests/`

### Adding New Tools
1. Implement tool in `src/tools/` directory
2. Add to `FsTools` trait implementation
3. Register in `src/tools/mod.rs`
4. Add tests and documentation

### Adding Custom Commands
Create a TOML file in `.doge/commands/` directory:
```toml
name = "my-command"
description = "Description of my command"
template = "Execute the following task: {args}"
```

## 📖 Documentation

- **System Prompt**: `resources/system_prompt.md` - AI behavior guidelines
- **Agent Guidelines**: `AGENTS.md` - Integration procedures
- **Emacs Integration**: `elisp/emacs-integration.md`
- **API Documentation**: Generate with `cargo doc`

## 🏗️ Architecture

### Module Structure
- **`src/main.rs`**: CLI entry point and application orchestration
- **`src/analysis/`**: tree-sitter based code analysis and symbol extraction
- **`src/tools/`**: File system and code manipulation tools
- **`src/tui/`**: ratatui-based terminal user interface
- **`src/llm/`**: OpenAI-compatible LLM client and tool execution
- **`src/session/`**: SQLite-based session persistence
- **`src/mcp/`**: Model Context Protocol server implementation
- **`src/config/`**: Configuration management and TOML parsing
- **`src/features/`**: Additional feature modules (verification, worktree)
- **`src/error_recovery/`**: Autonomous error recovery system
- **`src/hooks/`**: Instruction hook system

### Key Characteristics
- **Async Architecture**: Tokio-based for high performance
- **Memory Efficient**: Lazy loading and pagination for large codebases
- **Extensible**: Plugin system for additional languages and tools
- **Type Safe**: Rust's strong typing prevents common errors
- **Cross-Platform**: Works on Linux, macOS, Windows

## 🤝 Contributing

1. Fork the repository
2. Create a feature branch
3. Implement changes with tests
4. Ensure `cargo fmt` and `cargo clippy` pass
5. Submit a pull request

## 📄 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## 🙏 Acknowledgments

- tree-sitter for excellent parsing capabilities
- Claude Desktop team for MCP specification
- Rust community for excellent tools
- All contributors and testers

---

**Note**: This is an active research project. Features may change as we explore the boundaries of AI-supported development.