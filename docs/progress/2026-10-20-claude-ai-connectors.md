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
- Cross-review of 0e6f2b3 (changes needed), fixed:
  - Connectors off by `apiKeyHelper`, another provider, or `ENABLE_CLAUDEAI_MCP_SERVERS`, `ANTHROPIC_API_KEY` or
    `ANTHROPIC_AUTH_TOKEN` in `env`, in the files where each counts (decision 011 finding 4a, tried live).
  - An allowed list without address rules says to add a `serverUrl` rule; with address rules the row says
    "may not load" (`ServerRow::may_not_load`) instead of plainly on.
  - Off in some projects and held back in the rest: not loaded, each cause where it applies (user and organisation
    servers too). `ServerRow::disabled_projects` names where; the app's Enabled row and the CLI note name them.
  - The user's own switch is not policy (`Blocked::policy`): "Connectors off" status, "turned off in n/m" badge;
    switched off in some projects and held back in others is one "loads in n/m" badge, not a "+1".
  - List names drop `claude.ai `; details lead with the state, then one line and an "About claude.ai connectors"
    section; type `claude.ai` (`TransportKind::ClaudeAi`), target "on claude.ai". Messages name the setting and file.
  - CLI tables cut names to 64 characters with "…"; JSON keeps them whole and adds `disabled_projects`,
    `blocked.policy` and `may_not_load`.

## Checks
- New tests, failing first: inventory (4), ops (1), app (1), CLI (1).
- Review round: inventory (+6: keys and providers in user settings, per project, the organisation's, off plus held
  back, allowed lists of addresses), app (+2), CLI (+2). The settings reproducers failed on 0e6f2b3 (apiKeyHelper not
  said). Live: `claude --debug mcp list` with a temp `HOME` and `CLAUDE_CONFIG_DIR`, 33 settings variants, each as
  finding 4a says.
- Full verify: exit 0; 371 + 22 + 26 + 824 passed, 0 failed; deny ok.
- Review round, full verify: exit 0; 373 + 24 + 26 + 829 passed, 0 failed; deny ok. One earlier run had
  `status_ended_early_leaves_no_check_running` fail once (a check still running after the signal); it passed alone three
  times and in the next two full runs. This change does not touch how checks are started or ended.

## Open
- Not verified live (needs a claude.ai account with a connector). Klefki cannot know a connector's address.
- For the owner: turn a connector off for good with `deniedMcpServers` in the user's settings (a new file to write).

## Next
Roadmap item 2 is then complete; next item 3 (tools view) per docs/PLAN.md.
