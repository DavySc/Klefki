# "Only these servers there" ticked by default (2026-10-20)

Goal: the owner chose (decision card in the "already looks like this" thread, 13:56Z) that "Only these servers there"
starts ticked on each profile card. Follows #60.

## Done
- App: `Choice::default()` has `only: true`. Decision 012 says so.
- The "Nothing to change" message scrolls its whole block into view, not only its heading (#60 cross-review's optional
  nit).
- CLI unchanged: `klefki-cli use` keeps `--only` as an explicit flag (the card's default is a UI choice; a flag that
  changed meaning would surprise scripts).

## Checks
- `using_a_profile_is_reviewed_for_the_place_chosen` now asserts the box is ticked at first and that Use sends
  `only: true` without touching it, plus the unticked variant; it failed before the change. Tests that used the
  unticked behaviour untick first, and the one that ticked it no longer clicks.
- Full verify on main a3c778f + this branch: fmt, clippy -D warnings, `cargo test --workspace` (core 900, app 414, cli
  25 and 33), `cargo deny check`: exit 0.
- Real app at 800x480 (Xvfb, Claude Code 2.1.304): the box shows ticked on a new card; unticked in notes, the
  "Nothing to change" block near the bottom edge scrolls fully into view.

## Next
- None for this slice.

## Cross-review (approved, two optional items folded in)
- "This also turns off" names every server (`and_list`, not "and N more"), and in every project says in how many
  projects: `using_a_profile_alone_names_every_server_it_turns_off` failed before. Decision 012 re-wrapped to 120.
- Full verify again: exit 0 (core 901, app 414, cli 25 and 33).
