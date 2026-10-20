# An earlier edit's check lands on a server that took a renamed server's name (2026-10-20)
Goal: fix the model-based test failure seen on PR #50's CI (seed `7a12d6aa`, also failing on main `ebb8a5a`): after an
edit, a rename and a move into the old name, two servers ended with the edited definition.

## Cause
The check a moment after a save replays every logged change still due (`Docstore::replay`). An edit of `p` followed by
a rename of `p` to `p_r` replayed the edit under the name `p`. When the server had not been added within the log (it
was there before, or its copy was already checked), `as_they_ended` left the edit as it was, so it landed on whatever
held `p` by then: a server moved or copied in since. The check then wrote that over the newcomer.

## Done
- `config/docstore.rs` `as_they_ended`: for a rename whose server was not added in the log, the edits since the
  server came in are taken out and replayed right after the rename, under the new name, each as its own plan (two edits
  of one server in one plan are refused). Renamed twice, they follow it to its last name. Adds keep the old handling.

## Checks
- Reproducers, all failing on main and passing with the fix: engine
  `an_earlier_edits_check_does_not_land_on_a_copy_under_the_name_it_was_renamed_from`,
  `an_earlier_edits_check_does_not_land_on_a_server_moved_in_under_the_name_it_was_renamed_from` (the seed's shape),
  `a_late_check_does_not_put_an_edit_back_on_a_server_renamed_since` (the seed's shrunk sequence; timing-dependent:
  failed 6 of 6 runs on main, passed 30 of 30 with the fix); docstore
  `an_edit_of_a_server_renamed_since_stays_with_it_when_another_takes_its_old_name` (file intact and overwritten) and
  the variant `an_edit_of_a_server_renamed_twice_ends_on_its_last_name`. Seed added to
  `proptest-regressions/engine/machine.txt`.
- Full verify (fmt, clippy -D warnings, `cargo test --workspace`, `cargo deny check`): exit 0.

## Open / next
- None known. Cross-review, then merge.
