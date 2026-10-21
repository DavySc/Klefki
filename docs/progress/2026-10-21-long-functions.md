# Long functions into named steps (simplification A4)

Goal: slice A4 of `docs/proposals/simplification-plan.md`, with the leftovers from A1's and A2's reviews. One pull
request per file or small group. Behaviour and look unchanged: no golden file or look picture may change. Not in
this slice (behaviour changes, left for the owner): the Fix casing rename refused when both spellings are in one
place; `swap.rs` `same_settings`' stdio condition, inverted against its comment.

## Parts
1. Shell leftovers from A1: the notices above the list (old switch-off lists, kept servers) to `shell/notices.rs`
   (`legacy_notice`, `kept_notice`, `migrate_legacy`, `LegacyNotice`) with their tests in
   `shell/tests/notices.rs` (12 tests, both sections' headings kept); five theme and CLI-version tests to
   `shell/tests/top_bar.rs`; `outcome_toast`'s stacked doc comment dropped; `managed_read_only`'s doc says the
   organisation, not a plugin. `shell.rs` 6,914 to 6,324 lines. Checked by script: removed and added lines are
   the same apart from `pub(super)`, imports, module lines, the two doc comments and one signature `cargo fmt`
   joined. Merged in #82 (ca451ed).
2. Each dialog's handling of the engine's events moves into its module, and `apply_event` (540 lines) becomes a
   dispatch of 191: Export's three answers (`exported`, `export_file_chosen`, `export_saved`) to `export.rs`;
   Duplicate's definition (`copy_read`) to `duplicate.rs`; the editor's (`definition_read`, `editor_after_change`,
   `follow_editor`, `SecretView::answered`) to `editor.rs`; `plan_ready` and `plan_failed` to `review.rs`;
   `applied`, `apply_failed` and `Outcome::problem` (the five-field "went wrong" outcome written out three times) to
   `outcome.rs`; `AddForm` and `DuplicateForm` get `review_failed` and `port_answered`; the snapshot's arm is
   `take_snapshot` in `shell.rs`. Fields now private: the forms' `port_pending` and `port_checked`, Duplicate's
   `source`, Export's `save` and `status`, the editor's `moved`, all of `SecretView`'s. The rest stay `pub(super)`
   because the tests in `shell/tests/` read them (a test module there is not inside the dialog's module).
   Mutants on the changed lines (68): 46 caught, 13 unviable, 9 missed, each also missed on main (the same code in
   `apply_event`). Seven now have tests (seen failing against the mutant): `port_answered`'s `==` in Add and
   Duplicate; `applied`'s `signs_in && changed` and the "to each" guard (three mutants); `take_snapshot`'s
   `live_gen += 1` (a check cut off by a snapshot left the old explanation). Two are equivalent: `kept.is_empty()`
   guard (both arms give "Saved." for an empty `kept`), and `editor.moved` guard to `true` (an editor that did not
   move only gets its own definition again, which rebases to the same text unless the file changed in between). Review: a comment repeating
   `editor_after_change`'s doc dropped. Merged in #83 (8fdfb6c).
3. `klefki-cli`'s `args::parse` (167 lines) in named steps: `parse` reads the command (33 lines),
   `Options::read` the options and names after it (`None` when the help is asked for, so `-h` still answers at
   once), `Options::into_cmd` checks they fit the command, `use_cmd` builds `use`. The same checks in the same
   order with the same messages. Mutants on `args.rs`: main 76 (3 missed), branch the same three: an option given
   as the command, `--force` on a command other than export, an empty `--project=`. A test now refuses each
   (`list --force` added in review: the first version tested `list --output=x.json`, another line). Merged in
   #85 (0ccb057).
4. `restore::compare` (about 190 lines) in named steps: `put_back` (servers the backup had that are gone or
   changed, returning those left out), `switches` (one project's on/off switches), `leave_added` (servers added
   since, left as they are) and `closing_notes`; `compare` reads both sides and walks the projects (about 50
   lines). Same steps in the same order with the same words. Mutants on `restore.rs` on main: 91, 13 missed;
   eight in `compare` (a project gone since named even with no servers, the undefined-switch filter, two match
   arms for one and for several servers added since, and the three parts of "what a restore leaves" being said
   only when it writes). Tests now check each: `one_server_added_since_is_left_as_it_is`,
   `a_project_gone_since_is_named_only_when_it_had_servers`,
   `what_a_restore_leaves_is_said_whenever_it_writes_anything`, a no-notes assertion when nothing differs, and the
   undefined-switch line in `switches_go_back_except_for_servers_added_since`. The other five are outside
   `compare` (`Servers::read`, `places_of`, `mentions`, `server`) and not touched here.

## Next
5. The rest, file by file: `paste::add` (written, waits for part 4), `paste::lex_with`, `ops::use_profile`,
   `inventory::build`, `swap::run` last; the engine ideas from A2's review where they stay moves.

## Follow-ups for the owner (not in this slice)
- From #82's review, also on main: at 800×480 the kept notice's "back without some settings" row draws Forget…
  over its own label (WCAG 2.2 SC 1.4.10 Reflow). Wrapping the label above the buttons would fix it; a look change.

## Checks
See each pull request description.
