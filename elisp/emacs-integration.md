# Doge-Code Emacs Integration

## Overview

Doge-Code is an interactive CLI/TUI coding agent built in Rust that uses OpenAI-compatible LLMs to assist with code reading, analysis, searching, and editing. This document explains the integration features between Doge-Code and Emacs, allowing seamless use of AI assistance within Emacs.

The integration consists of two main components:
1. **CLI-based Minimum Viable Integration (MVI)**: Calling Doge-Code's `--exec` subcommand as a subprocess from Emacs for code analysis and refactoring.
2. **MCP Server Mode**: Running Doge-Code as an HTTP server and calling tools (e.g., `search_repomap`, `fs_read`) in real-time from the Emacs client.

After installation you can enable the minor mode wherever you want (e.g. by adding it to `prog-mode-hook`). The keybindings described below become available whenever `doge-code-mode` is active. The integration leverages Doge-Code's tool system (static code analysis, file operations, etc.) to assist with code analysis, explanation, and refactoring.

## Requirements

- **Doge-Code**: Built binary (`dgc` or `doge-code`). Dependencies are defined in Cargo.toml.
- **Emacs**: Version 27.1 or higher.
  - CLI helper (`doge-code.el`): depends on the built-in `json`, `async`, and the third-party `popup` package.
  - MCP client (`doge-mcp.el`): depends on `request`, `json`, and `deferred`.
- **API Key**: Set OpenAI-compatible API key in `OPENAI_API_KEY` environment variable.
- **Project Root**: Doge-Code operates within a project root. The directory of the Emacs buffer serves as the root.

## Installation and Setup

### 1. Build and Setup Doge-Code
1. Clone the repository:
   ```
   git clone https://github.com/mopemope/doge-code.git
   cd doge-code
   ```
2. Install the Rust toolchain (via rustup):
   ```
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   source ~/.cargo/env
   ```
3. Install dependencies and build:
   ```
   cargo build --release
   ```
   - Binary generated at: `target/release/dgc` (recommended to add to PATH, e.g., `export PATH="$PATH:$HOME/.cargo/bin:./target/release"`).
4. Set API key (environment variable):
   ```
   export OPENAI_API_KEY="sk-your-key-here"
   ```
   - For persistence: Add to `~/.bashrc` or `~/.zshrc`.

### 2. Install Emacs Packages
#### Option 1: Manual Installation (Recommended and Easy)
1. Save the following files to `~/.emacs.d/lisp/`:
   - `doge-code.el` (CLI integration).
   - `doge-mcp.el` (MCP client).
2. Add to `init.el` (or equivalent):
   ```elisp
   ;; Load Doge-Code integration
   (add-to-list 'load-path "~/.emacs.d/lisp/")
   (require 'doge-code)
   (require 'doge-mcp)

   ;; Automatically enable mode in programming modes
   (add-hook 'prog-mode-hook 'doge-code-mode)

   ;; Customize binary path (if needed)
   (setq doge-code-executable "/path/to/doge-code/target/release/dgc")
   (setq doge-mcp-server-url "http://127.0.0.1:8000")  ; MCP server URL

   ;; Enable popup display (optional)
   (setq doge-code-use-popup t)
   ```
3. Restart Emacs or evaluate `init.el` with `M-x eval-buffer`.
4. Test: Open a new buffer and run `M-x doge-code-mode` → "Doge" should appear in the mode line.

#### Option 2: Via MELPA (When Available)
- For package publication: Add MELPA recipe.
- Currently recommended to use manual installation.

#### Option 3: Using straight.el (Emacs 27+)
In `init.el`:
```elisp
(use-package straight
  :ensure t)

(straight-use-package
 '(doge-code
   :type git
   :host github
   :repo "mopemope/doge-code"
   :files ("doge-code.el" "doge-mcp.el")))

(require 'doge-code)
(add-hook 'prog-mode-hook 'doge-code-mode)
(setq doge-code-executable "/path/to/dgc")
(setq doge-mcp-server-url "http://127.0.0.1:8000")
```

### 3. MCP Server Setup
1. Start the Doge-Code MCP server in terminal:
   ```
   dgc --mcp-server  # Default: http://127.0.0.1:8000
   ```
   - Background: `dgc --mcp-server &`.
   - Custom port: `dgc --mcp-server 127.0.0.1:9000`.
2. Set URL in Emacs (init.el):
   ```elisp
   (setq doge-mcp-server-url "http://127.0.0.1:9000")
   ```
