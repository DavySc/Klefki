# Every server Claude Code loads, slice 2: turning a plugin's server on and off (2026-10-19)
Goal: roadmap item 2. Slice 1 (#43) listed plugin servers read-only. Claude Code switches a plugin's server off by
its full name in a project's `disabledMcpServers` (decision 008, finding 2), so Klefki now does too.

## Done
- `ops.rs`: `SetEnabled` on a plugin row writes `SetDisabled` with `plugin:<plugin>:<server>` (the raw name from the
  row's connection) in every project Claude Code knows, with the same per-project warning as a user server; no
  project yet is refused. Shared with the user branch as `switch_everywhere`. Every other change to a plugin row is
  still refused.
- Model-based test (`engine/machine.rs`): the rig has a plugin `pl@m` with one server. New steps `TogglePlugin` and
  `ChangePlugin` (edit, delete, rename, copy, move). After every step: the plugin's files and `settings.json` are
  unchanged; the plugin server's switch-off lists change only on `TogglePlugin`, and then exactly as asked; every
  other change to it is refused with "comes from the plugin".
- App: Turn on/off (toolbar and palette) take every selected row, plugin servers included; Delete, Edit, Export and
  the rest still skip them as in slice 1. The details note says Klefki switches it and the plugin changes it.

## Checks
- Tests written first and seen failing: ops `a_plugins_server_is_switched_by_its_full_name_in_every_project`;
  app `a_plugins_server_is_switched_on_and_off_but_nothing_else_changes_checks_or_exports_it` and the mixed-selection
  test (toggle takes both rows; Delete takes the other and says what it left out). The model test fails on the old
  refusal ("switching a plugin's server: step 0 TogglePlugin { on: false } … Refused") and passes with
  `PROPTEST_CASES=1500`.
- `ops::tests::no_change_can_name_a_plugins_server` no longer lists the two `SetEnabled` changes: they are now
  allowed and covered by the new test.
- Full verify on main `157d4a3` + this change: exit 0; 348 + 20 + 25 + 756 passed, 0 failed; deny ok.
- Real Claude Code 2.1.304 (temp `HOME`/`CLAUDE_CONFIG_DIR`, two projects): turning `plugin:p1:dup` off through the
  engine (`Prepare` → `Apply`, `start_headless`, throwaway test not kept) put it in both projects' lists, and
  `claude mcp list` showed "Disabled for this project" in both (and `plugin:p2:dup2`, which it had hidden, loaded).
  Turning it on emptied both lists and Claude Code loaded it again.

## Open
- Plugins on or off only in a project's settings files are not read yet (decision 008, finding 3).

## Next
Slice 3: plugins switched in project settings files. Then organisation servers, Blocked by policy, claude.ai connectors.
