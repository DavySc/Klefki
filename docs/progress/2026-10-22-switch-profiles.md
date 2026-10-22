# Switch between profiles

Goal (slice S4 of the on/off plan, `docs/proposals/on-off-audit.md`, findings 6, 7, 9, 10): change a project from
one profile to another in one step, with one review and one Undo; show the profile in use in the list seen from one
project (S3); `klefki-cli switch` and `stop`; and fix `klefki-cli use` in a git worktree.

## Done
- Core: `Change::SwitchProfile { from, name, members, project, only }`. Each profile of `from` is stopped in that
  project as `StopProfile` stops it, one after another, each on the files as the one before leaves them (a snapshot
  with the stop's switches and records applied, `ops::after_stop`); then `name` is used there on the files as they
  will be. Stopping `name` later goes back to before all of them. A switch a stop puts back and the use sets again is
  written once, as it ends (the diff and the file's list order stay calm). "This also turns off" names only servers
  on now. The review says "Switch designs from Writing to Research, …" and "Writing is no longer in use in
  designs.", not each switch the stop puts back. With no `from` (or only `name`), it is a use. Undoable (S2).
- Core: `Resolved::profiles_stopped` is a list; on Apply the stops are forgotten before the use is recorded, so the
  new profile's record is made against what is left. `api::profiles_in_use_in(snapshot, project)`.
  `Snapshot::profiles` holds the profiles (name and servers), not only names, so the list can switch without opening
  the Profiles page. `api::load_profile_uses()` for the CLI.
- App: on a profile's card, with a project chosen where another profile is in use, the main button is
  "Switch from Work…" and "Use as well…" keeps the old stacking; a line says "Work in use in notes.". In the list
  seen from one project, a menu beside Where (on its own line under it in a narrow window) names the profile in use
  ("No profile" otherwise) and offers Switch to / Use / Stop using, each reviewed as before: a profile can turn many
  servers off, so the one-project no-review exception (S3) is not stretched to it.
- CLI: `switch PROFILE [--project DIR] [--only] [--yes]` (stops whatever is in use there; `--all` refused) and
  `stop PROFILE [--project DIR | --all] [--yes]` (`--only` refused), with their own usage lines and `use`'s exit
  codes. `profiles` prints "In use in …" and JSON `in_use`. Decision 006 and 012 updated.
- Bug (audit finding 7): in a git worktree, `use` said "start Claude Code there once", which cannot help, as Claude
  Code uses the main checkout's project there. `git_root` now follows the worktree's `.git` file to its `commondir`
  and the main checkout. A submodule's `.git` file (no `commondir`) or a broken one is not followed.

## Tests
- Core `engine/tests/switch_profile.rs` (3): switch and its review, files, records, undo, stop back to before both;
  two profiles in use that agree on a switch both stopped (seen failing when each stop used the same snapshot);
  without only, from itself, from none, unknown project.
- CLI: `use_in_a_git_worktree_uses_the_main_checkouts_project` (absolute and relative `gitdir`, root, subfolder,
  `--project`; seen failing before the fix), nearby `a_git_file_that_is_no_worktree_is_not_followed`,
  `switch_and_stop_move_a_project_between_profiles_and_back`, golden `switch-unconfirmed.txt`, args unit test.
- App `shell/tests/switch_profile.rs` (3): the card's Switch from / Use as well / Every project; the lens menu's
  Switch to, Stop using, Use where none is in use; no menu without profiles.
- Look: new `profile-switch-from`, `connections-profile-menu`, `connections-profile-menu-800x480`; no existing
  picture changed. Golden `help.txt` changed for the two new commands (intended).

## Checks (Linux)
- `cargo fmt --check`: exit 0. `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
  `cargo clippy -p klefki -- -D warnings`: exit 0.
- `cargo test --workspace`: exit 0 (1119 core, 550 app, 29 + 51 CLI).
- `cargo deny check`: advisories ok, bans ok, licenses ok, sources ok.
- Not run here: macOS, Windows (CI), a real Claude Code session, a real `git worktree` through the CLI (the layout the
  test writes was checked against `git worktree add` from git 2.43).

## Open / next
- The palette has no "Switch to" entry yet (the card and the lens menu do it).
- The lens menu always switches with "Only these servers there"; the card has the choice.
- Next in the plan: S5, start a session with a profile (`claude --settings` allowedMcpServers, `klefki-cli run`).
