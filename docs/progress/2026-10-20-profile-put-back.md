# Stop using a profile puts back what it switched (2026-10-20)

Goal: the owner asked that deleting a profile leave servers as they were before it was used (a profile turned memory
off in Designs; after deleting it, memory should be on again), and floated a global template profile. Design pass
first (`docs/proposals/profile-put-back.md`); the owner chose "Remember and put back" (decision card, 14:52Z) with the
"In use in" / "Stop using" wording, which also approves the new `profile-uses.json`.

## Done
- Core: `profile_uses.rs` keeps, per profile, the `disabledMcpServers` switches its uses flipped (project, name,
  state set) in `profile-uses.json` beside `profiles.json`: read size-limited and checked like `profiles.json`,
  written whole and owner only, never when read in part. Read with each snapshot (`Snapshot::profile_uses`) and for
  each Prepare.
- Core: a saved `UseProfile` records what it flipped, and shares switches another profile set the same way. A rename
  through Klefki moves a server's switches to its new name; renaming a profile keeps its record; Delete only forgets it.
- Core: `Change::StopProfile { name, project, delete }` puts each switch back while it is still as the profile left it;
  one switched since is left and warned about; one another profile switched later is left as that profile has it,
  and that profile's switch is forgotten; one another profile shares stays; gone servers and projects are said. With
  `delete`, the profile is deleted once it is saved. `api::profile_in_use` says where a profile is in use.
- App: a card says "In use in …" with "Stop using in …"; Delete on a profile in use offers "Stop using and delete…"
  and "Delete only".
- Docs: decision 012, CLAUDE.md's exception now names `profile-uses.json`, proposal marked chosen.

## Checks
- New engine tests (owner's case with and without delete; changed since; two profiles in either order end as before
  both; stop in one project after a server and a profile rename; Delete only), `profile_uses` unit tests, and the app
  test `a_profile_in_use_says_where_and_stops_or_deletes`. Written with the code; flipping the put-back switch in
  `stop_profile` makes four of the engine tests fail.

- Full verify on main 04a0841 + this branch (round 1 fixes: core 915, app 416, cli 25 and 33, exit 0): `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D
  warnings`, `cargo test --workspace` (core 910, app 415, cli 25 and 33), `cargo deny check`: exit 0.
- Real app (Xvfb 1200x800, temp HOME, `CLAUDE_CONFIG_DIR` and `KLEFKI_DATA_DIR`, Claude Code 2.1.304): the owner's case.
  Writing (gw, db) used alone in designs turned memory off; the review said Klefki remembers it; the card said "In
  use in designs." with "Stop using in designs…"; Delete offered "Stop using and delete…" and "Delete only"; the
  review showed `- memory`, "memory goes back on in designs, as before Writing was used."; after it,
  `disabledMcpServers` in designs was `[]`, `profiles.json` and `profile-uses.json` held nothing.

## Cross-review (round 1, changes requested; fixed)
- B1: a switch turned on and off again through Klefki after a use was put back by Stop. Now a change other than a
  profile's forgets every profile's record of the switches it writes (`profile_uses::forget_switches`; migrations by
  the names they turn off), and its review says so (`ops::no_longer_put_back`). Reproducers
  `a_switch_changed_through_klefki_since_is_no_longer_the_profiles` and the Restore variant
  `a_restore_since_a_profile_was_used_is_not_put_back` failed with the forget disabled.
- B2: a leftover record under a name, then a rename onto it, wrote the name twice and locked the file; a new profile
  under it took the record over. `profile_uses::claim_name` drops it first; forget and rename errors are said on the
  page. Reproducers `a_profile_saved_under_a_leftover_name_does_not_lock_or_take_over_its_record` (engine) and
  `a_leftover_record_is_dropped_when_its_name_is_taken` (unit) failed with the drop disabled.
- Folded in: "Stop using in…" asks which project when there are several (N1, app test
  `a_profile_in_use_in_several_projects_stops_in_the_one_chosen`); a project both in use and on alone is said once
  (N2); `load` and `record` find switches by key, not by search (N5); the snapshot is read again after a stop even if
  the delete then fails (N6). N3 (tooltip) left: the review shows the same.

## Next
- `klefki-cli` showing where a profile is in use and a `stop` command; Connections showing profiles in use.
