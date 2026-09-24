# Remote MCP modernization notes

Doge-Code now uses the stable `rmcp` 3.x client/server boundary (currently
`rmcp 3.4.1`; see the official [SDK changelog](https://github.com/modelcontextprotocol/rust-sdk/blob/main/CHANGELOG.md)).
The SDK negotiates supported protocol revisions. Streamable HTTP probes the MCP
2026-07-28 lifecycle and falls back to legacy negotiation for older servers;
stdio uses the established initialize lifecycle for predictable legacy
compatibility. Doge-Code does not maintain a second protocol-version fork.

## Configuration

Use structured stdio configuration for new servers:

```toml
[[mcp_servers]]
name = "filesystem"
enabled = true
transport = "stdio"
command = "/opt/My MCP Server/bin/server"
args = ["--config", "/tmp/foo config.json"]

connect_timeout_ms = 30000
list_timeout_ms = 10000
call_timeout_ms = 120000

[mcp_servers.env]
LOG_LEVEL = "info"
```

`command` is executed directly and `args` are passed as an argv array. No
shell is involved. The old `address = "server --foo bar"` stdio form remains a
deprecated compatibility fallback and is split only on whitespace; migrate
it to `command` plus `args` so paths and arguments containing spaces work.

HTTP endpoints continue to use `address`:

```toml
[[mcp_servers]]
name = "remote"
enabled = true
transport = "http"
address = "https://example.com/mcp"
```

A timeout of `0` means unlimited for the individual operation. HTTP uses the
legacy initialize lifecycle in this mode so rmcp's fixed modern-discovery probe
does not impose a hidden connection deadline. During registry construction,
an unlimited server that does not complete within a five-second
registration grace period is recorded as failed rather than blocking all other
tools. Project configuration overrides global fields, and project environment
keys override matching global keys while retaining unrelated global keys.
Ambiguous stdio configuration (both `command` and `address`) and HTTP entries
with stdio-only fields are rejected. Enabled server names must be unique.

## Result semantics

A successful JSON-RPC request does not imply a successful tool. For a
completed `CallToolResult`, `is_error == true` becomes
`ToolOutput.is_success == false`; `false` and legacy-absent `None` become
success. Protocol, transport, timeout, and cancellation failures remain typed
client errors. Completed content blocks and arbitrary `structuredContent` are
normalized into bounded JSON.

`InputRequired` and `Task` responses are recognized but reported as explicit
unsupported results until Doge-Code has an elicitation UI and a durable task
manager. No Tasks or elicitation capability is advertised in the meantime.

## Operational boundaries

- `tools/list` follows all SDK pagination cursors.
- Tool aliases are deterministic and collision-safe.
- A `tools/list_changed` notification invalidates the next registry snapshot.
- Connect, list, and call timeouts are independently configurable.
- Agent cancellation is propagated to remote waits and is not converted into a
  tool-level failure.
- A failed call is never automatically retried. A later invocation may
  reconnect an unhealthy server, but it never replays the failed call.
- Arguments, full results, environment values, and credentials are not logged
  verbatim; child stderr lines are bounded.

## Deferred follow-ups

These are intentionally outside this migration:

- MCP Tasks integration with a durable `BackgroundJobManager`.
- MCP `InputRequired` / elicitation UI and response flow.
- MCP OAuth and credential storage.
- RepoMap single-flight coordination across the remaining runtime paths.
- ACP Agent Server support.
