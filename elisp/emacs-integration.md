# Doge-Code Emacs Integration

## Overview

Doge-Code is an interactive CLI/TUI coding agent built in Rust that uses OpenAI-compatible LLMs to assist with code reading, analysis, searching, and editing. This document explains the integration features between Doge-Code and Emacs, allowing seamless use of AI assistance within Emacs.

The integration calls Doge-Code's `exec` subcommand as a subprocess from Emacs for code analysis, explanation and refactoring.

> The former Emacs MCP client (`doge-mcp.el`) and the Semantic HUD (`doge-hud.el`, which depended on it) were removed: they called a `/mcp/call_tool` route that `dgc mcp-server` does not serve (it speaks the standard MCP protocol at `/mcp`). To use the MCP server from an editor, connect an MCP-capable client to it (see the README).

After installation you can enable the minor mode wherever you want (e.g. by adding it to `prog-mode-hook`). The keybindings described below become available whenever `doge-code-mode` is active. The integration leverages Doge-Code's tool system (static code analysis, file operations, etc.) to assist with code analysis and explanation.

## Requirements

- **Doge-Code**: Built binary (`dgc`). Dependencies are defined in Cargo.toml.
- **Emacs**: Version 27.1 or higher.
  - CLI helper (`doge-code.el`): depends on the built-in `json`, `async`, and the third-party `popup` package.
- **API Key**: Set OpenAI-compatible API key in `OPENAI_API_KEY` environment variable.
- **Project Root**: Doge-Code operates within a project root. The directory of the Emacs buffer serves as the root.

## Installation and Setup

### 1. Build and Setup Doge-Code
1. Build the release binary: `cargo build --release`.
2. Ensure the binary (default: `dgc`) is in your `PATH`.

### 2. Emacs Configuration (Quick Start)
Add the following to your `init.el`. This single setup function automatically detects and enables available extensions (refactoring, Org-babel, etc.).

```elisp
;; Add doge-code elisp directory to load-path
(add-to-list 'load-path "/path/to/doge-code/elisp")
(require 'doge-code)

;; Optional: Customize behavior before setup
(setq doge-code-executable "dgc") 

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
- `C-c d e`: **Explain Region** - Get plain text explanation.
- `C-c d b`: **Analyze Buffer** - Analyze the entire file.
- `C-c d c`: **Cancel** - Stop the current Doge-Code process.

### Refactoring
- `C-c d R`: **Global Refactor** - Perform large-scale codebase refactoring (via `exec`).

The former inline rewrite (`C-c d r`), Flymake fix (`C-c d f`) and compilation
auto-fix helpers were removed together with the `dgc rewrite` and `dgc fix`
subcommands.

## Customization

You can control which features are enabled by setting these variables **before** calling `doge-code-setup`:

| Variable | Default | Description |
|----------|---------|-------------|
| `doge-code-enable-auto-mode` | `t` | Auto-enable `doge-code-mode` in `prog-mode`. |
| `doge-code-enable-refactor` | `t` | Enable git-aware refactoring tools. |

## Dependencies

Doge-Code intelligently handles missing packages. If an optional dependency is missing, that specific feature will be disabled with a message in the echo area, but the rest of Doge-Code will continue to work.

- **Core**: `json`, `async` (required)
- **Better UI**: `popup`

## Detailed Features

### 1. CLI-based Integration (MVI)
Asynchronously calls Doge-Code's CLI from Emacs: analysis and explanation flows use the `exec` subcommand. Results are parsed from JSON (`--json`) and displayed.

#### Commands
- **doge-code-analyze-region** (`C-c d a`):
  - Analyze selected region and display improvement suggestions.
  - Example: Select a function and analyze → Display "Code improvements: ..." in popup.
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
4. On error: Display "Doge-Code Error: ..." in message bar.

#### Customization
- `doge-code-executable`: Binary path (default: "dgc").
- `doge-code-use-popup`: Use popup display if t, else buffer display.
- `doge-code-show-progress`: Show progress messages during execution.
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

## Troubleshooting

- **"Doge-Code executable not found"**:
  - Ensure `dgc` is in your PATH.
  - Or set `(setq doge-code-executable "/full/path/to/target/release/dgc")` in Emacs.
- **"JSON parse error" / Raw Output**:
  - Often indicates the process panicked or the API key is missing. Check the `*doge-output*` buffer or the error message for details like "OPENAI_API_KEY not set".
- **Encoding Issues (Japanese characters)**:
  - The integration now explicitly decodes output as UTF-8. If garbled text persists, check your `process-coding-system-alist`.


## Future Enhancements

- **LSP Support**: Run Doge-Code as an LSP server (lsp-mode integration).
- **Auto Apply**: Automatically insert/apply analysis results to Emacs buffers.
- **Buffer Integration**: Inline suggestions (Copilot-style).

For details, refer to the source code (`elisp/`) or the repository. For issues, see Doge-Code's issue tracker.

---

*Generated by Doge-Code Agent on [current date].*
