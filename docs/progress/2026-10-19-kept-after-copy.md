# A kept definition after a copy lands where its move was going (2026-10-19)
Goal: main's model test `engine::machine::no_change_ever_loses_a_server_or_its_secret` failed on CI (Linux, PR #1,
seed `19dc2de0…`). Find whether the model or the product is wrong, fix it, keep the seed.

- **Reproduced** on main 693a16a with the seed in `crates/core/proptest-regressions/engine/machine.txt`. The steps:
  `a` (signs in) copied into pb; a move of pb's `a` to pa fails three times, so Klefki keeps its definition; a copy
  of the user's `a` (same definition) to pa. The listing dropped the kept definition; the model expected it kept.
  Not caused by the scrub (#31 touched no engine or swap code); the input is random, and this seed is new.
- **Both sides were wrong.** Dropping it is right when the copy also carried the client secret: the server is where
  the user sent it, and Put back would bring it back where it was moved from. The model did not allow that. But the
  listing also dropped it when the copy carried no secret (its source had no Klefki copy): Claude Code then has no
  client secret for that sign-in (the failed move's remove took it), and the only one left was Klefki's copy under a
  server that is gone, with nothing offering it. Shown with a one-off test before the fix.
- **Product** (`swap.rs`, `Recovery::list` takes the store; `engine.rs` passes it): a kept definition whose change
  got through is dropped only once Klefki's copy of its client secret, if it holds one where the server was, is also
  where the server went. The copy left behind is deleted first; if that fails, it stays listed and is tried again.
  Store read failures count as not yet.
- **Model** (`machine.rs`): predicts the drop from its own expected servers and Klefki secrets, and the deleted copy.
- **Tests**, each failing before the product fix: machine `a_kept_definition_is_not_offered_once_a_copy_puts_it_where_its_move_was_going`
  (the seed, plus no copy left behind), `…_stays_offered_when_a_copy_without_its_secret_…` (Put back then restores
  Claude Code's secret), engine `a_kept_definition_whose_change_got_through_without_its_client_secret_is_still_offered`.
  Variant that passed before too: `…_stays_offered_when_a_copy_puts_another_definition_…`. The seed is kept.
- **Changed test, for human review:** `engine::tests::a_kept_definition_whose_change_got_through_is_not_offered`
  (cross-review S1) now also puts Klefki's copy at the new place before the listing, the state the swap leaves just
  before it deletes the recovery file, and checks the copy left behind is deleted. As it was, it simulated a stop
  between the add and moving Klefki's copy; that state cannot be told from the copy without a secret (Klefki never
  reads Claude Code's credentials), so it is now listed (variant test above). The window is milliseconds and the
  outcome safe: Put back or Forget. Also took #5's wait for a snapshot with nothing kept, same lines.
- Decision 007 updated (the got-through bullet).
- **Checks** (Linux, this branch before the main merge): `cargo fmt --check` 0; `cargo clippy --workspace
  --all-targets -- -D warnings` 0; `cargo test --workspace` 0 (528 core, 243 app, 19 + 15 cli); `cargo deny check` 0;
  `cargo clippy -p klefki -- -D warnings` 0. Model soak: 6000 cases found the seed below (on main too); then 2 × 4000 cases
  passed with the product fix.
- **Open, not this slice:** a soak found `cc f857ab3d242d6d4ed2dc7fbc58dd9bf0676912dab34df17970f88fb86b501bd0`
  (Edit, PutBack, Copy, Rename, Copy with DeleteFails, Rename, Move): "an earlier change's check failed: b_r already
  exists there". It fails about 4 runs in 6 on plain main 693a16a too (timing: Klefki's own check of a write), so it
  is the journal replay, not kept definitions. Not added to the regression file here, so this PR stays green; it
  needs its own slice.
- **Open:** a copy into the place a kept definition came from overwrites Klefki's copy of the kept secret (seen
  while probing; the model only checks it when the secrets differ, and they never do there). Not changed.
- **Next:** cross-review by another thread, CI on three systems, merge.
