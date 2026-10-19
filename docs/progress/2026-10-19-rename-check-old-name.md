# An earlier rename's check accepts a later server under the old name (2026-10-19)
Goal: the model test `engine::machine::no_change_ever_loses_a_server_or_its_secret` failed on plain main about 4 runs
in 10 with seed `cc f857ab3d…` ("an earlier change's check failed: b_r already exists there", found while soaking
PR #7), and on macOS CI for PR #3 with seed `cc 522e43e2…` ("q_r_r already exists there"). Find the cause and fix it.

- **Reproduced** on main 2822293 with the seed added to `crates/core/proptest-regressions/engine/machine.txt`:
  4 of 10 runs failed.
- **Cause (product):** the after-save check replays every logged change that is not yet checked, oldest first, on
  the file as it is now. The log held "rename b to b_r in pa" and then "copy the user's b into pa as b". Replayed on
  the file, the rename found the new b and its new name b_r taken, and the check reported "b_r already exists
  there" as a failure of a change that was in place. A user would see that error after renaming a server and then
  copying or renaming another one into the old name within the check delay. Nothing was lost; the error was false.
  It was timing-dependent only because the rename's own check, if it ran before the copy started, retired it.
  Not a replay-timing race in the model: the model was right to fail.
- **Fix** (`config/docstore.rs`, `without_done_renames`): in a replay, a rename whose new name is already there is
  in place (the name was free when it was made), so it and its switch-off list changes are left out. Otherwise,
  replaying its list change would have moved a later server's switch to the renamed one.
- **Tests**, each failing before the fix: docstore
  `an_earlier_renames_check_accepts_a_later_server_under_the_old_name` (copy under the old name; variant: another
  server renamed into it; both also after Claude Code overwrote the file),
  `an_earlier_renames_check_leaves_the_switch_of_a_later_server_under_the_old_name` (the switch-off list variant);
  machine `an_earlier_renames_check_does_not_fail_on_a_copy_under_the_old_name` (deterministic, 400 ms check
  delay). Both seeds kept in the regression file.
