# Every server Claude Code loads, slice 6: claude.ai connectors (2026-10-20)
Goal: roadmap item 2. List the claude.ai connectors Claude Code loads, switch them per project, say when one is off.

## Done
- Spike in the Claude Code 2.1.304 binary: decision 011 (no live claude.ai sign-in here).
- Core: `Scope::ClaudeAi` ("claude.ai", model word `claudeai`), read-only and everywhere; rows from
  `claudeAiMcpEverConnected` and the needs-auth cache; not loaded for `managed-mcp.json` (unless
  `allowAllClaudeAiMcps`), `disableClaudeAiConnectors` per place, a denied name, an allowed list without address rules.
  Switch by full name in every project; every other change refused, saying it is changed on claude.ai.
- App: details say where it comes from and that the list may hold a removed connector; status "See /mcp";
  selection notes name claude.ai. CLI: group "claude.ai", note "a connector of your claude.ai account", JSON scope
  `claudeai`, `status` skips it (`skipped: "claudeai"`). Fixture `claude.ai Linear`.

## Checks
- New tests, failing first: inventory (4), ops (1), app (1), CLI (1).
- Full verify: exit 0; 371 + 22 + 26 + 824 passed, 0 failed; deny ok.

## Open
- Not verified live (needs a claude.ai account with a connector). Klefki cannot know a connector's address.

## Next
Roadmap item 2 is then complete; next item 3 (tools view) per docs/PLAN.md.
