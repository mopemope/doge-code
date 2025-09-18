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
