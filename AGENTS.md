# Doge-Code Agent System Documentation

This document provides a comprehensive overview of the Doge-Code agent system, detailing its architecture, core components, and operational mechanisms. 
Doge-Code is a Rust-based CLI/TUI interactive AI coding agent that leverages OpenAI-compatible LLMs to autonomously read, analyze, search, and edit code across multiple programming languages.

## Project Overview

Doge-Code is a sophisticated coding agent that enables developers to interact with their codebase through natural language commands.
The system integrates static code analysis, LLM-powered reasoning, and autonomous task execution to provide an intelligent development assistant.

### Basic Information

- **Language**: Rust (Edition 2024)
- **Architecture**: Modular design with clear separation of concerns
- **LLM Compatibility**: OpenAI-compatible APIs
- **License**: MIT/Apache-2.0
- **Version**: 0.1.0

## Key Features

### 1. Static Analysis Driven

- Utilizes tree-sitter for multi-language source code parsing
- Provides high-accuracy context to the LLM through symbol extraction
- Extracts key symbols (classes, functions, variables) and positional information
- Implements caching mechanisms for efficient processing

### 2. High Performance

- Leverages tokio for fast asynchronous processing
- Optimizes processing and caching to minimize response times
- Eliminates unnecessary UI animations
- Optimizes file I/O and LLM requests
- Implements context-aware prompt design

### 3. Context Efficiency

- Partitions LLM prompts for construction
- Separates system messages, static analysis context, user instructions, and conversation history
- Reduces API costs and response times

### 4. Autonomous Task Execution

- Enables agents to think and autonomously execute necessary actions
- Implements loop processing for continuous task execution
- Decomposes complex tasks for step-by-step execution

### 5. Function Calling for Actions

- Leverages LLM function calling (Tool Use) capabilities
- Ensures reliable file reading, writing, and editing
- Implements feedback loops for tool execution results

## Core Architecture

### Main Components

1. **TUI (Terminal User Interface)**
   - Provides an interactive terminal interface using crossterm and ratatui
   - Handles user input, command processing, and output display
   - Manages session state and UI rendering

2. **LLM Client**
   - Interfaces with OpenAI-compatible APIs
   - Manages chat history and token usage
   - Implements streaming responses and tool calling

3. **Analysis System**
   - Performs static code analysis using tree-sitter
   - Builds and maintains a repository map (repomap) of code symbols
   - Caches analysis results for performance

4. **Tool System**
   - Provides a comprehensive set of tools for the LLM to interact with the filesystem
   - Includes file operations, code search, execution, and editing capabilities
   - Manages tool execution and result reporting

5. **Session Management**
   - Handles persistent conversation history
   - Manages session creation, loading, and deletion
   - Tracks session metadata and statistics

6. **Planning System**
   - Analyzes tasks and creates execution plans
   - Decomposes complex tasks into manageable steps
   - Manages plan execution and tracking

## Detailed Component Analysis

### TUI System

The TUI system provides the interactive interface for Doge-Code. It's built using crossterm for low-level terminal operations and ratatui for widget rendering.

#### Key Features:
- Real-time streaming output with status indicators
- Input history persistence
- File completion with @ symbol
- Theme support (dark/light)
- Scrollable log display
- Session management interface

#### State Management:
- Tracks application status (Idle, Streaming, Processing, etc.)
- Manages conversation history and log display
- Handles input modes (Normal, Shell, SessionList)
- Maintains todo list state

### LLM Client

The LLM client module handles all interactions with OpenAI-compatible APIs. It manages chat history, token counting, and implements retry logic.

#### Key Features:
- Streaming chat completions with tool use support
- Automatic retry with exponential backoff
- Token usage tracking and reporting
- Context length management
- Cancellation support

#### Core Components:
- `OpenAIClient`: Main client implementation
- `ChatHistory`: Manages conversation context
- `ChatRequestWithTools`: Request structure with tool definitions
- `run_agent_loop`: Main processing loop for agent interactions

### Analysis System

The analysis system uses tree-sitter to parse source code and build a repository map. This provides the LLM with accurate context about the codebase.

#### Key Components:
- `Analyzer`: Main analysis orchestrator
- `RepoMap`: Repository symbol map
- `SymbolInfo`: Information about individual symbols
- Language-specific collectors (Rust, TypeScript, Python, etc.)
- Caching system for performance

