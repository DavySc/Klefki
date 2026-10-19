# Edited servers, F5 and after-save checks (2026-10-19)
Three engine bugs from the core review's list of open items (engine items 1-3), fixed together because they share the check queue and `reload`.

1. **An edited server kept its old verdict.** `reload_after_change` only checked servers that were new or just switched on, so an edited server (same connection) was never checked again, and a check running during the save answered for the old definition.
   - `FileKey` now carries a fingerprint of the server's definition (`inventory::fingerprint`, a hash of the entry as the file has it). `State::reload` compares the old and new keys and returns the servers whose definition changed.
   - Every reload notes changed and removed servers in `Shared::changes` (a counter and, per server, the count at its last change). A check that started before its server changed does not answer. A diagnosis in that case says "the server changed while it ran; diagnose it again" rather than never answering.
   - An edited server is checked again (in the background, announced; a waiting background check of it is replaced) and shows "Checking…" at once. When it is not checked (a project server, a server that is off, no background checks) it no longer claims a verdict: Unknown, "changed since its last check" (a project server waiting for approval keeps that).
2. **F5 dropped checks the user asked for.** `refresh` cleared the whole queue and bumped a generation that threw away every running check's answer, so a server that is off or a project's, never queued again, got no answer. Now only background jobs are cleared, and the generation is gone: `Shared::changes` decides per server whether an answer is stale.
3. **The after-save check stopped at the first failing file.** `check_later` returned on the first error, so later files were neither checked nor retired from the redo log, and the next change on that file replayed the stale plan (undoing, for example, a rejection made in Claude Code meanwhile). It now checks every file, then reports all failures in one `ApplyFailed`.

## Tests (each seen failing on main first)
- `an_edited_server_is_checked_again`, variant `a_server_whose_environment_alone_changed_is_checked_again` (nothing the row shows changed), and the other side `a_server_left_alone_by_an_edit_is_not_checked_again`.
- `a_check_running_while_its_server_is_edited_does_not_answer_for_the_new_definition`.
- `a_check_the_user_asked_for_still_answers_after_f5` (running) and `a_check_the_user_asked_for_that_waits_its_turn_survives_f5` (queued, a server that is off).
- `a_file_whose_after_save_check_fails_does_not_stop_the_next_file_being_checked` and `a_change_after_a_failed_check_does_not_replay_the_earlier_one`.

## Checks
- Full verify (`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`) on this branch from main `9b2be4e`: exit 0, 847 passed, 0 failed, deny "advisories ok, bans ok, licenses ok, sources ok".
- Real app (debug build, Xvfb 1200x720, temp `HOME` and `CLAUDE_CONFIG_DIR`, fake-claude as `claude` on `PATH`): `demo` (`old-server`) shows Connected. With `.claude.json` edited from outside to `new-server` and the fake now answering failed, the row shows `new-server` and Failed a few seconds later; the log shows exactly two `mcp get demo` runs.

## Open
- The app still drops "Checking…" on every snapshot (the separate app item in the findings); an edited server's "Checking…" can be hidden by a later unrelated reload until its answer comes.
- Next: cross-review, CI on three systems, merge.
