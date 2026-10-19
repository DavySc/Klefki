# Every server Claude Code loads, slice 1: plugin servers, read-only (2026-10-19)
Goal: roadmap item 2. Servers that Claude Code plugins bring were not in Klefki's list. This slice lists them,
read-only, and says where Claude Code leaves one out. Decision 008 records the spike against real Claude Code 2.1.304
and the later slices.

## Done
- Spike (temp `HOME` and `CLAUDE_CONFIG_DIR`, real `claude plugin install`, `mcp list`, `mcp get`): names, toggle,
  where files are read from, duplicates left out, policy rules. Results in `docs/decisions/008-plugin-and-policy-servers.md`.
- Core: `config/plugins.rs` reads the plugins that are on in the user's `settings.json` and their servers
  (`.mcp.json`, manifest `mcpServers` inline, by path or as a list; a local marketplace's own folder). Untrusted:
  size, count and path limits.
- `api.rs`: `Scope::Plugin` (`ConnectionId` word `plugin`), `ServerRow::plugin: Option<PluginInfo>` (plugin,
  marketplace, `same_as`).
- `inventory.rs`: plugin rows after the others, named `plugin:<plugin>:<server>`, with the toggle state like a user
  row, no status from the needs-auth cache, no file key, and "left out" (`shadowed`/`shadowed_in` plus `same_as`)
  where a server configured by hand, or an earlier plugin's, has the same command or address.
- Engine: background checks only for user and local rows (a plugin row would have been refused with "not checked: no
  longer in the configuration"). Ops refuse every change to a plugin row with "comes from the plugin …". Export and
  the Secrets page skip plugin rows.
- App: grouped as "Plugin · <plugin>" with the marketplace beside it; the row shows the server's own name, the
  details the full name, the plugin, "Left out", and a read-only note. Only the General and Status tabs. Toolbar
  buttons, palette entries and shortcuts that change, check, sign in or export are off for a selection holding one.
  Badges "left out" and "left out in N/M"; "Not checked" says on hover where the status is.
- `klefki-cli`: `list` groups them under "Plugin" after the user servers with notes "from the plugin …" and "left
  out: same as …"; `status` skips them ("not checked: it comes from a plugin"); JSON `scope: "plugin"` and a
  `plugin` object; `export` without names leaves them out.
- Fixture: two plugin rows (one left out). `api::tests::project_rows_always_carry_a_project_path` now filters
  local and project rows (it filtered "not user", which plugin rows broke) and also checks plugin rows carry no
  project and a plugin.

## Checks
- Tests written first and seen failing: inventory `plugin_servers_are_read_only_rows_under_the_name_claude_code_gives_them`,
  `a_plugin_server_repeating_one_configured_by_hand_is_marked_as_left_out`,
  `what_could_not_be_read_of_a_plugin_is_a_cleaned_warning`. Also: six in `config/plugins.rs`, engine
  `a_plugins_servers_are_never_checked_not_even_on_request`, ops `no_change_can_name_a_plugins_server`, app
  `a_plugins_server_is_shown_but_nothing_changes_checks_or_exports_it` and
  `a_plugins_server_claude_code_leaves_out_says_which_server_it_repeats`, CLI
  `a_plugins_servers_are_listed_after_the_users_say_where_from_and_are_not_checked`.
- Mutation checks, each failing a test: background checks back to "not project" (engine test); toolbar read-only
  guard off; Delete-key guard off (app test).
- Real Claude Code against the spike folder: `claude mcp list` shows `plugin:p1:tool` and `plugin:g1:gitsrv2`
  (disabled here) and leaves out `plugin:p1:web` (same address as user `u`); `klefki-cli list` on the same folder
  shows the same three, `web` "left out: same as u" and `gitsrv2` off.
- Real app (debug, Xvfb 1200x720) on the same folder: groups, badge, details as described.
- Full verify on main `a3d3b38` + this change: exit 0; 347 + 20 + 25 + 750 passed, 0 failed; deny "advisories ok, bans ok, licenses ok, sources ok".
- Screenshots: `reviews/2026-10-19-plugin-servers/` in the project files.

## Review of #43 (round 1)
Four places where "left out" differed from real Claude Code, each with a reproducer and a variant:
- `${CLAUDE_PLUGIN_ROOT}` is expanded before comparing (and in the target shown; what the plugin writes is on hover):
  inventory `a_plugin_server_is_compared_with_its_root_variable_expanded` (variant: two plugins writing the same text
  under different roots are not the same), plugins `the_plugin_root_variable_is_expanded_as_claude_code_runs_the_server`.
  Found along the way: a local marketplace's root kept a `./` (`mkt/./p1`), which Claude Code does not write; the
  root is now built from plain components (asserted in the local-marketplace test).
- The manifest's definition wins over `.mcp.json`'s: `the_manifest_wins_over_mcp_json_for_a_name_both_define`
  (variant: through a file the manifest names).
- Per project between plugins: `an_earlier_plugins_server_hides_a_later_one_only_where_it_is_loaded`, four cases
  (earlier one off in a, off in both, a local server plus the earlier plugin's, and that with the earlier one off in b).
  `add_plugin_servers` now walks each place (outside every project, then each project) with what is loaded there.
Polish: a selection mixing plugin and other servers turns on/off, deletes and exports the others and says how many
plugin servers it left out; with only plugin servers, the buttons' hint names the server and Del says why nothing
happens (`a_selection_with_a_plugins_server_acts_on_the_others_and_says_what_it_left_out`); `settings.json`,
`plugins/installed_plugins.json` and `known_marketplaces.json` are watched
(`the_files_that_say_which_plugins_are_on_are_watched`); the status reads "See /mcp"; the read-only note is first on
General; "Left out: in every project: … has the same command or address". `klefki-cli list --json` gives
`plugin.written_target`. Decision 008: findings 5 and 6 extended, known gaps added.

Checks for this round:
- New tests seen failing before the fix (core: 2 in `plugins.rs`, 2 in `inventory.rs`, 1 in `watch.rs`). App tests
  checked by mutation: status label back to "Not checked", no note on a mixed toggle, hint for mixed selections, each
  failing `a_plugins_server_is_shown_…` or `a_selection_with_a_plugins_server_…`.
- Real Claude Code 2.1.304 against `klefki-cli list`, temp `HOME`/`CLAUDE_CONFIG_DIR`, local marketplace: (1) `rooted`
  with `${CLAUDE_PLUGIN_ROOT}` and a user `manual` with the expanded path: both leave `rooted` out; `plugin:p1:dup`
  is `echo manifest` in both; `plugin:p2:dup2` loaded in both. (2) Two projects, local `local-echo` in the first and
  `plugin:p1:dup` off in the second: `dup2` left out in the first only, in both; with `p1:dup` on in the second, left
  out in both, in both.
- Full verify on this round: fmt, clippy `-D warnings` and deny ok; tests 348 + 20 + 25 + 755 passed, 0 failed. Then
  main (`f516b5e`, release files only) merged in.

## Open
- Plugins on or off only in a project's settings files are not read yet (decision 008, finding 3).
- No `.claude.json`: the list stops before plugins, as it does for everything else.

## Next
Slice 2: plugins switched in project settings, and turning a plugin's server on and off (with the model-based test of
change operations). Then organisation servers, Blocked by policy, claude.ai connectors.