#### Process:
1. File discovery with .gitignore support
2. Parallel parsing using tree-sitter
3. Symbol extraction and processing
4. Cache management for incremental updates

### Tool System

The tool system provides the LLM with capabilities to interact with the filesystem and execute operations.

#### Available Tools:
- **Filesystem Operations**: `fs_list`, `fs_read`, `fs_write`, `fs_read_many_files`
- **Search**: `search_text`, `find_file`, `search_repomap`
- **Execution**: `execute_bash`
- **Editing**: `edit`, `apply_patch`
- **Planning**: `todo_write`, `todo_read`

#### Implementation:
- Each tool is implemented as a method in `FsTools`
- Tools are registered with the LLM client
- Results are returned in a structured format
- Session tracking for tool usage statistics

### Session Management

Session management provides persistent conversation history and session tracking.

#### Key Features:
- Automatic session creation
- Session persistence using JSON files
- Session metadata tracking (tokens, requests, tool calls)
- Session listing and switching

#### Components:
- `SessionManager`: Main session management interface
- `SessionData`: Session information and conversation history
- `SessionStore`: Handles session persistence
- `SessionMeta`: Session metadata

## Technical Stack

### Core Technologies

- **Rust**: Edition 2024
- **Async Runtime**: tokio
- **TUI**: crossterm, ratatui
- **Error Handling**: thiserror, anyhow
- **Logging**: tracing, tracing-subscriber

### External Integrations

- **LLM APIs**: OpenAI-compatible endpoints
- **Static Analysis**: tree-sitter with language-specific parsers
- **Serialization**: serde, serde_json
- **Database**: SeaORM with SQLite for persistence

## Safety Features

- **Project Root Confinement**: All file operations are restricted to the project directory
- **Binary File Detection**: Automatic skipping of binary files in search operations
- **Path Normalization**: Safe resolution of relative and absolute paths
- **Command Allowlisting**: Configurable allowlist for shell command execution
- **Graceful Error Handling**: Comprehensive error reporting and recovery

## Development Notes

### Coding Conventions

- Uses Rust Edition 2024
- Follows modern Rust patterns and idioms
- Code formatting with rustfmt
- Linting with clippy
- Modular design with clear separation of concerns

The guidelines are as follows:

- Before executing a task, always investigate the current implementation, reorganize the task,
  create an implementation plan, and then start the task.
- Always execute shell commands in the foreground and include the results in the context.
- Write code comments as much as possible, and always use English as the language.
- Do not rush to conclusions, but think deeply and prioritize accuracy.
- Think step-by-step and execute the task.

### Testing

- Comprehensive test suite with unit tests
- Integration tests for key components
- Mocking for external dependencies

This documentation provides a comprehensive overview of the Doge-Code agent system. For detailed implementation specifics, please refer to the source code and inline documentation.

<!-- BACKLOG.MD GUIDELINES START -->
# Instructions for the usage of Backlog.md CLI Tool

## Backlog.md: Comprehensive Project Management Tool via CLI

### Assistant Objective

Efficiently manage all project tasks, status, and documentation using the Backlog.md CLI, ensuring all project metadata
remains fully synchronized and up-to-date.

### Core Capabilities

- ✅ **Task Management**: Create, edit, assign, prioritize, and track tasks with full metadata
- ✅ **Acceptance Criteria**: Granular control with add/remove/check/uncheck by index
- ✅ **Board Visualization**: Terminal-based Kanban board (`backlog board`) and web UI (`backlog browser`)
- ✅ **Git Integration**: Automatic tracking of task states across branches
- ✅ **Dependencies**: Task relationships and subtask hierarchies
- ✅ **Documentation & Decisions**: Structured docs and architectural decision records
- ✅ **Export & Reporting**: Generate markdown reports and board snapshots
- ✅ **AI-Optimized**: `--plain` flag provides clean text output for AI processing

### Why This Matters to You (AI Agent)

1. **Comprehensive system** - Full project management capabilities through CLI
2. **The CLI is the interface** - All operations go through `backlog` commands
3. **Unified interaction model** - You can use CLI for both reading (`backlog task 1 --plain`) and writing (
   `backlog task edit 1`)
4. **Metadata stays synchronized** - The CLI handles all the complex relationships

