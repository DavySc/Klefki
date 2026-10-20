# Profiles, slice 2: palette and "on alone" (2026-10-20)

Goal: roadmap item 4, after slice 1 (#57): profiles from the command palette, and which projects a profile is on
alone in, worked out from the files (decision 012).

## Done
- Core: `api::profile_alone_in(members, snapshot)`: the projects where, of the servers Claude Code switches there, a
  profile's members are on and every other one is off. By name, as Claude Code switches; a local or project server
  counts in its own project only; a project where no member loads is never one.
- Core: each snapshot names the profiles (`Snapshot::profiles`), read with it like the kept definitions, so the
  palette reads nothing when it opens.
- App: the palette offers Profiles, New profile, and "Use profile NAME…" for each profile (not on sample data). Use
  opens the page with that profile's "Use in" taking the keyboard (Space opens it); a filter that would hide it is
  cleared. New keeps a profile already being made or edited.
- App: each card says "Only its servers are on in q, r and 2 more." Worked out once per snapshot and per answer
  from the engine, not each frame.
- Docs: decision 012 updated (Decision and Not yet).

## Checks
- New tests failed first: `a_profile_is_on_alone_*` (3, ops.rs; one compares with what using the profile alone would
  switch, over every way of switching a project's servers, for five profiles), `each_snapshot_names_the_profiles`
  (engine.rs), `a_profile_says_where_only_its_servers_are_on_and_follows_the_files`,
  `the_palette_opens_profiles_starts_one_and_uses_one` (fails without the focus request),
  `the_palette_goes_to_profiles_from_anywhere_and_offers_no_change_on_sample_data` (shell.rs),
  `a_few_projects_are_named_and_the_rest_counted` (profiles.rs).
- Full verify (fmt, clippy -D warnings, `cargo test --workspace`, `cargo deny check`): exit 0; core 896, app 405,
  cli 24 + 26.
- Real app under Xvfb with a temp `CLAUDE_CONFIG_DIR` and `XDG_DATA_HOME`: Ctrl+K, "use" lists both profiles; Enter
  opens Profiles with Work's "Use in" focused; cards say "Only its servers are on in q." and "... in r."

## Next
- `klefki-cli` and profiles (writing from it needs the owner's word).
- Show on the Connections page which profiles a project is on alone in.