3. Test: In Emacs, run `M-x doge-mcp-list-tools` → Tool list should be displayed.

### 4. Basic Setup Verification
- **Environment Variable Check**: In Emacs, `M-x shell-command` → `echo $OPENAI_API_KEY` (should output the key).
- **Binary Check**: `M-x shell-command` → `which dgc` (should display path).
- **Mode Check**: In a rust-mode buffer, `C-h m` → Check for "Doge" keybindings.

## Detailed Features

### 1. CLI-based Integration (MVI)
Asynchronously calls Doge-Code's CLI from Emacs: analysis/explanation flows use the `exec` subcommand, while inline rewrites invoke the dedicated `rewrite` subcommand. Results are parsed from JSON (`--json`) and either displayed or applied directly in the buffer as appropriate.

#### Commands
- **doge-code-analyze-region** (`C-c d a`):
  - Analyze selected region and display improvement suggestions.
  - Example: Select a function and analyze → Display "Code improvements: ..." in popup.
- **doge-code-refactor-region** (`C-c d r`):
  - Prompt for a rewrite instruction, send the selected region (or whole buffer if no region) to Doge-Code, and replace the text with the rewritten snippet returned from the CLI.
  - Example: Highlight a function, supply "Convert to async/await" as the prompt, and the region is replaced with the rewritten implementation.
- **doge-code-explain-region** (`C-c d e`):
  - Explain selected region (plain text output).
- **doge-code-analyze-buffer** (`C-c d b`):
  - Analyze the entire current buffer.
- **doge-code-cancel** (`C-c d c`):
  - Cancel the current Doge-Code process.

#### Usage Example
1. Open a Rust file.
2. Select a function.
3. Execute `C-c d a` → Display analysis results in *doge-output* buffer or popup.
4. Execute `C-c d r`, enter an instruction such as "Replace indexing with iterator APIs", and the region is rewritten inline when the CLI returns `rewritten_code`.
5. JSON response for rewrites: `{"success": true, "mode": "rewrite", "rewritten_code": "...", "tokens_used": 98}`.
6. On error: Display "Doge-Code Error: ..." in message bar.

#### Customization
- `doge-code-executable`: Binary path (default: "dgc").
- `doge-code-use-popup`: Use popup display if t, else buffer display.
- `doge-code-show-progress`: Show progress messages during execution.
- `doge-code-timeout`: Timeout for Doge-Code execution in seconds.

### 2. MCP Server Mode
Run Doge-Code as an HTTP server (`dgc --mcp-server [address]`). Directly call tools from Emacs client for real-time analysis.

#### Server Startup
- In terminal: `dgc --mcp-server` (default: http://127.0.0.1:8000).
- Custom port: `dgc --mcp-server 127.0.0.1:9000`.

#### Emacs Client Commands
- **doge-mcp-search-repomap** (`C-c d m s`):
  - Search repository map with keywords (e.g., "function name").
- **doge-mcp-fs-read** (`C-c d m f`):
  - Read a file (enter path).
- **doge-mcp-list-tools** (`C-c d m l`):
  - Display list of available tools.

#### Usage Example
1. Start the MCP server.
2. In Emacs: `M-x doge-mcp-search-repomap` → Enter keywords → Display symbols/code in result buffer.
3. Response: JSON format search results (file paths, symbols).

#### MCP Tools
Doge-Code tools available via MCP:
- `search_repomap`: Symbol search.
- `fs_read`: File reading.
- `fs_list`: Directory listing.
- etc. (extensible via rmcp).

## Troubleshooting

- **API Key Error**: Set `OPENAI_API_KEY`. JSON output shows `{"success": false, "error": "..."}`.
- **Server Connection Failure**: Check if MCP server is running (port 8000). Check firewall.
- **Emacs Errors**: Manually load with `M-x load-file`. Debug with `M-x toggle-debug-on-error`.
- **Output Display**: Check *doge-output* buffer, or enable popup with `C-h v doge-code-use-popup`.

## Future Enhancements

- **LSP Support**: Run Doge-Code as an LSP server (lsp-mode integration).
- **Streaming**: Real-time streaming via MCP (WebSocket).
- **Auto Apply**: Automatically insert/apply analysis results to Emacs buffers.
- **Buffer Integration**: Inline suggestions (Copilot-style).

For details, refer to the source code (doge-code.el, doge-mcp.el) or the repository. For issues, see Doge-Code's issue tracker.

---

*Generated by Doge-Code Agent on [current date].*