### Key Understanding

- **Tasks** live in `backlog/tasks/` as `task-<id> - <title>.md` files
- **You interact via CLI only**: `backlog task create`, `backlog task edit`, etc.
- **Use `--plain` flag** for AI-friendly output when viewing/listing
- **Never bypass the CLI** - It handles Git, metadata, file naming, and relationships

---

# ⚠️ CRITICAL: NEVER EDIT TASK FILES DIRECTLY. Edit Only via CLI

**ALL task operations MUST use the Backlog.md CLI commands**

- ✅ **DO**: Use `backlog task edit` and other CLI commands
- ✅ **DO**: Use `backlog task create` to create new tasks
- ✅ **DO**: Use `backlog task edit <id> --check-ac <index>` to mark acceptance criteria
- ❌ **DON'T**: Edit markdown files directly
- ❌ **DON'T**: Manually change checkboxes in files
- ❌ **DON'T**: Add or modify text in task files without using CLI

**Why?** Direct file editing breaks metadata synchronization, Git tracking, and task relationships.

---

## 1. Source of Truth & File Structure

### 📖 **UNDERSTANDING** (What you'll see when reading)

- Markdown task files live under **`backlog/tasks/`** (drafts under **`backlog/drafts/`**)
- Files are named: `task-<id> - <title>.md` (e.g., `task-42 - Add GraphQL resolver.md`)
- Project documentation is in **`backlog/docs/`**
- Project decisions are in **`backlog/decisions/`**

### 🔧 **ACTING** (How to change things)

- **All task operations MUST use the Backlog.md CLI tool**
- This ensures metadata is correctly updated and the project stays in sync
- **Always use `--plain` flag** when listing or viewing tasks for AI-friendly text output

---

## 2. Common Mistakes to Avoid

### ❌ **WRONG: Direct File Editing**

```markdown
# DON'T DO THIS:

1. Open backlog/tasks/task-7 - Feature.md in editor
2. Change "- [ ]" to "- [x]" manually
3. Add notes directly to the file
4. Save the file
```

### ✅ **CORRECT: Using CLI Commands**

```bash
# DO THIS INSTEAD:
backlog task edit 7 --check-ac 1  # Mark AC #1 as complete
backlog task edit 7 --notes "Implementation complete"  # Add notes
backlog task edit 7 -s "In Progress" -a @agent-k  # Multiple commands: change status and assign the task when you start working on the task
```

---

## 3. Understanding Task Format (Read-Only Reference)

⚠️ **FORMAT REFERENCE ONLY** - The following sections show what you'll SEE in task files.
**Never edit these directly! Use CLI commands to make changes.**

### Task Structure You'll See

```markdown
---
id: task-42
title: Add GraphQL resolver
status: To Do
assignee: [@sara]
labels: [backend, api]
---

## Description

Brief explanation of the task purpose.

## Acceptance Criteria

<!-- AC:BEGIN -->

- [ ] #1 First criterion
- [x] #2 Second criterion (completed)
- [ ] #3 Third criterion

<!-- AC:END -->

## Implementation Plan

1. Research approach
2. Implement solution

## Implementation Notes

Summary of what was done.
```

### How to Modify Each Section

| What You Want to Change | CLI Command to Use                                       |
|-------------------------|----------------------------------------------------------|
| Title                   | `backlog task edit 42 -t "New Title"`                    |
| Status                  | `backlog task edit 42 -s "In Progress"`                  |
| Assignee                | `backlog task edit 42 -a @sara`                          |
| Labels                  | `backlog task edit 42 -l backend,api`                    |
| Description             | `backlog task edit 42 -d "New description"`              |
| Add AC                  | `backlog task edit 42 --ac "New criterion"`              |
| Check AC #1             | `backlog task edit 42 --check-ac 1`                      |
| Uncheck AC #2           | `backlog task edit 42 --uncheck-ac 2`                    |
| Remove AC #3            | `backlog task edit 42 --remove-ac 3`                     |
| Add Plan                | `backlog task edit 42 --plan "1. Step one\n2. Step two"` |
| Add Notes (replace)     | `backlog task edit 42 --notes "What I did"`              |
| Append Notes            | `backlog task edit 42 --append-notes "Another note"` |

---

## 4. Defining Tasks

### Creating New Tasks

**Always use CLI to create tasks:**

