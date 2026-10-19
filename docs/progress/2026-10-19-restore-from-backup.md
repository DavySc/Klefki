# Restore from a backup in the Activity log (2026-10-19)
Roadmap slice (follow-up of the Activity log, #15): restore Claude Code's servers from a backup listed on the Activity log page, and move an unreadable log file aside (cross-review R2 of #15).

## Design
- A backup is a whole copy of `.claude.json`, which also holds Claude Code's own settings, history and trust answers. Writing it back would undo those, so a restore compares **servers only**: each user and local server goes back to its definition in the backup, one that is gone is put back, and each project's `disabledMcpServers` goes back to the backup's. Nothing else in the file changes.
- A restore **never removes a server**. Servers added since the backup are left as they are, switches included, and named in the review ("Leave memory for the user as it is: it was added after this backup").
- It is one more change: `Change::Restore { backup }` → Prepare → review → Apply. The docstore backs up the file as it is first, the restore is logged with that backup, so a restore can itself be undone the same way.
- A value that looks like a secret (env or header) and goes back to the backup's value, or away, is named in a warning (never its value): "the value it has now is kept only in the copy Klefki takes before restoring".
- Servers that sign in with OAuth go through Claude Code's own commands as always: a changed one is a swap (as an edit), a gone one `claude mcp add-json --client-secret` with the client secret Klefki keeps under its own connection (in `MCP_CLIENT_SECRET` only). No copy: a note says so.
- What cannot be restored is left out with a warning, not a failed restore: a `<hidden>` value in the backup, an invalid definition, one that cannot be reviewed safely, an OAuth one with a typed-in secret or a name Claude Code's commands cannot take, or whose project folder is gone; servers of a project Claude Code no longer lists get a note.
- A backup is named by its file name; the folder is always Klefki's backup folder, and only `claude.json.<ms>-<n>.bak` is accepted.

## Done
- Core `restore.rs` (new): the comparison and the plan. `engine::prepare` resolves a lone `Change::Restore` through it (with other changes it is refused), and looks up kept client secrets for servers it puts back (`CliAdd::stored`).
- `docstore`: an add and a change (replace, remove, rename) in the same server list in one plan was refused; the edits never overlap (the addition goes after the last member, where its change ends), and the overlap check still refuses the one case that would (every member removed plus an add). Found trying the restore in the real app.
- `activity.rs`: a log that cannot be read (too large, not UTF-8, not a file) is moved to `activity.unreadable-<ms>-<n>.jsonl` next to it, when the page reads it or when a change is appended, and a new log starts; the page says where it went. The size cut now happens before the line is appended. Summary times use `moment()`: "19 Oct 2026 at 14:43".
- App: on the Activity log page, each kept `.claude.json` backup has "Restore to before this…" (or "Restore this copy…" under Other backups); it opens the usual review dialog. The page's intro says restoring shows the change first.

## Tests (written first, seen failing)
- `restore.rs`: 9 unit tests (put back and leave added ones; change back and say which settings; nothing to do; switches except servers added since; secret named never valued; OAuth through Claude Code; what cannot be restored, said; only kept `.claude.json` backups by name; a backup that is not a configuration).
- Engine: `a_restore_puts_back_a_deleted_server_and_leaves_the_rest_of_the_file_alone`, `a_restore_puts_back_one_server_and_changes_another_back_in_the_same_list` (the real-app reproducer), `restoring_what_is_already_there_writes_nothing`, `a_restore_is_refused_for_anything_but_a_kept_backup_of_claude_json`, `a_restore_puts_back_a_server_that_signs_in_with_its_kept_client_secret`.
- Docstore: `servers_can_be_added_and_others_changed_in_one_list_in_one_plan` (pretty and one-line files; add with replace of the last member, remove of a middle one, remove of the last one; every member removed plus an add still refused).
- Activity: `a_log_too_large_to_read_is_set_aside_whole_and_a_new_one_started` replaces `a_log_too_large_to_read_is_left_alone`: that test pinned the behaviour R2 asked to change (appending to an unreadable log). The new one keeps its guarantee, nothing that was there is lost, by checking the set-aside file byte for byte. Plus `a_log_that_is_not_text_is_set_aside_when_read_and_says_where` and `a_moment_in_a_sentence_has_its_date_and_minute`.
- App: `a_kept_backup_of_claude_json_can_be_restored_after_review`.

## Checks
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`: exit 0; 304 app + 620 core + 17 + 22 cli tests, 0 failed; deny "advisories ok, bans ok, licenses ok, sources ok".
- `cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu -- -D warnings`: exit 0 (Windows code compiles; not run). macOS not built locally.
- Real app (release, Xvfb, temp `HOME`, `CLAUDE_CONFIG_DIR`, `XDG_DATA_HOME`, no Claude Code): a backup with filesystem, github (old token) and time, a project with postgres and time off; now github has a new token, time other args, memory added. Restore: review lists each change, warns about github's Authorization header, shows what each server runs; after Restore the file has all four back as in the backup, memory kept, `numStartups` as it was now, key order kept, and the log has the restore with its backup. Unreadable log: moved aside, page says where. 1200x720 and 800x480 screenshots in the project folder `reviews/2026-10-19-restore/`.

## Open / next
- Approvals (`enabledMcpjsonServers`, a project's `settings.local.json`) and the legacy `mcpServersDisabled` list are not restored; only servers and their switches.
- Not tried with a real Claude Code (OAuth put-back runs the same `add-json` path as Put back, covered with fake-claude).
