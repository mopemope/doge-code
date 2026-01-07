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
1. Build the release binary: `cargo build --release`.
2. Ensure the binary (default: `dgc`) is in your `PATH`.

### 2. Emacs Configuration (Quick Start)
Add the following to your `init.el`. This single setup function automatically detects and enables available extensions (MCP, Flymake, HUD, etc.).

```elisp
;; Add doge-code elisp directory to load-path
(add-to-list 'load-path "/path/to/doge-code/elisp")
(require 'doge-code)

;; Optional: Customize behavior before setup
(setq doge-code-executable "dgc") 
(setq doge-code-enable-hud t) ; HUD is disabled by default

;; Initialize all components
(doge-code-setup)
```

### 3. Using with use-package
```elisp
(use-package doge-code
  :load-path "/path/to/doge-code/elisp"
  :config
  (setq doge-code-executable "dgc")
  (doge-code-setup))
```

## Features and Keybindings

When `doge-code-setup` is called, it automatically enables `doge-code-mode` in programming modes and sets up the following keybindings:

### Core CLI Features
- `C-c d a`: **Analyze Region** - Get improvement suggestions.
- `C-c d r`: **Rewrite/Refactor Snippet** - Replace region with AI-generated code.
- `C-c d e`: **Explain Region** - Get plain text explanation.
- `C-c d b`: **Analyze Buffer** - Analyze the entire file.
- `C-c d c`: **Cancel** - Stop the current Doge-Code process.

### MCP Extensions (Requires `request`, `deferred`)
If dependencies are met, these become available:
- `C-c d m s`: **MCP Search** - Search repository map symbols.
- `C-c d m f`: **MCP Read** - Read file content via MCP server.

### Auto-Fix and Refactoring
- `C-c d f`: **Fix Flymake** - Attempt to fix Flymake diagnostic at point.
- `C-c d R`: **Global Refactor** - Perform large-scale codebase refactoring.
- **Compilation Fix**: Automatically prompts to fix failed compilations.

### Semantic HUD (Optional)
Set `(setq doge-code-enable-hud t)` before setup to enable ghost-text overlays showing symbol info.

## Customization

You can control which features are enabled by setting these variables **before** calling `doge-code-setup`:

| Variable | Default | Description |
|----------|---------|-------------|
| `doge-code-enable-auto-mode` | `t` | Auto-enable `doge-code-mode` in `prog-mode`. |
| `doge-code-enable-mcp` | `t` | Enable MCP tools integration. |
| `doge-code-enable-hud` | `nil` | Enable Semantic HUD overlays. |
| `doge-code-enable-flymake` | `t` | Enable Flymake auto-fix command. |
| `doge-code-enable-compile` | `t` | Enable compilation finish hook for fixing. |
| `doge-code-enable-refactor` | `t` | Enable git-aware refactoring tools. |

## Dependencies

Doge-Code intelligently handles missing packages. If an optional dependency is missing, that specific feature will be disabled with a message in the echo area, but the rest of Doge-Code will continue to work.

- **Core**: `json`, `async` (required)
- **MCP/HUD**: `request`, `deferred`
- **Better UI**: `popup`

## Detailed Features

### 1. CLI-based Integration (MVI)
Asynchronously calls Doge-Code's CLI from Emacs: analysis/explanation flows use the `exec` subcommand, while inline rewrites invoke the dedicated `rewrite` subcommand. Results are parsed from JSON (`--json`) and either displayed or applied directly in the buffer as appropriate.

When a rewrite is requested, Doge-Code now normalizes the reported file path relative to the configured project root before it is sent to the LLM and echoed back in JSON responses. This avoids leaking absolute paths while still grounding the model in the correct file context and gives Emacs enough information to show which buffer was rewritten.

#### Commands
- **doge-code-analyze-region** (`C-c d a`):
  - Analyze selected region and display improvement suggestions.
  - Example: Select a function and analyze → Display "Code improvements: ..." in popup.
- **doge-code-refactor-region** (`C-c d r`):
  - Prompt for a rewrite instruction, send the selected region (or whole buffer if no region) to Doge-Code, and replace the text with the rewritten snippet returned from the CLI.
  - The Emacs helper now verifies that the buffer has not changed while the LLM request is in-flight; if the user edits the region, the rewrite is aborted with a clear message.
  - Success messages include the relative project path reported by the Rust CLI (e.g. `src/lib.rs`) so you can confirm the rewrite scope.
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