```bash
# Example
backlog task create "Task title" -d "Description" --ac "First criterion" --ac "Second criterion"
```

### Title (one liner)

Use a clear brief title that summarizes the task.

### Description (The "why")

Provide a concise summary of the task purpose and its goal. Explains the context without implementation details.

### Acceptance Criteria (The "what")

**Understanding the Format:**

- Acceptance criteria appear as numbered checkboxes in the markdown files
- Format: `- [ ] #1 Criterion text` (unchecked) or `- [x] #1 Criterion text` (checked)

**Managing Acceptance Criteria via CLI:**

⚠️ **IMPORTANT: How AC Commands Work**

- **Adding criteria (`--ac`)** accepts multiple flags: `--ac "First" --ac "Second"` ✅
- **Checking/unchecking/removing** accept multiple flags too: `--check-ac 1 --check-ac 2` ✅
- **Mixed operations** work in a single command: `--check-ac 1 --uncheck-ac 2 --remove-ac 3` ✅

```bash
# Examples

# Add new criteria (MULTIPLE values allowed)
backlog task edit 42 --ac "User can login" --ac "Session persists"

# Check specific criteria by index (MULTIPLE values supported)
backlog task edit 42 --check-ac 1 --check-ac 2 --check-ac 3  # Check multiple ACs
# Or check them individually if you prefer:
backlog task edit 42 --check-ac 1    # Mark #1 as complete
backlog task edit 42 --check-ac 2    # Mark #2 as complete

# Mixed operations in single command
backlog task edit 42 --check-ac 1 --uncheck-ac 2 --remove-ac 3

# ❌ STILL WRONG - These formats don't work:
# backlog task edit 42 --check-ac 1,2,3  # No comma-separated values
# backlog task edit 42 --check-ac 1-3    # No ranges
# backlog task edit 42 --check 1         # Wrong flag name

# Multiple operations of same type
backlog task edit 42 --uncheck-ac 1 --uncheck-ac 2  # Uncheck multiple ACs
backlog task edit 42 --remove-ac 2 --remove-ac 4    # Remove multiple ACs (processed high-to-low)
```

**Key Principles for Good ACs:**

- **Outcome-Oriented:** Focus on the result, not the method.
- **Testable/Verifiable:** Each criterion should be objectively testable
- **Clear and Concise:** Unambiguous language
- **Complete:** Collectively cover the task scope
- **User-Focused:** Frame from end-user or system behavior perspective

Good Examples:

- "User can successfully log in with valid credentials"
- "System processes 1000 requests per second without errors"
- "CLI preserves literal newlines in description/plan/notes; `\\n` sequences are not auto‑converted"

Bad Example (Implementation Step):

- "Add a new function handleLogin() in auth.ts"
- "Define expected behavior and document supported input patterns"

### Task Breakdown Strategy

1. Identify foundational components first
2. Create tasks in dependency order (foundations before features)
3. Ensure each task delivers value independently
4. Avoid creating tasks that block each other

### Task Requirements

- Tasks must be **atomic** and **testable** or **verifiable**
- Each task should represent a single unit of work for one PR
- **Never** reference future tasks (only tasks with id < current task id)
- Ensure tasks are **independent** and don't depend on future work

---

## 5. Implementing Tasks

### 5.1. First step when implementing a task

The very first things you must do when you take over a task are:

* set the task in progress
* assign it to yourself

```bash
# Example
backlog task edit 42 -s "In Progress" -a @{myself}
```

### 5.2. Create an Implementation Plan (The "how")

Previously created tasks contain the why and the what. Once you are familiar with that part you should think about a
plan on **HOW** to tackle the task and all its acceptance criteria. This is your **Implementation Plan**.
First do a quick check to see if all the tools that you are planning to use are available in the environment you are
working in.   
When you are ready, write it down in the task so that you can refer to it later.

```bash
# Example
backlog task edit 42 --plan "1. Research codebase for references\n2Research on internet for similar cases\n3. Implement\n4. Test"
```

## 5.3. Implementation

Once you have a plan, you can start implementing the task. This is where you write code, run tests, and make sure
everything works as expected. Follow the acceptance criteria one by one and MARK THEM AS COMPLETE as soon as you
finish them.

### 5.4 Implementation Notes (PR description)

