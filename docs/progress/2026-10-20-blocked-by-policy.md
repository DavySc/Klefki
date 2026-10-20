# Every server Claude Code loads, slice 5: Blocked by policy (2026-10-20)
Goal: roadmap item 2. Say when Claude Code's policy keeps a server from loading, naming the rule and its file.

## Done
- Spike against real Claude Code 2.1.304: decision 010, findings 1-5 (sources, union of allowed lists,
  `allowManagedMcpServersOnly`, how each field matches, names before policy before plugin comparison).
- `config/policy.rs`: rules per place; `Rules::blocks(name, entry, organisation)` gives the reason.
- Inventory: per place for user, local, project, plugin and organisation servers; held back everywhere it would load
  is `not_loaded`, in some projects `ServerRow::blocked`. A blocked server hides no plugin server.
- App: badge "blocked in N/M" with the reason; details row "Blocked". CLI: note "blocked by policy in N of M
  projects"; JSON `blocked: {why, projects}`.

## Checks
- New tests, failing before the change: inventory (5: blocked everywhere names the rule; a project's own rules;
  a blocked local keeps the name; plugins; allowed lists vs the organisation's servers); policy (10); app (1); CLI (1).
- Real Claude Code vs `klefki-cli list`, two projects: user rules deny `serverCommand ["localx"]` and
  `serverUrl "https://api.example.com/*"`; `proj2/.claude/settings.local.json` denies `other`. Claude Code loads
  `other` in proj, user `x` and `l2` in proj2. Klefki: `web` and local `x` not loaded (naming the rule), `other`
  blocked in 1 of 2 projects, user `x` shadowed in 1 of 2. Same.
- Full verify on this branch (on top of #47): exit 0; 353 + 21 + 26 + 793 passed, 0 failed; deny ok.

## Next
Warn in a change's review when policy will block it; then claude.ai connectors.
