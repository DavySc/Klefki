# 011: claude.ai connectors (2026-10-20)

## Context
Roadmap item 2, after plugins (008), the organisation's servers (009) and policy (010): Claude Code also loads the
connectors of the user's claude.ai account. Klefki listed none of them, so a user could not see them or turn one off
for a project.

## What Claude Code 2.1.304 does (read in the binary; 4 and 4a also tried live, without a claude.ai sign-in)
| # | Finding |
|---|---|
| 1 | Each session it fetches the account's connectors from claude.ai (`/v1/mcp_servers`) with the user's sign-in, and names each `claude.ai <display name>`, adding ` (2)` and so on when two share a name. It keeps no list on disk. |
| 2 | It writes the name of each connector it has connected to `claudeAiMcpEverConnected` in `.claude.json`, and never removes one. A connector waiting for a sign-in is in `mcp-needs-auth-cache.json` by the same name. |
| 3 | A connector is turned off per project by that name in `disabledMcpServers`, like every other server (008 finding 9). |
| 4 | `disableClaudeAiConnectors: true` in any settings file (user, a project's, the organisation's) stops all of them. With `managed-mcp.json` they load only when the organisation's settings say `allowAllClaudeAiMcps: true`. |
| 4a | Every connector is also off when, checked in this order: `ENABLE_CLAUDEAI_MCP_SERVERS` in `env` is `0`, `false`, `no` or `off` (any case, trimmed); a `CLAUDE_CODE_USE_BEDROCK`, `_VERTEX`, `_FOUNDRY`, `_ANTHROPIC_AWS`, `_ANTHROPIC_GOOGLE_CLOUD` or `_MANTLE` in `env` is `1`, `true`, `yes` or `on` (another provider); Claude Code signs in with a key ahead of the claude.ai account: a non-blank `apiKeyHelper`, or `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN` in `env`. `disableClaudeAiConnectors` and `apiKeyHelper` count in every settings file, a provider in every file's `env`, the switch and the keys only in the user's and the organisation's `env`, not a project's. Tried live with `claude --debug mcp list`, whose debug log says `[claudeai-mcp] Disabled …`. |
| 5 | Policy applies: a denied `serverName` with the connector's name blocks it. An allowed `serverName` cannot hold its name (only letters, digits, `-`, `_`), so with an allowed list a connector needs a `serverUrl` rule for its address (managed-mcp docs). |

## Decision
- A scope `ClaudeAi`, shown as **claude.ai**: read-only rows like the organisation's, switched per project by full name.
  The rows are the names of finding 2, at most 200, only those starting `claude.ai `. Status from the needs-auth
  cache; never checked. Type `claude.ai` and target "on claude.ai": Klefki does not know how or where Claude Code
  reaches it. The list drops the `claude.ai ` its group already says; the details, JSON and the switch keep the whole
  name. The details lead with the connector's state, then one line on where it comes from, and the rest of finding 2
  under "About claude.ai connectors".
- Not loaded, with why: `managed-mcp.json` without `allowAllClaudeAiMcps`; findings 4 and 4a, per place, naming the
  setting and the file, with what to do; a denied name; an allowed list with no address rules, which says to add a
  `serverUrl` rule for the address claude.ai shows. Findings 4 and 4a in the user's or a project's files are the user's
  own switch, not policy: `Blocked::policy` is false, and the app says "Connectors off", not "Blocked by policy".
- With address rules in an allowed list its address decides, which Klefki cannot see: `ServerRow::may_not_load` says
  so as a note ("may not load"), naming the projects when only some have such a list. A denied `serverUrl` or
  `serverCommand` is not compared.
- A connector switched off in some projects and held back in the others loads nowhere: it says so, each cause where it
  applies. User and organisation servers do the same. `ServerRow::disabled_projects` names where a server is off.
- `.claude.json` keeps `claudeAiMcpEverConnected` when read; every other unknown root key is still skipped.

## Not yet
- A connector's address, and so `serverUrl` rules. A server the user adds at a connector's address takes its place
  (Claude Code then lists the connector as hidden); Klefki cannot say when.
- Connectors never used: Claude Code lists them under "Show unused connectors" in `/mcp`, but has not connected them,
  so Klefki does not list them. Anthropic's own connectors appear only once used too.
- What finding 4a reads from the shell's environment rather than a settings file, a gateway, safe mode, and sign-in
  states (no sign-in, a missing scope). Klefki cannot see Claude Code's environment or sign-in.
- Turning a connector off for good (`deniedMcpServers` in the user's settings, which also covers projects opened
  later): a new file to write, so it waits for the owner's decision.

## Not verified
- Listing and switching live: needs someone signed in to claude.ai with a connector. Findings 4 and 4a were tried live
  through Claude Code's debug log, which says why it turns connectors off without a sign-in.
