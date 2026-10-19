# `claude mcp list` fixtures

| File | Origin |
|---|---|
| `lab-stdio.txt` | real capture from Claude Code 2.1.303 (Windows, piped, `NO_COLOR=1`, `ENABLE_CLAUDEAI_MCP_SERVERS=false`): 23 stdio servers of a small Python test server that behaves differently per name (healthy, exits, bad handshake, tools failing, missing command, `${VAR}` arguments). Paths anonymised, line endings normalised to LF. Each of these servers was also run through `claude mcp get`: status and issue were identical. |

A line reads `<name>: <command and arguments> - <glyph> <status>[ — <issue>]`. The command comes from the configuration and the issue from the server, so a line is only used when it holds exactly one status (`parse_list`).

| File | Origin |
|---|---|
| `lab-short-timeout.txt` | real capture, same servers and version, run the way klefki runs a list (`MCP_TIMEOUT=3000`, `MCP_SERVER_CONNECTION_BATCH_SIZE=10`): `hang` and `noargs` never connect, `slow4` and `slowtools5` run out of time that `get` (30 s) would give them. |
