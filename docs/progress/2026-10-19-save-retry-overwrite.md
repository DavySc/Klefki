# Save retry no longer overwrites Claude's write (2026-10-19)

Goal: on Windows a refused rename of `.claude.json` is retried for about 0.6 s (`retry_denied`) without looking at the file again, so a write Claude Code makes without the lock in that wait was replaced by ours. Fold in the small docstore lock and backup items that belong with it.

## Done
- **Save retry** (`Docstore::write`): every try at the rename first checks the file's stamp, not only the first. If someone wrote meanwhile, nothing is replaced, the try's backup is removed, and `read_edit_write` redoes the change on their version (up to 3 times, then `Locked`, as for a write before the first try). Test seam: `refuse_replace` (tests only) refuses a try as Windows does.
- **Lock release** (`Lock`): the lock folder's stamp is taken when we make it; on release it is removed only if it is still that folder. A lock held past the stale age and taken over by someone else is left to them.
- **Backup names** (`new_backup_names`, `backups_oldest_first`): the rotation orders backups by the time and count in their names as numbers (`-10` after `-2`), and a new backup is named after the newest one, so a clock that went back cannot rotate it out at once. Existing names keep their form (`<label>.<ms>-<n>.bak`); the Activity log reads them as before.
- **`settings.local.json` size limit**: read with the same 1 MB limit as the inventory (was 64 MB), so a change is never made to a file the list could not show.
- **Temp file name**: named after the file it replaces (`.settings.local.json.tmp.…` in a project's `.claude` folder, `.claude.json.tmp.…` as before).
- Already fixed on main, so not changed: an after-save check that fails with `Locked` no longer says "Nothing was changed" (#14).

## Tests (each seen failing on main 2279dd4 first)
- `a_write_landing_while_a_refused_replace_waits_is_not_overwritten` (failed: `numStartups` 12, Claude's 99 lost), variant `a_refused_replace_with_no_write_meanwhile_goes_through_on_a_later_try`.
- `a_lock_someone_else_took_over_is_left_to_them`.
- `a_backup_made_after_the_clock_went_back_is_kept_as_the_newest`, variant `backups_made_in_one_millisecond_are_rotated_in_the_order_made`.
- `a_settings_file_larger_than_the_inventory_reads_is_not_changed`, `the_settings_temp_file_is_named_after_the_settings_file`.

## Checks
- Full verify (`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`) on this branch from main 2279dd4: exit 0, 952 passed, 0 failed, deny "advisories ok, bans ok, licenses ok, sources ok".
- `cargo test -p klefki-core --lib docstore` 20 runs in a row: 112 passed each time.
- Windows and macOS: CI only (the refused-rename seam runs on every system; the real Windows sharing violation is covered by the existing `the_file_is_replaced_even_while_someone_has_it_open_for_a_moment`).

## Trade-off
- A backup made while the clock was far ahead pins later names to that time until the clock passes it, so the Activity log shows that time for them. Rotation stays correct.

## Not done (left open, larger than this slice)
- The lock is not refreshed while held (Claude's rule: stale after 10 s), and two waiters can both take over one stale lock (the window between the age check and the removal).
- `settings.local.json` is rewritten whole (duplicate keys collapse, huge integers become floats).
- Windows backups get no owner-only ACL.
- `<repo>/.claude` is created before the lock; the lock lives in that folder, so this stays.

## Next
- Cross-review, CI on three systems, merge.
