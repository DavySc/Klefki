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
- Rebuilt on #47 after its cross-review fixes: a blocked organisation server leaves no plugin duplicate out
  (`an_organisations_server_policy_blocks_leaves_no_plugin_server_out`); a broken organisation file is one quiet
  note, not also a policy warning (`a_broken_organisation_file_is_one_quiet_note_not_also_a_warning`). Both failed
  first.
- Full verify on this branch (on top of #47 2ba30ba): exit 0; 355 + 21 + 26 + 797 passed, 0 failed; deny ok.

## Next
Warn in a change's review when policy will block it; then claude.ai connectors.
