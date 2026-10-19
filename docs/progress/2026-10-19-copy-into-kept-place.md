# A change into a kept definition's place keeps its secret (2026-10-19)
Goal: a copy into the place a kept definition came from overwrote Klefki's copy of its client secret (open item in
`2026-10-19-kept-after-copy.md`). Reproduce, fix the product so no change there can destroy that secret.

- **Reproduced** on main 79b9a79 (machine.rs rig): `a` copied into pb, its move to pa fails three times, so Klefki
  keeps pb's `a`, with `sec-a` under pb's `a` in Klefki's store. Then a copy of the user's `a` into pb as `a`:
  - with another Klefki secret at the source (`sec-other`): Done; the kept place now holds `sec-other`, so Put back
    hands Claude Code the wrong secret;
  - with no Klefki secret at the source: Done; the kept secret is deleted.
  The model test missed it: every server named `a` shares `sec-a`, and the case is rare. 3000 cases passed on main.
- **Cause** (`swap::run`): a copy or move to a new connection sets Klefki's copy there (or deletes it when it has
  none). The place of a kept definition is empty in the file, so nothing refused it. The same holds for a move, an
  add with a secret (`add_with_claude`), and, for a server that does not sign in, its later Auth tab or move away.
- **Fix, the safer choice: refuse.** A kept definition's place stays free until it is put back or forgotten.
  `swap::free_of_kept` is checked at review for every arrival of a plan (`ops::Resolved::arrivals`: adds, copies,
  moves, renames, Klefki's or Claude Code's), and again just before each swap and each Claude Code add runs, since
  another Klefki may keep a definition there between review and apply. Alternative not taken: move the kept secret
  under the recovery record's own key so the place can be reused (second key scheme, migration). Decision 007 updated.
- **Second gap, same secret:** `kept_list` passed `shared.config.secrets`, which is `None` in the app (the system
  store is opened lazily), and `Recovery::list` took `None` as "no copy to lose". So in the app PR #7's check never
  ran: a change that got through dropped the kept definition without checking or moving Klefki's copy. The listing
  now opens the store only when a change got through (`secret_store(shared)`), and a store that cannot be opened
  keeps the definition listed.
- **Tests**, each failing before the fix: machine `a_copy_into_the_place_a_kept_definition_came_from_is_refused_and_its_secret_kept`,
  `a_copy_without_a_secret_…`, `a_move_into_…`, `adding_a_server_into_…`, `a_plain_copy_into_…`, `a_rename_into_…`
  (each then puts it back and checks `sec-a` in both stores), `a_copy_reviewed_before_a_definition_was_kept_in_its_place_is_refused_when_applied`
  (fails with the apply-time check removed); swap `a_kept_definition_whose_change_got_through_stays_listed_while_the_secret_store_cannot_be_opened`
  (fails with the old "no store, nothing to lose"); variant that passed before too: `…_goes_with_its_secret_once_the_store_opens`.
  `a_copy_into_the_place_a_failed_edit_kept_is_refused` (the model's own find, below).
- **Model:** predicts a refusal for a move, copy or rename into a kept place (after the review's own listing), and
  has a step `CopyToKept` that aims a copy at one, as random steps almost never do.
- **Soaks** (Linux, 4 cores): plain main 79b9a79, old model 3000 cases passed; with the refusal check only, 3000
  and 20000 cases passed (never reached). With `CopyToKept`, plain main failed in under a second (the edit-then-copy
  case above). This branch: 2 × 4000 cases passed (109 s, 113 s).
- **Checks** (after merging main 7105902, #9): `cargo fmt --check` 0; `cargo clippy --workspace --all-targets -- -D
  warnings` 0; `cargo test --workspace` 0 (544 core, 259 app, 20 + 15 cli); `cargo deny check` 0; `cargo clippy -p
  klefki -- -D warnings` 0. Windows and macOS: CI only.
- **Open:** none known in this slice. The app shows the refusal in the review; it does not grey out the target in the
  Move or Duplicate dialog beforehand.
- **Next:** PR, CI on three systems, cross-review, merge.
