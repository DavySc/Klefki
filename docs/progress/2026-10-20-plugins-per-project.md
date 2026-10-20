# Every server Claude Code loads, slice 3: plugins switched in a project's own settings (2026-10-20)
Goal: roadmap item 2. Claude Code merges `enabledPlugins` from the user's `settings.json`, then the project's
`.claude/settings.json` and `settings.local.json` (decision 008, finding 3). Slices 1 and 2 read only the user's.

## Done
- Spike, real Claude Code 2.1.304 (temp `HOME`/`CLAUDE_CONFIG_DIR`, two projects): `claude plugin enable --scope
  project` writes the project's `settings.json`, `disable --scope local` its `settings.local.json`; a plugin on only in
  one project loads only there; local wins over project, project over user. Merged order: with the user's
  `{p1: false, p2: true}` and the project's `{p1: true}`, p1 comes first and its server wins a duplicate; with the
  user's `{p2: true}` and the project's `{p1: true}`, p2 wins. Recorded in 008 (finding 3).
- `plugins::load` takes the projects and returns, per place (outside every project, then each project), the servers
  of the plugins on there in Claude Code's order. Project files are read only for a local absolute path, size-limited,
  and only when some plugin is installed.
- Inventory: "left out" is worked out per place over that place's plugins; `PluginInfo::off_in` names the projects
  where the plugin is off; a server is shadowed when it is left out in every project where its plugin is on.
- Watched: each project's `.claude/settings.json`.
- App: quiet badge "plugin off in N/M"; details row "Plugin on" ("in every project", or "in K of M projects; off in
  …", paths on hover). `klefki-cli list`: note "plugin off in N of M projects"; JSON `plugin.off_in`.
- Review (from #44's cross-review): turning off a server that a plugin's server repeats warns that the plugin's server
  loads instead where it is off.
- Fixture: `plugin:docs-search:search` shows its expanded target, what the plugin writes, and is off in one project.

## Checks
- New tests, failing before the change: plugins `a_projects_own_settings_switch_plugins_there_and_the_local_file_wins`,
  `a_plugin_keeps_the_place_it_first_has_in_the_merged_settings`, `a_projects_settings_are_read_only_when_a_plugin_is_installed`;
  inventory `a_projects_own_settings_switch_a_plugin_there_and_what_it_leaves_out`; watch (project `settings.json`
  watched); ops `turning_off_a_server_a_plugins_repeats_says_that_one_loads_instead`; app
  `a_plugin_off_in_some_projects_says_where`; CLI list note and JSON.
- Real Claude Code against `klefki-cli list`, same folder: user `{p1: false, p2: true}`, project 1 local `{p1: false}`,
  project 2 `{p1: true}`, both servers `echo dup`. Claude Code: project 1 loads `p2:two`; project 2 loads `p1:dup` and
  leaves `p2:two` out. Klefki: `p1:dup` "plugin off in 1 of 2 projects"; `p2:two` "left out in 1 of 2 projects: same as
  plugin:p1:dup".
- Full verify on main `f207f5e` + this change: exit 0; 349 + 20 + 25 + 761 passed, 0 failed; deny ok.

## Cross-review fixes
- Blocking: a server left out only outside every project read "left out in 0 of 2 projects: same as …". The other
  server is now named only where the row counts it (projects, or outside them when Claude Code knows none).
  Reproducer and variant: inventory `a_server_left_out_only_outside_every_project_names_nothing`.
- A plugin's server whose plugin is off in every project is dimmed, reads "Enabled: yes, but its plugin is off in
  every project", and `klefki-cli list` shows `off` (JSON `enabled` stays the switch). App
  `a_plugin_off_in_every_project_reads_so`; CLI render test.
- Turning such a server on says where its plugin keeps it off: ops
  `turning_on_a_server_whose_plugin_is_off_in_a_project_says_it_stays_off_there`.

## Open
- A plugin on only in a project but not installed: not listed, with the warning "its files were not found" (Claude Code
  offers to install it; not tried).

## Next
Organisation servers (`managed-mcp.json`, `managedMcpServers`, `.mcpb`), then Blocked by policy, then claude.ai
connectors.
