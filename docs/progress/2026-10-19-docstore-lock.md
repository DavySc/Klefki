# Docstore lock: refreshed while held, one takeover at a time (2026-10-19)

Goal: the docstore leftovers PR #24 listed, part 1 of 2. This slice: the lock is refreshed while Klefki holds it, and two waiters cannot both take over one stale lock. Part 2 (next PR): `settings.local.json` changed in place instead of rewritten whole, and owner-only permissions for Windows backups.

## Done
- **Refresh while held** (`Lock::held`, `refresh`, `touch`): a held lock's modification time is set to now every half stale age (5 s), the same rule as Claude's own `proper-lockfile`, on a small thread that lives only while the lock is held and stops at once when it is dropped. A refresh first checks the folder is still the one we made or last refreshed. If it is not (ours was held past the stale age and taken over), it is never refreshed again and is left to them when ours is dropped. On Windows the folder is opened with backup semantics and only the right to write attributes.
- **One takeover at a time** (`take_over`): a stale lock is removed only under Klefki's takeover marker, `<label>.takeover` in Klefki's own backup folder (never next to Claude's file). Under the marker the lock's age is checked again, so a lock another waiter took over meanwhile is fresh and left alone. A marker that is still there after the stale age was left by a Klefki that ended in the middle; it is removed and the next try goes ahead. The marker is created with `create_new`, so no OS file locks are needed (keeps rust-version 1.88).
- `Docstore::lock_stale` (default 10 s) makes the age a field, as `lock_wait` already was, so tests can use a short one.

## Tests (the first three seen failing on main efe73f2 first)
- `a_lock_held_longer_than_the_stale_age_is_refreshed_and_not_taken_over` (failed: taken over after 2.5 s with a 1 s stale age).
- `two_waiters_cannot_both_take_over_one_stale_lock` (failed: both held it).
- `a_takeover_marker_left_by_a_klefki_that_ended_is_cleared_once_stale` (failed: a fresh marker did not hold the second waiter back).
- Variant `a_lock_taken_over_while_held_is_neither_refreshed_nor_removed` (passes on main, which never refreshes; fails when the refresh skips the "still ours" check).
- Mutations: refresh without the "still ours" check fails the variant; takeover without the second age check fails `two_waiters_…`.

## Checks
- See the PR description for the final runs.

## Left open
- Claude Code does not know our marker: Claude and Klefki finding the same lock stale at the same moment can still both take it (microsecond window). The validation before the rename still keeps either write from overwriting the other.
- A refresh and a takeover by someone else in the same microsecond can leave us refreshing theirs once; at worst our drop then removes theirs. Same window as Claude's own library.

## Next
- Cross-review, CI on three systems, merge. Then part 2.
