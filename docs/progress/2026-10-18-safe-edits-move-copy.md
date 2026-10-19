# Safe edits, move and copy (2026-10-18)
M2 slice: rollback-safe remove-then-add so servers that sign in with OAuth can be edited, moved and copied, plus Move
between the user and local scope. Acceptance: PLAN's model-based test. Design and spike findings: decision 007.

- **Core:** `swap.rs` (new). A swap removes and adds one server through Claude Code's `mcp remove` and
  `mcp add-json --client-secret` (secret only in `MCP_CLIENT_SECRET`), behind the `swap::Claude` trait (`Program` over
  `runner::run`). The old definition is saved to a private recovery file first; a failed add puts it back with the
  same secret; a failed put-back keeps the file, listed in `Snapshot::kept`. The file decides every outcome (re-read
  after each command). Plan-time `review` refuses taking another server's secret, warns about signing in again, and
  runs nothing for an unchanged OAuth edit.
- **Engine/ops:** `Change::Move { id, to }`, `Change::PutBack { kept }`, `Change::ForgetKept { kept }` (destructive).
  `Event::Applied` carries `notes` (settings Claude Code left out). Edit and Duplicate of an OAuth server become swaps.
  A non-OAuth move is one docstore plan (`CopyServer` + `RemoveServer`). Before Claude Code runs, klefki's pending
  journal for `.claude.json` is replayed and retired (`settle`). `add_with_claude` uses the same trait.
- **Docstore:** OAuth refusals stay, except for `Purpose::Preview`; `keep_copy` takes a private copy before a swap.
  Bug fixed: the journal's verify of an edit whose server was deleted afterwards failed the plan
  (`gone_is_done` now covers `ReplaceServer`); reproducer and variant.
- **App:** Move button (arrow icon) and palette entry; Move dialog with radio places, where it is now disabled and
  tagged, a taken name refused, an OAuth note. After a move or copy the result is selected and the message says
  "Moved sso to webapp." / "Copied …", with Sign in offered for OAuth. Applied notes are shown. A notice lists kept
  servers with Put back and Forget…. The OAuth edit form is no longer blocked; it says you sign in again.
- **Found trying it for real** (`cargo run -p klefki` under Xvfb, temp `HOME` and `CLAUDE_CONFIG_DIR`, real Claude
  Code 2.1.302): the Move icon (U+E0A0) drew JetBrains Mono's branch symbol, so the toolbar is now a const checked by
  `every_toolbar_and_nav_icon_is_drawn_by_the_icon_font` (failed with the old icon); a move warned that the server
  "is used instead of" itself in the target project (`moving_a_server_does_not_warn_about_itself`, failed before the
  fix, plus a variant that still warns about another server). The plain move then worked: the file kept the unknown
  key and the rest, a backup was made, the row was selected in its new place. The OAuth move was refused before
  anything ran: there is no Secret Service in the container. The same OAuth move through the engine with real
  Claude Code and a test store (a one-off test, not kept): Applied, the entry in the project, Claude's client secret
  under its scope-free key, Klefki's copy moved to the local id, no recovery file left.
- **Tests:** model test `no_change_ever_loses_a_server_or_its_secret` (256 cases by default, ~7 s; a 4000-case soak
  passed) and 3 explicit machine tests; engine via fake-claude: OAuth move (argv, cwd, secret only in env, Klefki's
  copy moved), remove that leaves it there, remove that hangs; moves without self-warnings; docstore 2; app 12 (move
  dialog, palette, taken name, selection and messages, OAuth note and Sign in, kept notice, icons). Mutations of the
  rollback (no put back, no recovery file, no secret moved, …) are caught by the model test.
- **Changed tests (behaviour changed on purpose):** `definition.rs` OAuth definitions now have no problem (they are
  saved through Claude Code); the app test that OAuth servers are not edited in the form is split into one for
  project servers (unchanged) and one that an OAuth server is edited and says it signs in again.
- Checks after cross-review round 1 and the main merge: `cargo fmt --check` 0, clippy workspace
  and `-p klefki` 0, `cargo test --workspace` 485 core + 225 app + 32 cli passed (after the main merge), `cargo deny check` 0, no IO in
  `crates/app/src`.
- **Open / for human review:** OAuth servers are now removed through `claude mcp remove` (invariant area). A move or
  edit of an OAuth server signs it out (Claude deletes its credentials). Proposals in decision 007: letting the
  docstore remove OAuth entries (keeps the sign-in), and a fake-claude mode that writes the config. The OAuth move
  was not tried in the real app (no keyring here).