When you are done implementing a tasks you need to prepare a PR description for it.
Because you cannot create PRs directly, write the PR as a clean description in the task notes.
Append notes progressively during implementation using `--append-notes`:

```
backlog task edit 42 --append-notes "Implemented X" --append-notes "Added tests"
```

```bash
# Example
backlog task edit 42 --notes "Implemented using pattern X because Reason Y, modified files Z and W"
```

**IMPORTANT**: Do NOT include an Implementation Plan when creating a task. The plan is added only after you start the
implementation.

- Creation phase: provide Title, Description, Acceptance Criteria, and optionally labels/priority/assignee.
- When you begin work, switch to edit, set the task in progress and assign to yourself
  `backlog task edit <id> -s "In Progress" -a "..."`.
- Think about how you would solve the task and add the plan: `backlog task edit <id> --plan "..."`.
- Add Implementation Notes only after completing the work: `backlog task edit <id> --notes "..."` (replace) or append progressively using `--append-notes`.

## Phase discipline: What goes where

- Creation: Title, Description, Acceptance Criteria, labels/priority/assignee.
- Implementation: Implementation Plan (after moving to In Progress and assigning to yourself).
- Wrap-up: Implementation Notes (Like a PR description), AC and Definition of Done checks.

**IMPORTANT**: Only implement what's in the Acceptance Criteria. If you need to do more, either:

1. Update the AC first: `backlog task edit 42 --ac "New requirement"`
2. Or create a new follow up task: `backlog task create "Additional feature"`

---

## 6. Typical Workflow

```bash
# 1. Identify work
backlog task list -s "To Do" --plain

# 2. Read task details
backlog task 42 --plain

# 3. Start work: assign yourself & change status
backlog task edit 42 -s "In Progress" -a @myself

# 4. Add implementation plan
backlog task edit 42 --plan "1. Analyze\n2. Refactor\n3. Test"

# 5. Work on the task (write code, test, etc.)

# 6. Mark acceptance criteria as complete (supports multiple in one command)
backlog task edit 42 --check-ac 1 --check-ac 2 --check-ac 3  # Check all at once
# Or check them individually if preferred:
# backlog task edit 42 --check-ac 1
# backlog task edit 42 --check-ac 2
# backlog task edit 42 --check-ac 3

# 7. Add implementation notes (PR Description)
backlog task edit 42 --notes "Refactored using strategy pattern, updated tests"

# 8. Mark task as done
backlog task edit 42 -s Done
```

---

## 7. Definition of Done (DoD)

A task is **Done** only when **ALL** of the following are complete:

### ✅ Via CLI Commands:

1. **All acceptance criteria checked**: Use `backlog task edit <id> --check-ac <index>` for each
2. **Implementation notes added**: Use `backlog task edit <id> --notes "..."`
3. **Status set to Done**: Use `backlog task edit <id> -s Done`

### ✅ Via Code/Testing:

4. **Tests pass**: Run test suite and linting
5. **Documentation updated**: Update relevant docs if needed
6. **Code reviewed**: Self-review your changes
7. **No regressions**: Performance, security checks pass

⚠️ **NEVER mark a task as Done without completing ALL items above**

---

## 8. Quick Reference: DO vs DON'T

### Viewing Tasks

| Task         | ✅ DO                        | ❌ DON'T                         |
|--------------|-----------------------------|---------------------------------|
| View task    | `backlog task 42 --plain`   | Open and read .md file directly |
| List tasks   | `backlog task list --plain` | Browse backlog/tasks folder     |
| Check status | `backlog task 42 --plain`   | Look at file content            |

### Modifying Tasks

| Task          | ✅ DO                                 | ❌ DON'T                           |
|---------------|--------------------------------------|-----------------------------------|
| Check AC      | `backlog task edit 42 --check-ac 1`  | Change `- [ ]` to `- [x]` in file |
| Add notes     | `backlog task edit 42 --notes "..."` | Type notes into .md file          |
| Change status | `backlog task edit 42 -s Done`       | Edit status in frontmatter        |
| Add AC        | `backlog task edit 42 --ac "New"`    | Add `- [ ] New` to file           |

---

## 9. Complete CLI Command Reference

### Task Creation

