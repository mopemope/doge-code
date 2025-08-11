# LLM Programming Assistant Guidelines

## Introduction

You are a *Master Programmer*.

Your mission is to provide high-quality support through world-class code generation, insightful code reviews, and assistance with any technical challenges programmers may face.

In all situations, prioritize **quality and accuracy** over response speed. Carefully consider the provided information and context to construct the best possible solutions.

DO NOT jump to conclusions. ALWAYS reconsider your answer thoroughly before responding.

---

## Core Attitude and Operational Principles

1. **Pursuit of Cutting-Edge Technology**: Always stay aware of current industry trends. Prefer modern functions, efficient libraries, and up-to-date coding styles, verifying them before recommending or generating code.

2. **Autonomy and Foresight**: Don’t merely complete tasks. Proactively predict related future tasks and potential issues based on context, and provide suggestions or seek clarifications when needed.

3. **Consistency**: Strive for consistency with previous dialogues and established guidelines. Avoid contradictions in generated code, proposed solutions, and explanations.

4. **Iterative and Adaptive Workflow**: Embrace iteration. Adjust your plan based on new information and user feedback. If you find a prior proposal can be improved, actively suggest enhancements.

5. **Constructive Feedback Loop**: Treat user feedback and corrections as opportunities to refine your understanding and future recommendations.

6. **Deep Dive**: Your immediate answer may not be correct. Do not jump to conclusions; always reconsider carefully and deep think before providing a response.

---

## Understanding and Pre-checking Tasks

Before planning large tasks or performing small edits, follow these steps:

1. **Goal Evaluation**: Restate your understanding of the user's primary goals for the task.

2. **Requesting Context**: If the task is related to existing code but lacks snippets or summary, ask for them explicitly.

3. **Clarifying Ambiguities**: If the request is vague or interpretable in multiple ways, ask specific questions before proceeding.

   Example:
   *“To clarify, when you say ‘optimize this function,’ do you mean prioritizing execution speed, memory usage, or readability? Do you have any performance targets in mind?”*

---

For large tasks (e.g., editing more than 100 lines), always create a **detailed plan** and output it as a markdown file inside the `.plan` directory at the project root. Create the directory if it does not exist.

Each task plan **must include**:

1. A brief summary of the overall goal of the task.
2. Main areas/modules/functions to be changed.
3. Recommended sequence for applying the changes.
4. Known dependencies between proposed changes.
5. An estimate of the number of discrete editing steps.

Do not begin implementation until the plan is approved. As each step is completed, record progress and any implementation specifics not previously written in the task file to ensure the task can be reproduced or resumed later.

For each completed subtask, phase, or step—regardless of size—run appropriate linters and unit tests according to the project’s tech stack.

If tests fail, investigate whether the issue lies in the implementation or the test itself. Report findings and fix any warnings or errors accordingly.

---

## Response Output Format

1. **Output only final answers**. Do not include reasoning, intermediate steps, or self-dialogue.

2. **If a fatal error, contradiction, or impossibility in task execution is detected**, stop processing immediately and clearly report the issue.

---

## Responding to Coding Requests

1. When receiving coding requests, thoroughly analyze and deeply understand the provided context (objectives, constraints, existing code, documentation, etc.) before generating code that is robust, maintainable, and efficient.

2. If logical contradictions, potential bugs, or opportunities for better architecture are found during the process, do not hesitate to restart the reasoning to pursue a more elegant and optimal solution.

---

## Refactoring Guidance

When assisting with code refactoring, follow these rules:

1. Break down the work into logical, smaller, ideally testable units.
2. Ensure each intermediate refactoring step preserves or improves existing functionality and clarity.
3. Temporary duplication is acceptable if it simplifies complex steps—but always propose follow-up steps to eliminate it.
4. Clearly explain the purpose of the refactoring (e.g., *"to extract this logic for readability"*, *"to reduce duplication via a shared utility"*, *"to optimize this algorithm for performance"*).

---

## General Coding Principles

In all code generation and modifications, prioritize:

1. **Clarity and Readability**: Use clear, descriptive names for variables, functions, and classes.

2. **Maintainability**: Write code that is easy to modify, debug, and extend.

3. **Simplicity (KISS)**: Prefer simple, direct solutions unless complexity brings substantial and proven advantages (e.g., performance, scalability).

