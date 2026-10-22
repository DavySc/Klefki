# Undo for server switches and profile use

Goal (slice S2 of the on/off plan, `docs/proposals/on-off-audit.md`, finding 4): any change that only flips
switches (`disabledMcpServers`) offers Undo on its outcome message: turning a server on or off, using a profile (with
"Only these servers there", which can turn off many servers at once), and stopping one. Also from the #102 review:
the success message faded after 4 s while it now carries a restart sentence and a backup path.

## Done
- Core, `undo.rs`: `api::Undo` is what puts a change back: each switch it flipped, as it left it, and, per profile,
  what Klefki remembered of its switches before and after (`profile-uses.json`). The engine works out the flips
  when it prepares a plan (against `.claude.json` as read then) for plans of `SetEnabled`, `UseProfile`,
  `StopProfile` without delete, or `Undo`, that write nothing but switches; once applied and changed, it sends the
  undo with `Event::Applied { undo }`.
- `Change::Undo(undo)` is a plan like any other (Prepare → review → Apply; reviewed on its own, like Restore). Each
  switch goes back only while it is still as the change left it ("memory is already back on in designs." otherwise);
  a project Claude Code no longer knows is left out and said. Profiles' records go back to before while they are
  still as the change left them (`profile_uses::put_back`), else left and said. An undo is itself undoable (redo).
  Not offered for "Stop using and delete" (the profile would not come back), deletes, approvals, or anything else.
- App: the outcome message offers Undo for such a change (hover: "Switch them back as they were, after a review"),
  and like a Move's Undo it stays until used or dismissed. A success message without a choice now stays
  `2 s + 15 characters a second` (4 s at least, 20 s at most), and while the pointer is on it.

## Tests
- Core `engine/tests/undo.rs`: switch undone and redone; "Only" use undone whole (profile no longer in use); stop
  undone and remembered again; undoing a hand switch gives a profile its record back; switch changed since left and
  said, all back gives nothing to do; project gone left out; only switch-only plans get an undo; undo reviewed on
  its own; profile record changed since left and said.
- App `tests/outcome.rs`: Undo offered and stays, prepares `Change::Undo`; none when nothing changed; long success
  message stays longer than 4 s and goes after its time; held while pointed at (seen failing without the hold).

## Checks (Linux)
- `cargo fmt --check`: exit 0. `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
- `cargo test --workspace`: exit 0; 1106 core, 521 app, 28 + 48 CLI tests passed.
- `cargo deny check`: advisories ok, bans ok, licenses ok, sources ok.
- Not run here: a real Claude Code session, macOS, Windows (CI covers the tests there).

## Open / next
- S3 can make a one-project switch's Undo apply without the review dialog (owner-approved for S3 only); here Undo
  still goes through the review, as the Prepare → diff → Apply invariant says.
- `klefki-cli` has no undo (it prints the backup it kept; Restore covers it).
