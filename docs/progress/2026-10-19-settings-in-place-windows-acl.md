# settings.local.json changed in place; owner-only backups on Windows (2026-10-19)

Goal: part 2 of the docstore leftovers from PR #24 (part 1, the lock, is `2026-10-19-docstore-lock.md`).

## Done
- **`settings.local.json` in place** (`approve`): approving a project server replaces only the bytes of `enabledMcpjsonServers` and `disabledMcpjsonServers`, found with `jsonspan` as in `.claude.json`. Duplicate keys elsewhere, huge integers, number spelling and layout stay exactly as they were. A missing list is added after the last member in that member's style (one line stays one line). A new file is written as before (`{` + list on lines of their own + line break). A file with either list twice is refused untouched, since collapsing them would drop one. `Current::render` (the whole-file rewrite) is gone.
- **Owner-only on Windows** (`config/owner_only.rs`): `restrict` sets a protected access list with one entry, the current user's, with full access (inherited by what is made in a folder). `make_private` (the hard-link backup after the rename), `create_private_file` (kept copies, the copy fallback, the activity log, swap recovery files, the takeover marker) and `create_private_dir` (when it makes the folder) use it. Best effort, as on Unix: a drive without access lists keeps what it has. A folder that already existed is left as it is (as Unix `DirBuilder::mode` does); the files made in it are restricted one by one. Uses `windows-sys` (feature `Win32_Security_Authorization` added), `unsafe` blocks each with a SAFETY note.

## Tests
- Seen failing on main first: `approving_changes_only_the_two_lists_and_keeps_every_other_byte`, `a_list_added_to_a_one_line_settings_file_keeps_the_rest_exactly`, `a_settings_file_with_a_list_twice_is_refused_untouched`. The existing settings tests pass unchanged.
- Windows only: `backups_and_their_folder_are_owner_only_on_windows` (backup link, backup folder, kept copy, a private file in an open folder). Not run here (no Windows); runs in CI. On main it cannot pass: `make_private` was a no-op on Windows.

## Checks
- Full verify (`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`) on this branch from main e5583c6 (with #27 and #28): exit 0; core 640, app 305, cli 17 + 22, 0 failed; deny "advisories ok, bans ok, licenses ok, sources ok".
- `cargo clippy -p klefki-core --all-targets --target x86_64-pc-windows-gnu -- -D warnings`: exit 0.
- CI on 15eb385: green on Windows, macOS and Linux; the Windows log shows `backups_and_their_folder_are_owner_only_on_windows ... ok` (core 635 there).
- Cross-review (another thread, review on #30): ready, nothing blocking; each `unsafe` block checked. Optional O1 folded in as a comment: unlike Unix, folders made above the private one keep their inherited access.

- Main f0b1d91 (with #26 and #25) merged in: full verify exit 0; core 654, app 314, cli 18 + 25, 0 failed; deny ok. Windows clippy exit 0.

## Left open
- Existing backup folders on Windows keep their old access list; new backups in them are owner-only.

## Next
- Merge #30 on green CI.
