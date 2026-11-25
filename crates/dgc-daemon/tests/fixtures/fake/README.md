# Fake provider scenarios

`dgc-fake-provider <scenario.jsonl> [provider args...]` replays a scenario over
stdin/stdout. Adapters use it as their program, with the scenario path first and
their normal provider args after it. Blank lines and lines starting with `//`
are skipped. Each other line is one JSON object:

| Line | Effect |
|---|---|
| `{"expect_args": ["--resume", "${ANY}"]}` | Must be the first line. The strings must appear in the provider args as an ordered subsequence; `"${ANY}"` matches any one arg. Mismatch: exit 3. |
| `{"expect": <pattern>}` | Waits for the next stdin line. Subset match: every pattern key must exist and match; arrays match element-wise with equal length; scalars are equal; `"${ANY}"` matches anything; `"${name}"` matches only the value captured as `name`. Non-JSON line or mismatch: diff on stderr, exit 3. |
| `{"capture": "name", "path": "/a/0/b"}` | Stores the value at that JSON pointer in the last matched `expect` line. |
| `{"send": <json>}` | Writes one compact JSON line to stdout. A JSON string that is exactly `"${name}"` is replaced by the captured value (unknown name: exit 2). |
| `{"stderr": "text"}` | Writes the text and a newline to stderr. |
| `{"sleep_ms": n}` | Sleeps. |
| `{"exit": code}` | Exits with `code`. |
| `{"expect_eof": true}` | Ignores input until stdin closes. |

If the scenario ends without `exit`, the process waits for stdin EOF and exits 0.

Exit codes: 2 scenario or usage error, 3 mismatch, 4 timeout, 5 stdio failure.
Every wait (`expect`, `expect_eof`, final EOF) times out after 10 s, or
`DGC_FAKE_TIMEOUT_MS`.
