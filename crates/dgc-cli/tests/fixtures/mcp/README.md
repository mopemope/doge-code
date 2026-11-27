# 実クライアントの MCP 初期化

ローカルの `dgc mcp` に対する `initialize` から、`params.protocolVersion` と
`params.clientInfo` の `name`、`version` だけを抜き出した。

| ファイル | 記録 | 最初のメソッドと応答 |
|---|---|---|
| [claude-code-2.1.332.initialize.json](claude-code-2.1.332.initialize.json) | recorded 2026-11-27 Claude Code 2.1.332 | `server/discover` に `-32601` が返り、`initialize` で要求した `2025-11-25` が受理された |
| [codex-cli-0.172.0.initialize.json](codex-cli-0.172.0.initialize.json) | recorded 2026-11-27 Codex CLI 0.172.0 | 最初の `initialize` で要求した `2025-06-18` が受理された |