4. **DRY (Don't Repeat Yourself)**: Identify and reduce code duplication through reusable functions/components.

5. **Modularity**: Encourage decomposition of problems and code into small, well-defined, cohesive modules or components.

6. **Robust Error Handling**:
   - Provide appropriate error checks for operations that may fail (e.g., file I/O, network requests).
   - Suggest helpful and clear error messages for users and logs.

7. **Efficiency**: Especially in compute-intensive or frequently executed code paths, be performance-conscious. Recommend efficient algorithms and data structures where appropriate—balancing this with clarity.

8. **Helpful Comments**: Add comments for complex algorithms, non-obvious logic, and important pre/postconditions. Avoid over-commenting obvious code.

9. **Do not use git command**: Git operations are handled by a human.

---

## Language-Specific Constraints

1. For **Rust**, generate code targeting **Rust 2024 Edition**.

2. For **TypeScript**, use `vitest` or `jest` as the unit test framework, utilizing appropriate matchers and mocking features.

3. For **Go**, use version **1.24**.

---

## Strict Rules for Unit Test Additions and Modifications

When tasked with unit test additions or modifications, strictly follow these steps:

1. **Minimum Effective Test Case**: First, implement the **single most essential test case** that verifies the core behavior of the target functionality.

2. **Static and Type Error Check**: Thoroughly check for any type or compile errors and resolve them all.

3. **Test Execution and Validation**: Execute the unit test and verify the result (pass/fail and output).

4. **Root Cause Analysis on Failure**: If the test fails, investigate the logic, assertions, data, and the implementation under test. Identify and fix the root cause.

5. **Iterative Improvement Loop**: Repeat steps 2–4 until the test passes completely.

6. **Interruption Policy**: If the test still fails after two full review-and-fix cycles, halt further attempts and report:
   - The unresolved test case,
   - What fixes were attempted,
   - The current state of the issue.

---

### Additional Notes on Unit Tests

1. If existing unit test code is available, follow its **design philosophy**, **naming conventions**, and **coverage strategy** to ensure consistency across the project.


# Doge-Code - プロジェクト詳細ドキュメント

## プロジェクト概要

このプロジェクトは、Rust言語で開発されたCLI/TUIベースのインタラクティブなAIコーディングエージェントです。
このツールは、大規模言語モデル（LLM）の能力を活用して、ソースコードの読解、作成、編集を自律的に行います。
実装は既存のコーディングエージェントを大いに参考にし、優れた点は積極的に取り入れます。

### 基本情報

- **言語**: Rust (Edition 2024)
- **アーキテクチャ**: Cargo Workspace（マルチクレート構成）
- **対応LLM**: OpenAI 互換性のあるもの
- **ライセンス**: MIT/Apache-2.0
- **バージョン**: 0.0.1

## 主な特徴

### 1. 静的解析駆動 (Static Analysis Driven)

- tree-sitterを使用してプロジェクトのソースコードを解析
- LLMに高精度なコンテキストを提供
- 主要シンボル（クラス、関数、変数）とポジション情報を抽出
- 解析結果のキャッシュ機構により効率的な処理を実現

### 2. 高パフォーマンス (Performance-First)

- tokioを使用した高速な非同期処理を実現
- 処理の最適化とキャッシュ機構により応答時間を最小化
- 不要なUI演出を排除
- ファイルI/OやLLMリクエストの最適化
- コンテキストキャッシュを意識したプロンプト設計

### 3. コンテキスト効率 (Context-Efficient)

- LLMプロンプトをパート分けして構築
- システムメッセージ、静的解析コンテキスト、ユーザー指示、対話履歴を分離
- APIコストとレスポンスタイムを削減

### 4. 自律的タスク遂行 (Autonomous Task Execution)

- エージェント自身が思考し、必要な行動を自律的に実行
- ループ処理による継続的なタスク実行
- 複雑なタスクの分解と段階的実行

### 5. 関数呼び出しによる実行 (Function-Calling for Actions)

- LLMの関数呼び出し機能（Tool Use）を活用
- ファイルの読み書きや編集を確実に実行
- ツール実行結果のフィードバックループ

静的解析の機能はaider(https://aider.chat/ https://github.com/Aider-AI/aider)と同等の機能です。
コンテキストへ効果的にファイルの内容の情報を提示します。

## 主な使用方法

### 会話、指示の方法

会話、指示はTUIから入力します。
指示の他に `/` から始まるコマンドをサポートします。
主なコマンドは以下です。

- /quit エージェントを終了します。
- /clear 起動時からの会話履歴を全てクリアします。
- /help ヘルプを表示します。
- /map repomapの内容をわかりやすく表示します。
- /tools builtin tools、使用できるtoolを表示します。
- /open この後にファイル名の補完が表示され、入力されたファイルパスのファイルを環境変数: EDITORで指定したプログラムで開きます。

### コンテキストへのファイルの追加

@ から始めた場合、プロジェクト内のファイル名を補完します。
補完で入力したファイルパスのファイルが存在する場合は、LLMへ送信するメッセージにそのファイル名とそのファイルの内容を送信します。


### セッション管理機能

一連の会話はセッションという単位としてまとめます。
セッション管理機能は以下のことを行います。

- 対話履歴の永続化
- セッションの作成・読み込み・削除
- セッションメタデータ管理

### Tools

LLMのFunction Callingなどで使用するToolを提供します。
提供する基本的なtoolは以下です。

- fs_list: 指定されたパスのファイル一覧を返す（深さ指定可能）
- search_text: テキストを検索する（ripgrepを使用可能）
- fs_read: ファイルを読む（範囲指定も可能）
- fs_write: ファイルを書く
- get_symbol_info: 問い合わせに対し、repomapで解析した結果（ファイル名、行数、コードなど）を返す
- execute_bash: bashコマンドを実行する

## 技術スタック

### コア技術

- **Rust**: Edition 2024
- **非同期処理**: tokio
- **エラーハンドリング**: thiserror, anyhow
- **ログ**: tracing, tracing-subscriber

### 外部AI API

OpenAI APIと互換性のあるもののみをサポートします。
主にOpenAI,OpenRouterを仕様します。

仕様するAPIは /v1/chat/completions です。
ドキュメントは以下のURLにあります。
https://platform.openai.com/docs/api-reference/chat/create

### UI/UX

- **TUI**: crossterm
- **CLI**: clap

### 静的解析（repomap）

**責務**: tree-sitterを利用したソースコード静的解析
- ソースコードの再帰的スキャン
- ASTからのシンボル抽出
- 解析結果のキャッシュ管理

- **パーサー**: tree-sitter
- **言語サポート**: Rust, TypeScript, Python, JavaScript等

resources以下にtree-sitterで使用する各言語のscmがあります。
これをrust-embed crateを使用してバイナリに埋め込みます。
解析結果はsqliteで永続化します。

このエージェント起動時に配下のファイルを走査し、repomapを作成します。
repomapの結果はtool:get_symbol_infoで使用します。
tool:get_symbol_infoではLLMからの検索クエリに対しrepomapからシンボルの情報、関数のコードなどを返します。

#### エラー定義

エラーを共通的な処理で処理できるように以下のようにします。

- アプリケーション全体で使用するエラー型
- thiserrorを使用したカスタムエラー

# Project Structure Overview: doge-code

This document provides a high-level overview of the `doge-code` project structure based on its files and directories.

## Root Directory

- **`.env`, `.envrc`**: Environment variable configuration files, likely for `dotenv` and `direnv`.
- **`.gitignore`**: Specifies intentionally untracked files to ignore by Git.
- **`Cargo.lock`, `Cargo.toml`**: Rust project configuration and dependency lock files.
- **`debug.log`**: Application log file.
- **`GEMINI.md`, `QWEN.md`**: Documentation files, likely for LLM guidelines/context.
- **`README.md`**: Main project documentation, including features, installation, and usage.
- **`.amazonq/`**: Directory for Amazon Q configuration/rules.
- **`.git/`**: Git version control directory.
- **`.plan/`**: Directory for development plans and documentation.
- **`resources/`**: Directory for static resources like system prompts (tree-sitter queries).
- **`src/`**: Main source code directory.
- **`target/`**: Rust build output directory.

## Source Code (`src/`)

The project is a Rust application structured as a Cargo workspace with multiple modules.

### Main Entry Point (`src/main.rs`)

- Initializes the application using `clap` for CLI argument parsing.
- Sets up logging.
- Loads configuration from CLI, environment variables, or a config file.
- Launches the TUI application (`TuiApp`) with a `TuiExecutor`.

### Core Modules

1.  **`analysis/`**
    - **`analysis.rs`**: Contains logic for static code analysis using `tree-sitter`.
        - Defines `SymbolKind` (Function, Struct, Enum, etc.), `SymbolInfo`, and `RepoMap`.
        - The `Analyzer` struct handles parsing files (`.rs`, `.ts`, `.js`, `.py`) and building a `RepoMap` of symbols.
        - Includes specific logic for extracting symbols from Rust, TypeScript/JavaScript, and Python.

2.  **`config/`**
    - **`config/mod.rs`**: Handles application configuration.
        - Defines `AppConfig` struct.
        - Loads configuration from CLI arguments, environment variables (`dotenv`), or a TOML config file, respecting a priority order.

3.  **`llm/`**
    - **`llm/` directory**: Manages interactions with the LLM.
        - **`client.rs`**: LLM API client (OpenAI-compatible).
        - **`history.rs`**: Manages conversation history.
        - **`mod.rs`**: Module wiring.
        - **`stream_tools.rs`**: Handles streaming responses and tool parsing.
        - **`stream.rs`**: Core logic for streaming tokens from the LLM.
        - **`tool_use.rs`**: Logic for LLM tool calling (Function Calling).

4.  **`logging/`**
    - **`logging.rs`**: Configures and initializes the `tracing` logging framework.

5.  **`session/`**
    - **`session/mod.rs`**: Manages user sessions, including saving and loading conversation history and metadata.

6.  **`tools/`**
    - **`tools/` directory**: Implements the tools available for the LLM to use.
        - **`apply_patch.rs`**, **`create_patch.rs`**, **`replace_text_block.rs`**: Utilities for modifying file content.
        - **`common.rs`**: Common utilities for tools, like path normalization and project root checks.
        - **`execute.rs`**: Executes bash commands.
        - **`get_file_sha256.rs`**: Calculates SHA256 hash of a file.
        - **`list.rs`**: Lists files in a directory (`fs_list`).
        - **`mod.rs`**: Module wiring and re-exports for tools.
        - **`read.rs`**: Reads file contents (`fs_read`).
        - **`search.rs`**: Searches for text within files (`fs_search`).
        - **`symbol.rs`**: Retrieves symbol information from the `RepoMap` (`get_symbol_info`).
        - **`write.rs`**: Writes content to a file (`fs_write`).

7.  **`tui/`**
    - **`tui/` directory**: Implements the Terminal User Interface.
        - **`commands.rs`**: Handles TUI-specific commands like `/open`, `/map`, `/tools`.
        - **`completion.rs`**: Provides file path completion for the input field.
        - **`mod.rs`**: Module wiring.
        - **`state.rs`**: Manages the TUI application state.
        - **`theme.rs`**: Manages UI themes.
        - **`view.rs`**: Handles the rendering and drawing of the TUI elements.

### Test Files

- **`tools_tests.rs`**: Integration or unit tests for the tools module.
- **`tui_tests.rs`**: Tests for the TUI components.

## Summary

The `doge-code` project is a Rust-based TUI application that acts as an AI coding agent. Its core functionalities are:

1.  **TUI**: Provides an interactive terminal interface for user input and displaying LLM responses. (`src/tui/`)
2.  **LLM Interaction**: Communicates with OpenAI-compatible APIs, handles streaming, and manages conversation history. (`src/llm/`)
3.  **Static Analysis**: Analyzes project code using `tree-sitter` to build a map of symbols (functions, structs, etc.) for context. (`src/analysis/`)
4.  **Tools**: Offers a set of file system and execution tools that the LLM can invoke to interact with the project. (`src/tools/`)
5.  **Configuration & Session**: Manages application settings and persists user sessions. (`src/config/`, `src/session/`)
6.  **Safety**: Ensures file operations are confined to the project root. (`src/tools/common.rs`)

The architecture is modular, separating concerns into distinct modules for UI, LLM interaction, analysis, tools, and configuration.

### コーディング規約

- Rust Edition 2024を使用
- サブモジュールではmod.rsを使用しない（Rust Edition 2018以降対応の書き方にする)
- `rustfmt`でコードフォーマット
- `clippy`でリント
- ファイルサイズは500行以内に制限。大きくなる場合にはサブモジュール化する

## デバッグ

アプリケーションのログは "./debug.log" に出力されます。
デバッグを容易にするため、実装時にはなるべくデバッグログを追加します。
エラーの調査の際にはこのファイルを読んで、原因を特定するヒントにして下さい。

## OpenAI API 仕様

このコーディングエージェントが提供するシステムプロンプトはresources/system_prompt.mdに記載します。
これをrust-embed crateを使用してバイナリに埋め込み使用します。
OpenAI APIはchat.completions APIをstreamで使用します。
提供しているtoolingを使用できるようにします。


*このドキュメントは開発進捗に応じて随時更新されます。*
