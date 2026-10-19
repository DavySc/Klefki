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
- **Fix** (`config/docstore.rs`, `without_done_renames`, `names_since`): in a replay, a rename whose server is
  there under its new name, or under any name later logged renames gave it, is in place (the name was free when it
  was made), so it and its switch-off list changes are left out. Otherwise, replaying its list change would have
  moved a later server's switch to the renamed one. A rename whose server a later logged change deleted is replayed
  as before: the delete then takes whatever it renamed, and the plan that brought the later server puts it back.
  The file is parsed once per replayed plan that holds a rename, off the engine thread.
- **Chain variant** (found by the first soak, and it is the macOS seed `522e43e2…`): rename q to q_r, copy q in,
  rename q_r to q_r_r. With only "new name is there", the first rename found q_r gone and renamed the later q, so
  a q_r appeared that nobody made. Fixed by following later renames (`names_since`).
- **Cross-review** (PR #7's thread) found a blocker: with only the last name of the chain checked, Claude Code
  writing the file back as it was between the copy and the second rename still failed with "gw already exists
  there". Now any name in the chain counts. Reproduced as a variant in the chain test before the fix.
- **Tests**, each failing before the fix: docstore
  `an_earlier_renames_check_accepts_a_later_server_under_the_old_name` (copy under the old name; variant: another
  server renamed into it; both also after Claude Code overwrote the file),
  `an_earlier_renames_check_leaves_the_switch_of_a_later_server_under_the_old_name` (the switch-off list variant),
  `an_earlier_renames_check_follows_its_server_through_later_renames` (the chain; variant: deleted at the end);
  machine `an_earlier_renames_check_does_not_fail_on_a_copy_under_the_old_name` (deterministic, 400 ms check
  delay). Both seeds kept in the regression file.
- Merged main after PR #7 (79b9a79); both sides' tests and seeds kept. Added PR #7's seed `cc 2bc4448e…` (a kept
  secret lost on a copy; fails 3/3 on main before #7, passes after), at its author's request.
- **Checks** (Linux, after the main merge 33eb7e0): `cargo fmt --check` 0; `cargo clippy --workspace --all-targets --
  -D warnings` 0; `cargo test --workspace` 0 (534 core, 255 app, 15 + 20 cli); `cargo deny check` 0.
  Seed `f857ab3d…` alone: 0 failures in 30 runs (4 in 10 before). Soak with PR #7 + this fix (before the
  review fix): 3 × 4000 cases passed; after the review fix, on this branch: 2 × 4000 passed, and all checks above
  pass again.
- **Open, for review:** a delete-ended chain (rename x to y, a new x, delete y) checked together replays the rename
  onto the new x; the servers end right, but the new x can move to the end of its list and the file is written
  once more. That goes against the key-order invariant in CLAUDE.md (key order preserved). Nothing is lost; not
  changed here (behaviour as on main).
- **Next:** cross-review by another thread, CI on three systems, merge.
