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

## Cross-review of 270545b (fixed)
The reviewer compared it with real Claude Code and the docs (code.claude.com/docs/en/managed-mcp). Fixed:
- Allowed lists judge a server by the rules of its own kind (`an_allowed_list_judges_a_server_by_the_rules_of_its_own_kind`).
- A rule has one key; an entry with two is ignored. `every_field_of_a_rule_must_match` held the opposite and is
  replaced by `a_rule_with_more_than_one_key_is_ignored` (the behaviour it tested was wrong, not weakened).
- Allowed names only `[A-Za-z0-9_-]`, so a plugin's server is not allowed by name
  (`a_plugins_server_cannot_be_allowed_by_name_but_can_be_denied_by_it`, `an_allowed_name_has_only_…`).
- `serverUrl`: no path is any path, ports by how the host is written, `:*`, trailing dot
  (`a_port_matches_by_how_the_host_is_written`, address test extended).
- A `managed-mcp.json` server with `${VAR}` needs an allowed rule (`an_organisations_file_server_with_variables_…`).
- UX: plain words first with the next step by whose rule it is (`what_to_do_depends_on_whose_rule_it_is`); status
  "Blocked by policy" / CLI `blocked` when blocked everywhere (`a_server_policy_blocks_everywhere_says_so_in_the_list`);
  the details and CLI name the projects; two causes each named (`a_server_not_loaded_for_two_causes_…`).
- Live, Claude Code 2.1.304 vs `klefki-cli list` (user settings, 7 servers, /tmp/claude-0/s4): five rule sets (kinds,
  two-key entries, `*.example.com` ports and trailing dot, `:*`, no path, `*://`) give the same servers loaded.
- Full verify after the fixes: exit 0; 356 + 21 + 26 + 804 passed, 0 failed; deny ok.
- Reviewer OK at 8f4e281 (re-ran every case live). Folded in its two wording nits: "takes its place" (that server may be
  blocked too) and the missing period; full verify exit 0, same counts.

## Open
- Reviewer's side note: without `hasTrustDialogAccepted`, Claude Code loads the user's server instead of an approved
  project server of the same name. Not looked at yet.

## Next
Warn in a change's review when policy will block it; then claude.ai connectors.