- **Cross-review round 1** (PR comment 6255733102, head d9d9ed4), each with a reproducer that failed before its fix:
  - B1 Put back and Forget always said the definition was gone (the plan read kept definitions from a fresh read
    that never lists them): `a_kept_definition_can_be_put_back` / `_forgotten`; the model test now fails on a refused
    Put back or Forget (reverting the fix fails it; seed kept).
  - B2 a missing project folder was found only after the remove signed the server out: checked at review and before
    the first command (`a_move_into_a_missing_folder_…`, `a_move_out_of_a_missing_folder_…`).
  - B3 the review diff showed `alwaysAllow` kept: the preview now shows what `add-json` writes and warns what it
    drops (`the_review_shows_what_claude_code_writes_and_warns_what_it_drops`, plus edit-in-place variant).
  - S1 stale recovery after a change that got through: reconciled on listing (`a_kept_definition_whose_change_got_through_is_not_offered`).
  - S2 Klefki's keyring copy: moved before the recovery file goes; set/delete failures are notes; model test has
    keyring faults and checks Klefki's secrets exactly.
  - S3 project key through a link (unix): refused (`a_project_reached_through_a_link_…`).
  - S4 progress per command (`a_slow_change_says_what_claude_code_is_doing`, app `…says_each_step_while_it_runs`).
  - S5 moving a user server into one project warns where it is no longer (`moving_a_user_server_into_one_project_…`).
  - S6 Undo after a plain move, warning tone that stays when settings were dropped (app test); S7 OAuth warning by
    the places in warning colour.
  - Polish 1–7 and 9 folded in (wording, full stops and line breaks in errors, kept notice name first, diff scroll per
    plan, clearer icon plus Ctrl+Shift+M, faded disabled radio, "In a project, just for you", decision 007 line on
    `.credentials.json`). Polish 8 (backup path behind a link) left: the path is the way back, and a warning toast
    no longer fades.
  - Found by a 3000-case soak: a delete's journal replay undid a later move back into that place (pre-existing on
    main): plans now store copies expanded; docstore test and machine `an_earlier_deletes_check_does_not_undo_a_move_back_into_its_place` (+ copy variant).
  - Then found by a 4000-case soak: a move then a rename, checked together, failed ("q_r already exists there"):
    the move's add found its server missing and added it back. A replay now treats a server added by one plan and
    deleted or renamed by a later one as it ended (`as_they_ended`): docstore
    `an_earlier_moves_check_does_not_add_back_a_server_renamed_since` (+ edit between), `…_deleted_since` (+ rename
    into the name), machine `an_earlier_moves_check_…`; each failed before the fix.
  - Found by the soak after the main merge: (1) when Klefki's store refused the moved copy, the old copy stayed
    where the server was, and a server put there later claimed it; it is now deleted too (Claude Code has the
    secret; the note says to type it in) (`a_secret_klefki_could_not_move_is_not_left_where_the_server_was`, failed
    before). (2) Two servers that share one sign-in (a copy under the same name): when an edit of one fails twice,
    Claude Code's remove has taken the other's client secret, and only Claude Code can hand it back. The failure now
    says so, and Put back restores it (`a_kept_definition_says_which_server_lost_its_shared_sign_in_…`). **For human
    review:** the model test's "every OAuth server with a Klefki secret has it in Claude Code" check now allows this
    one state (a kept definition sharing the sign-in), and requires the failure to have said it.
  - Windows CI (first CI run of this branch; earlier pushes conflicted with main, so CI never ran): Claude Code was
    run from the project folder as Klefki spells it (`C:/…`), not as the file does (`C:\…`), and the server landed
    under a second entry. Claude Code now runs from the file's own spelling of the folder
    (`a_server_that_signs_in_lands_under_the_projects_own_spelling`, reproduced on Linux with a trailing separator;
    failed before). The swap's places are also respelled, so Klefki's copy of the secret is filed under the
    server's id as the list shows it, and a plain Add with Claude Code runs from the same spelling (this one was on
    main too). A 1500-case run of the model test with every project key spelled with a trailing separator passed.
    Real Claude Code on Windows not tried.
  - Soak after all fixes: 3 × 4000 cases passed (~110 s each), twice (before and after the main merge).
- **Cross-review round 2** (comment 6256596745, head 3a8c10c): no blockers. Folded in:
  - The replay now follows a chain of renames (add, rename, rename again) inside one check window. Test
    `an_earlier_adds_check_follows_a_chain_of_renames` (+ a variant that deletes at the end); it failed before.
  - Moving or copying into a project that switches the name off now warns that it stays off there
    (`moving_into_a_project_where_it_is_switched_off_says_it_stays_off`).
  - A definition put back without some settings is listed as back (`Kept::back`). The notice offers only
    Forget… and says what is kept (core `a_definition_put_back_without_some_settings_is_listed_as_back`, app
    `a_server_back_without_some_settings_offers_only_forget`). Each failed before.
  - Singular wording in the notice, and "Klefki no longer keeps the definition of …" after Forget.
  - Signing in again is the first warning. `CLAUDE_KEEPS` names the versions it was checked against.
  - Left for Davy: Undo without a second review. Every write goes Prepare → diff → Apply (CLAUDE.md), so Undo
    stays a reviewed move, and the key's position in `mcpServers` is not restored. Also left: marking a project
    whose folder is gone in the Move dialog. That needs core to check folders when it lists projects; today it is
    refused at Review.
- **Merged main** again with the core review fixes (#27): no conflicts; verify 0 (490 core + 225 app + 32 cli).
- **Merged main** with the free-port check (#23) and the CLI (#25): one conflict in `api.rs` (`real_config` from
  main, plus `changer: None`). Decision renumbered to 007: main has its own 006 (CLI output).
- **Next:** re-review round. After the UI-review thread's `closed_on_purpose(&modal)` lands on main: merge main, switch the Duplicate and Move dialogs to it (a backdrop click must not close them), with `click_beside` tests. Not merged by this session:
  the merge is left for Davy.