| Action           | Command                                                                             |
|------------------|-------------------------------------------------------------------------------------|
| Create task      | `backlog task create "Title"`                                                       |
| With description | `backlog task create "Title" -d "Description"`                                      |
| With AC          | `backlog task create "Title" --ac "Criterion 1" --ac "Criterion 2"`                 |
| With all options | `backlog task create "Title" -d "Desc" -a @sara -s "To Do" -l auth --priority high` |
| Create draft     | `backlog task create "Title" --draft`                                               |
| Create subtask   | `backlog task create "Title" -p 42`                                                 |

### Task Modification

| Action           | Command                                     |
|------------------|---------------------------------------------|
| Edit title       | `backlog task edit 42 -t "New Title"`       |
| Edit description | `backlog task edit 42 -d "New description"` |
| Change status    | `backlog task edit 42 -s "In Progress"`     |
| Assign           | `backlog task edit 42 -a @sara`             |
| Add labels       | `backlog task edit 42 -l backend,api`       |
| Set priority     | `backlog task edit 42 --priority high`      |

### Acceptance Criteria Management

| Action              | Command                                                                     |
|---------------------|-----------------------------------------------------------------------------|
| Add AC              | `backlog task edit 42 --ac "New criterion" --ac "Another"`                  |
| Remove AC #2        | `backlog task edit 42 --remove-ac 2`                                        |
| Remove multiple ACs | `backlog task edit 42 --remove-ac 2 --remove-ac 4`                          |
| Check AC #1         | `backlog task edit 42 --check-ac 1`                                         |
| Check multiple ACs  | `backlog task edit 42 --check-ac 1 --check-ac 3`                            |
| Uncheck AC #3       | `backlog task edit 42 --uncheck-ac 3`                                       |
| Mixed operations    | `backlog task edit 42 --check-ac 1 --uncheck-ac 2 --remove-ac 3 --ac "New"` |

### Task Content

| Action           | Command                                                  |
|------------------|----------------------------------------------------------|
| Add plan         | `backlog task edit 42 --plan "1. Step one\n2. Step two"` |
| Add notes        | `backlog task edit 42 --notes "Implementation details"`  |
| Add dependencies | `backlog task edit 42 --dep task-1 --dep task-2`         |

### Multi‑line Input (Description/Plan/Notes)

The CLI preserves input literally. Shells do not convert `\n` inside normal quotes. Use one of the following to insert real newlines:

- Bash/Zsh (ANSI‑C quoting):
  - Description: `backlog task edit 42 --desc $'Line1\nLine2\n\nFinal'`
  - Plan: `backlog task edit 42 --plan $'1. A\n2. B'`
  - Notes: `backlog task edit 42 --notes $'Done A\nDoing B'`
  - Append notes: `backlog task edit 42 --append-notes $'Progress update line 1\nLine 2'`
- POSIX portable (printf):
  - `backlog task edit 42 --notes "$(printf 'Line1\nLine2')"`
- PowerShell (backtick n):
  - `backlog task edit 42 --notes "Line1`nLine2"`

Do not expect `"...\n..."` to become a newline. That passes the literal backslash + n to the CLI by design.

Descriptions support literal newlines; shell examples may show escaped `\\n`, but enter a single `\n` to create a newline.

### Task Operations

| Action             | Command                                      |
|--------------------|----------------------------------------------|
| View task          | `backlog task 42 --plain`                    |
| List tasks         | `backlog task list --plain`                  |
| Filter by status   | `backlog task list -s "In Progress" --plain` |
| Filter by assignee | `backlog task list -a @sara --plain`         |
| Archive task       | `backlog task archive 42`                    |
| Demote to draft    | `backlog task demote 42`                     |

---

## Common Issues

| Problem              | Solution                                                           |
|----------------------|--------------------------------------------------------------------|
| Task not found       | Check task ID with `backlog task list --plain`                     |
| AC won't check       | Use correct index: `backlog task 42 --plain` to see AC numbers     |
| Changes not saving   | Ensure you're using CLI, not editing files                         |
| Metadata out of sync | Re-edit via CLI to fix: `backlog task edit 42 -s <current-status>` |

---

## Remember: The Golden Rule

**🎯 If you want to change ANYTHING in a task, use the `backlog task edit` command.**
**📖 Use CLI to read tasks, exceptionally READ task files directly, never WRITE to them.**

Full help available: `backlog --help`

<!-- BACKLOG.MD GUIDELINES END -->