- `doge-code-timeout`: Timeout for Doge-Code execution in seconds.

### 2. Org-Mode Integration (Org-Doge)
Interactive "Laboratory" mode using Org-babel.

1.  Add `ob-doge.el` to your load path.
2.  Add `(require 'ob-doge)` to your init file.
3.  Register the language:
    ```elisp
    (org-babel-do-load-languages
     'org-babel-load-languages
     '((doge . t)))
    ```

#### Usage
Create a source block with `doge` language:

```org
#+BEGIN_SRC doge
Explain how `Option<T>` works in Rust.
#+END_SRC
```

Execute with `C-c C-c`. The output will be inserted below the block.

### 3. MCP Server Mode
Run Doge-Code as an HTTP server (`dgc --mcp-server [address]`). Directly call tools from Emacs client for real-time analysis.

#### Server Startup
- In terminal: `dgc --mcp-server` (default: http://127.0.0.1:8000).
- Custom port: `dgc --mcp-server 127.0.0.1:9000`.

#### Emacs Client Commands
- **doge-mcp-search-repomap** (`C-c d m s`):
  - Search repository map with keywords (e.g., "function name").
  - 既定では `result_density="compact"` が有効で、スニペット無し＆5シンボル/ファイルに圧縮されます。詳細が必要な場合のみ `"full"` を指定してください。
  - コンテキスト節約のために `response_budget_chars` でレスポンス上限（例: 4000）を渡すか、`cursor`/`page_size` でページングしてください。戻り値の `next_cursor` を次リクエストに渡すと続きが取得できます。
- **doge-mcp-fs-read** (`C-c d m f`):
  - Read a file (enter path).
- **doge-mcp-list-tools** (`C-c d m l`):
  - Display list of available tools.

### 4. Semantic HUD
Ghost-text overlays showing symbol information.

1.  Add `doge-hud.el` to load path.
2.  `(require 'doge-hud)`.
3.  Enable with `(doge-hud-mode 1)`.

### 5. Self-Healing Flymake
Automatic error fixing integration.

1.  Add `doge-flymake.el` to load path.
2.  `(require 'doge-flymake)`.
3.  Bind the fix command to a convenient key:
    ```elisp
    (with-eval-after-load 'flymake
      (define-key flymake-mode-map (kbd "C-c d f") 'doge-flymake-fix-at-point))
    ```

#### Usage
Move cursor to a Flymake error and press `C-c d f`. Doge will attempt to rewrite the code to fix the specific error message.

### 6. Usage Example
1. Start the MCP server.
2. In Emacs: `M-x doge-mcp-search-repomap` → Enter keywords → Display symbols/code in result buffer.
3. Response: JSON format search results (file paths, symbols).

#### MCP Tools
Doge-Code tools available via MCP:
- `search_repomap`: Symbol search.
- `fs_read`: File reading.
- `fs_list`: Directory listing.
- etc. (extensible via rmcp).

> ℹ️ すべてのファイル系ツール（`fs_read`/`fs_read_many_files`/`fs_list`）はデフォルトでコンパクトモードになりました。まずは summary で概要を掴み、必要な場合のみ `mode="full"` や `cursor`/`page_size` を指定して詳細を取得してください。

## Troubleshooting

- **"Doge-Code executable not found"**:
  - Ensure `dgc` (or `doge-code`) is in your PATH.
  - Or set `(setq doge-code-executable "/full/path/to/target/release/dgc")` in Emacs.
- **"JSON parse error" / Raw Output**:
  - Often indicates the process panicked or the API key is missing. Check the `*doge-output*` buffer or the error message for details like "OPENAI_API_KEY not set".
- **MCP Connection Failed**:
  - Is the server running? Run `dgc --mcp-server` in a terminal.
  - Check the port (default 8000).
- **Encoding Issues (Japanese characters)**:
  - The integration now explicitly decodes output as UTF-8. If garbled text persists, check your `process-coding-system-alist`.


## Future Enhancements

- **LSP Support**: Run Doge-Code as an LSP server (lsp-mode integration).
- **Streaming**: Real-time streaming via MCP (WebSocket).
- **Auto Apply**: Automatically insert/apply analysis results to Emacs buffers.
- **Buffer Integration**: Inline suggestions (Copilot-style).

For details, refer to the source code (doge-code.el, doge-mcp.el) or the repository. For issues, see Doge-Code's issue tracker.

---

*Generated by Doge-Code Agent on [current date].*
