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
   seven in `compare` (a project gone since named even with no servers, the undefined-switch filter, the match arms
   for one undefined switch and for one server added since, and the three parts of "what a restore leaves" being
   said only when it writes). Tests now check each: `one_server_added_since_is_left_as_it_is`,
   `a_project_gone_since_is_named_only_when_it_had_servers`,
   `what_a_restore_leaves_is_said_whenever_it_writes_anything`, a no-notes assertion when nothing differs, and the
   undefined-switch line in `switches_go_back_except_for_servers_added_since`. The other six are outside
   `compare` (`Servers::read`, `places_of`, `mentions`, `server`) and not touched here. Review: the test checks all
   three closing notes. Merged in #86 (17e7771).
5. `paste::add` (267 lines, reading `claude mcp add`) in named steps: `read_add` reads the options and words into
   `AddWords` (with its `Settings`: transport, env, headers, OAuth), `server_of` turns them into the server for its
   transport, and `add` (38 lines) checks what may be added. The same checks in the same order with the same words.
   Mutants on the changed lines: main 45 (5 missed); the branch before the tests 49 (the same 5 missed): an `-e`
   with nothing before `=`, an `-H` with no name, `--callback-port 0` (two), and `-e` on a remote server. A test now
   refuses each; with them 43 caught, 6 unviable, 0 missed. Merged in #87.
6. `paste::lex_with` (215 lines, splitting pasted text into commands and words) becomes a `Lexer` with named steps:
   `code_ended` (the end of Markdown inline code or a table cell), `pasted_line` and `not_claude` (what a pasted
   line that is not a claude command becomes), `read` (one character), `single_quoted`, `double_quoted`, `plain`,
   `needs_shell` and `finish`. The same steps in the same order. The paste speed tests take the same time as on main
   (five runs each, 1.52 s and 1.50 s on average). Mutants on the changed lines: main 88 (8 missed), branch 103 (the
   same 8, and one more that main's run counted as caught but is missed there too when tried by hand:
   `from + start` to `from - start`). Tests now catch seven: the arguments of inline code, two commands with more
   text between them than in the first, a line of text starting with an escape that does not show, `#` and a long
   dash inside a word, and parentheses. Review showed the last two, `end + 1` to `end - 1` or `end * 1` where the
   next command on the line is searched for, are not equivalent (a panic inside a two-byte character; the closing
   backtick read as opening code) and gave a test for both, so all 9 are caught. Merged in #88.

7. `ops::use_profile` (293 lines, using a profile) in named steps: `profile_places` (the project, or every one),
   `ProfileWalk::of` and `server` (what each server is in each place: its switch, whether it is on now, held back,
   elsewhere or not approved), `held`, `remember` (what stopping the profile puts back), `really_off`,
   `left_as_they_are`, `not_approved` and `already_on`; `use_profile` (89 lines) writes the review in the same
   order with the same words. Mutants on the changed lines: main 34 (3 missed), branch 64 (the same 3): whether a
   switch that goes off hides a duplicate (two) and grouping the places of a member held back. Tests now catch each:
   `a_duplicate_stays_out_while_what_it_repeats_stays_on`, `two_members_held_back_are_each_named`. Merged in #89
   (5486245). Review's polish (`ProfileWalk` built with `Default`, a line of doc on four fields) came with part 8.
8. `inventory::build` (452 lines, the list of every server Claude Code loads) in named steps: `Found` (what the files
   hold: snapshot, file keys, servers named by hand), `Judge` (what decides whether a server loads: the
   organisation's rules, policy, hints, the projects shown) with `user_servers`, `project_servers`,
   `local_servers`, `shared_server`, `user_verdicts`, `organisation_servers` and `connector`; `build` (132 lines)
   reads the files and calls them in the same order with the same words. Mutants on the changed lines: main 53 (7
   missed), branch 68 (the same 7): which project's rules a local server answers to (its own rules blocking its
   local server), nothing counted as off with no project known, an organisation's server off in every project, off in
   one and held back in the other, and a connector held back by a setting that is not policy. Five tests now catch
   all 7 (each seen failing against its mutant). Start-up (`scripts/perf-baseline.py --runs 9`, release builds):
   first snapshot with 950 rows 105.2 ms on main, 106.0 ms here; with 70 rows 111.8 and 109.8 ms; `klefki-cli list`
   12.1 and 12.4 ms; memory the same (about 130 MB resident, 44 MB private dirty). Merged in #90 (136909c).
9. `swap::run` (222 lines, an edit, move or copy of a server through Claude Code) in named steps: `checked` (the
   entry as it is and as it will be, or why not), `secret_for` (the client secret to hand over), `remove_old`,
   `undo_remove` (putting the old entry back when adding the new one failed) and `move_secret`; `run` (64 lines) does
   them in the same order with the same words. Mutants on the changed lines: main 18 (17 caught, 1 unviable), branch
   12 (11 caught, 1 unviable); none missed. `same_settings`' inverted stdio condition is left as it is (owner). Merged
   in #91 (cf488d9). Review's note on `remove_old`'s doc (it drops the recovery file) came with part 10.
10. The engine ideas from A2's review, as moves: reading the configuration again (`State::reload`, `refresh`,
    `reload_after_change`, `queue_checks`, `show_edited`, `needs_check`, `kept_list`, `profiles_changed`) moves from
    `command_loop.rs` to `engine/reload.rs`, which ends the `changes.rs`/`command_loop.rs` import cycle;
    `claude_program` moves from `checks.rs` to `engine.rs`, beside the shared state it reads; `Plans`' fields are
    private, and the two places that dropped a stored plan call `Plans::forget`. The queue's dropping of an edited
    server's waiting background checks is `Queue::drop_background_checks`, so a test can reach it. Checked by
    script: removed and added lines are the same apart from imports and visibility. Mutants on the changed lines:
    main 58 (7 missed), branch 60 (the same 7): the "could not determine" warning with no configuration found, an
    edited server not checked again counted as tried by a refresh, the three conditions of dropping a waiting
    check, and an unchecked edited server's "changed since its last check" (two). Four tests now catch all 7 (each
    seen failing against its mutant).

## Next
A4 is done once part 10 is merged.

## Follow-ups for the owner (not in this slice)
- From #82's review, also on main: at 800×480 the kept notice's "back without some settings" row draws Forget…
  over its own label (WCAG 2.2 SC 1.4.10 Reflow). Wrapping the label above the buttons would fix it; a look change.
- Fix casing offers a rename, then refuses it when both spellings are in one place (`ops.rs`); not touched here.
- `swap.rs` `same_settings`: its stdio condition is inverted against its comment (unreachable today); not touched.
- Six missed mutants in `restore.rs` outside `compare` (`Servers::read`, `places_of`, `mentions`, `server`).
- The reviewers of #89 to #91 saw `cargo deny check` fail on main too: tokio 1.54.0 in `Cargo.lock` was yanked.
  It passed in this session's container and in CI. Updating the lock file would be its own slice.

## Checks
See each pull request description.
