# 011: claude.ai connectors (2026-10-20)

## Context
Roadmap item 2, after plugins (008), the organisation's servers (009) and policy (010): Claude Code also loads the
connectors of the user's claude.ai account. Klefki listed none of them, so a user could not see them or turn one off
for a project.

## What Claude Code 2.1.304 does (read in the binary; not tried live: it needs a claude.ai sign-in)
| # | Finding |
|---|---|
| 1 | Each session it fetches the account's connectors from claude.ai (`/v1/mcp_servers`) with the user's sign-in, and names each `claude.ai <display name>`, adding ` (2)` and so on when two share a name. It keeps no list on disk. |
| 2 | It writes the name of each connector it has connected to `claudeAiMcpEverConnected` in `.claude.json`, and never removes one. A connector waiting for a sign-in is in `mcp-needs-auth-cache.json` by the same name. |
| 3 | A connector is turned off per project by that name in `disabledMcpServers`, like every other server (008 finding 9). |
| 4 | `disableClaudeAiConnectors: true` in any settings file (user, a project's, the organisation's) stops all of them. With `managed-mcp.json` they load only when the organisation's settings say `allowAllClaudeAiMcps: true`. |
| 5 | Policy applies: a denied `serverName` with the connector's name blocks it. An allowed `serverName` cannot hold its name (only letters, digits, `-`, `_`), so with an allowed list a connector needs a `serverUrl` rule for its address (managed-mcp docs). |

## Decision
- A scope `ClaudeAi`, shown as **claude.ai**: read-only rows like the organisation's, switched per project by full name.
  The rows are the names of finding 2, at most 200, only those starting `claude.ai `. Status from the needs-auth
  cache; never checked. The details say the list is the connectors Claude Code has connected before, so one removed on
  claude.ai may still be there, and that connectors are changed on claude.ai.
- Not loaded, with why: `managed-mcp.json` without `allowAllClaudeAiMcps`; `disableClaudeAiConnectors` (per place,
  naming the file, with what to do); a denied name; an allowed list with no address rules. Klefki does not know a
  connector's address, so with address rules in an allowed list it says nothing, and a denied `serverUrl` or
  `serverCommand` is not compared.
- `.claude.json` keeps `claudeAiMcpEverConnected` when read; every other unknown root key is still skipped.

## Not yet
- A connector's address, and so `serverUrl` rules; a connector added on claude.ai that Claude Code has not connected yet.
- The `ENABLE_CLAUDEAI_MCP_SERVERS` environment variable and sign-in states (an API key taking precedence, a missing
  scope), which also keep connectors away. Klefki cannot see Claude Code's environment or sign-in.

## Not verified
- Everything live: the spike read the binary only. Needs someone signed in to claude.ai with a connector.
