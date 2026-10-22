# Claude Code MCP log fixtures

Real files from `~/.cache/claude-cli-nodejs/<project>/mcp-logs-<server>/<time>.jsonl`, written by Claude Code 2.1.308
on Linux for one `claude mcp get <server>` each, run in a project folder (renamed `/home/u/proj`; session ids zeroed).
A real session writes the same lines, then one line per tool call, to one file per server for the whole session.

| File | The server |
|---|---|
| `fail.jsonl` | prints two lines on stderr (one with `token=…`, which Claude Code itself redacted) and exits with code 3 |
| `ok.jsonl` | prints one line on stderr and answers the handshake |
| `hang.jsonl` | never answers; `MCP_TIMEOUT=3000` |
| `nope.jsonl` | its program does not exist |
| `remote.jsonl` | an HTTP server at a closed port, with an `Authorization` header (redacted by Claude Code) |
