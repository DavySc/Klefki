# `claude mcp get` fixtures

Real captures from Claude Code 2.1.302 (Windows, piped output, so no colour codes). Line endings normalised to LF; the parser tests also run CRLF variants.

| File | Origin |
|---|---|
| `connected-stdio.txt` | real (server arguments anonymised) |
| `failed-stdio.txt`, `failed-http.txt` | real |
| `pending-approval.txt` | real (unapproved `.mcp.json` server; Claude does not run it) |
| `not-found.stderr.txt` | real stderr, exit code 1 |
| `needs-auth.txt`, `connected-tools-failed.txt`, `not-configured.txt`, `connection-error.txt` | **synthetic**: the status strings come from the Claude Code 2.1.302 source, the rest of the layout is copied from the real captures |
| `timed-out-stdio.txt` | real, Claude Code 2.1.308 on Linux with `MCP_TIMEOUT=2000` and a server that answers after 5 s (name and path anonymised) |
| `hostile.txt` | **synthetic**: terminal escapes, a bidi override, an oversized issue line, and fake secrets that must never reach the UI |
