# 004: Making connection problems easy to debug

Status: compared and built 2026-10-17 (Diagnose works; see docs/progress.md). Compared against the MCP debugging guide (https://modelcontextprotocol.io/docs/2026-07-28/tools/debugging) and the MCP Inspector page, with experiments on Claude Code 2.1.302.

## What the guide and klefki each cover
| Guide | Klefki today | Verdict |
|---|---|---|
| **MCP Inspector** is the first stop: an interactive tester for server authors (`npx @modelcontextprotocol/inspector ...`, web, CLI and TUI modes, Node 22.19+). | Not built. | **Do not rebuild it.** Calling tools and browsing resources is a server author's job. At most a "copy the Inspector command" hand-off later. It stores OAuth tokens and `env:` values in its own store, and an OAuth server would need a separate sign-in, so never pass secrets to it. |
| **Server stderr** is captured by the host (stdio) and is the first thing to read. | We show only the one `Issue:` line from `mcp get` (for example `CONNECTION_CLOSED`). | **Gap, now closed by design.** See the experiment below. |
| **Streamable HTTP**: stderr is not captured; use curl or the browser's network panel. | We show Claude's one-liner (`ECONNREFUSED`, `Needs authentication`). | A real opportunity: a network probe (DNS, TLS, the HTTP status of the URL) built on the planned `net/` module. Later. |
| **Common issues**: working directory is undefined, use absolute paths; stdio servers inherit only some environment variables; startup problems (path, config JSON, environment). | Config problems already show as invalid rows. Live checks run `claude mcp get` from the home folder. | Covered as plain-language hints (done). A lint for relative paths in user-scope stdio servers is a good nice-to-have, because a check from the home folder could pass where a real session fails. |
| **Protocol errors** in the 2026-07-28 spec: `-32022` unsupported protocol version, `-32021` missing client capability, `-32602` invalid params (missing `_meta`). | Not decoded. | Decoded as hints (done). Klefki will **not** call `server/discover` or speak MCP itself: that duplicates the client and would need OAuth. |
| **Client logs**: Desktop `%APPDATA%\Claude\logs\mcp*.log`. | Planned as a nice-to-have tail. | The guide's path is wrong on this machine (the MSIX copy is the real one, decision 003). Klefki resolving the right path is real value. |
| **Restart the client** after config or server changes (Desktop must be fully quit). | Not mentioned anywhere in the apply flow. | Add a "restart needed" notice to the apply dialog (M1). |
| **Security**: sanitise logs, protect credentials. | Sanitiser plus the planned redaction. | Redaction built and tested (`diagnose/redact.rs`). |
| MCP logging notifications (`notifications/message`) are deprecated as of 2026-07-28. | n/a | Do not build anything on them. |

## Experiment: the client's own diagnostics on demand
`claude --debug mcp --debug-file <path> mcp get <name>` works with `mcp get`, takes about 0.6 s and writes 4 to 5 KB. For a stdio server whose command does not exist it contains the server's **stderr** (`'...' is not recognized as an internal or external command`), which the normal output reduces to `CONNECTION_CLOSED`. For an HTTP server it shows the transport options, the proxy environment variables (`HTTP_PROXY`, `HTTPS_PROXY`), a connectivity test and `No token data found`.
In two tests the output did **not** repeat a configured `Authorization` header or an `env` value. That is not a guarantee (a server's own stderr can print anything), so the text is still treated as untrusted and secret-bearing.

## Decision
- **Diagnose** (Status tab): on an explicit click, re-run the check as `claude --debug mcp --debug-file <private temp file> mcp get <name>`, read the file with a size cap, keep the lines about that server, **sanitise and redact** them, delete the file, and show (a) plain-language hints, (b) what the server itself printed, (c) the client log excerpt, and (d) a copy-sanitised-report action. It follows the same rules as "Check now": a project stdio server runs the repository's command, so it needs the explicit click and the warning.
- No new protocol code, no inspector, no log files read from disk for this (the client writes the file for us).
- Later (nice to have): network probe for HTTP servers; relative-path lint; Inspector command hand-off; restart notice goes into the M1 apply dialog.

## Not verified
- Other Claude Code versions may print different debug text, so the hint patterns match on stable fragments and fail quietly (no hint) rather than guess.
- Whether the debug output could ever include a configured secret in other code paths (for example during an OAuth token exchange). Redaction is the safeguard, with tests.
