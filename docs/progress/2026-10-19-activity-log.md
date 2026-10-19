# Activity log page (2026-10-19)
Roadmap slice (M3): the Activity log page lists every change Klefki saved, newest first, with the backup it kept before each one. It was a "Coming soon" page. The app UI review (findings 7 and 8) asked for the backup path to live somewhere after the 4-second toast is gone; this is that place.

- Core `activity.rs`: a private, append-only log `activity.jsonl` next to the backup folder (like `recovery/`), one JSON line per change: time, the review's summary sentences, an error when the change stopped part way, and the backup paths. Cut back to the newest 200 lines once it passes 256 KiB. Read as untrusted: 1 MiB limit, unreadable lines skipped and counted, every string sanitised and clipped. Backups no line mentions (kept before the log existed) are listed on their own, dated from their file name. A backup that rotation removed is marked as gone. Times are local ("Today 14:05", "Yesterday 09:12", "13 Oct 18:40", "13 Oct 2025 18:40") with the offset each moment had (daylight saving): `localtime_r` on Unix, `SystemTimeToTzSpecificLocalTime` on Windows (adds the `Win32_System_Time` feature to `windows-sys`).
- Engine: `Command::Activity` → `Event::Activity`. `apply` logs a change that wrote something; a change that failed after writing something is logged with its error; a plan that writes nothing is not logged. A log that cannot be written leaves the change alone and adds a note to the result.
- App `activity.rs` + small `shell.rs` hooks: the page is read only when opened (nav, command palette "Activity log") and dropped when left; while open, a saved or failed change reads it again without flashing "Loading…". Each entry: when, what, "Did not finish: …" for a failure, "Backup of .claude.json" with Copy path and the full path. No spinner (it kept the window repainting).

## Checks
- Tests written first and seen failing (engine: `Command::Activity` did not exist; app: page not built). New: 13 core unit tests in `activity.rs`, 2 engine tests (`a_saved_change_is_in_the_activity_log_with_its_backup`, `a_change_that_writes_nothing_is_not_logged`), 9 app tests in `crates/app/src/activity.rs` (lazy load, rendering, copy, empty, problem, drop on leave, reload on apply, palette, not "Coming soon").
- Full verify (`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`): exit 0; tests 269 app + 559 core + 15 + 20 cli, 0 failed; deny "advisories ok, bans ok, licenses ok, sources ok".
- `cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu -- -D warnings`: exit 0 (Windows code compiles; not run). macOS not built locally; CI covers it.
- Real app (release, Xvfb, temp `HOME`, `CLAUDE_CONFIG_DIR` and `XDG_DATA_HOME`): turned off a user server and deleted a local one through the review dialog; the log file has the lines and the page shows both with their backups, plus a copied-in older backup as "A backup from before this log was kept". 1200x720 and 800x480 look right. Screenshots in the project folder `reviews/2026-10-19-activity-log/`.
- Memory: RSS 126.6 MB at start, 127.3 MB after opening the page, two changes and leaving it.

## Open / next
- No restore from a backup yet. Restoring a whole `.claude.json` would also undo Claude Code's own writes since; a safe restore needs its own design (per change, through Prepare → diff → Apply).
- The success toast could point to the Activity log for the backup path.
- Overlaps PR #12 (Export) in `shell.rs`, `engine.rs`, `api.rs` and `lib.rs`; the edits here are small and in other places.
