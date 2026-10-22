# Per-project switches: the list seen from one project

Goal (slice S3 of the on/off plan, `docs/proposals/on-off-audit.md`, findings 2 and 5): a "Where" choice on the
Connections page, All projects (the list as before) or one project. In one project the list shows what Claude Code
loads there, and Turn on / Turn off and **Space** act in that project only. Owner-approved (2026-10-22): such a
switch is saved without the review dialog and offers Undo instead; switches across all projects keep the review.
Also from S2: Ctrl/⌘+Z for the outcome's Undo.

## Done
- Core: `Change::SetEnabledIn { id, project, enabled }` switches a user, plugin, organisation or claude.ai server
  in one project (one `SetDisabled` op), or a local or project server in its own project (refused in another:
  "loc loads only in its own project, a."; a project Claude Code does not know is refused too). The review says
  "Turn off gw in b", the "already off in b" line, the open-session sentence, and warns when it will still not load
  there (policy, plugin off, another server of its name, a duplicate, a project server not approved) or when a
  plugin's duplicate loads instead once it is off there. It is undoable like any switch (`undo::kind_undoable`).
- Core: `api::in_project(snapshot, project)` (with `api::Here { off, held }`): how each server stands in one project,
  `None` for another project's own servers. Reuses `held_back`, plus "you have not approved / rejected this project
  server" and "the local server x has its name there".
- App: "Where" drop-down above the toolbar (All projects, then the projects by folder, typed into when many; hidden
  while Claude Code knows no project). In one project: rows that load there only; status "Off here" or "Not loaded"
  (hover: why); the cross-project badges (off in 3/17, on in new, blocked in, shadowed in…) give way; the details add
  "In <folder>: on/off/on, but not loaded: why"; the palette says "Turn off gw in b". A selection that does not load
  there is let go; going to such a server (palette, Secrets page, something just added) shows All projects again; a
  project Claude Code forgets returns the list to All projects.
- App: a one-project switch is prepared as usual; when its plan has no warning, nothing to review and nothing
  destructive, it is applied at once and no dialog is drawn. The outcome says "Turned off gw in b." plus which
  sessions to restart (no backup path; "Open activity log" is on the card) and offers Undo, which is applied the same
  way ("Turned gw back on in b."). With a warning, it opens the review as before. Nothing to change: the
  "already off in b." line, no dialog.
- App: Space switches the selection (all on where seen → off, else on); in All projects it goes through the review
  like the toolbar. Not while a text field or button has the keyboard (egui then wants it).
- App: Ctrl/⌘+Z uses the outcome's Undo (hover text names the keys); not while a text field has the keyboard.
- `CLAUDE.md` invariant amended (flagged for review): the one-project switch is the one exception to showing the diff;
  plan, backup and Activity log stay. `docs/PLAN.md` describes the Where choice.

## Tests
- Core `engine/tests/one_project.rs` (6): off in one project only and undone there; on in one where off everywhere;
  already so; local server in its own project and refused in another; unknown project refused; several in one
  project undone together. `ops::one_project` (2): `in_project` for user, local, shadowed and project servers;
  warnings for a shadowed user server and an unapproved project server. Golden `turn-off-user-in-one-project.txt`.
- App `tests/one_project.rs` (16): the lens shows what loads there and "Off here"; the Where list; selection let go;
  saved without review with Undo (also without review); reviewed with a warning; nothing to change; Space in a
  project and in All projects; Space with mixed on/off; Space on a focused button; Ctrl+Z quiet and reviewed;
  Ctrl+Z in a text field; details and palette; going to a hidden server; an Undo found done not offered again;
  forgotten project. Seen failing before the fix: the last three regression cases (undo found done, going to a hidden
  server) by reverting their fix.
- Existing test changed (flag): `a_plugins_server_is_switched_on_and_off_but_nothing_else_changes_checks_or_exports_it`
  pressed Space among the keys that must do nothing, from when Space was not wired. Space is a switch, which that test
  says plugin servers do allow; it now asserts Space prepares the same `SetEnabled` as the toolbar.
- Look: new `connections-one-project`, `connections-one-project-800x480`, `switched-in-one-project`;
  `diagnose(d)-in-project` changed only by the Where row (they are the only pictures whose snapshot has projects).

## Checks (Linux)
- `cargo fmt --check`: exit 0. `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
  `cargo clippy -p klefki -- -D warnings`: exit 0.
- `cargo test --workspace`: exit 0 (1115 core, 542 app, 28 + 48 CLI).
- `cargo deny check`: advisories ok, bans ok, licenses ok, sources ok.
- Not run here: macOS, Windows (CI), a real Claude Code session.

## Open / next
- The Where list is in folder order, not most recently used: Klefki does not read when a project was last opened.
- The choice is not remembered between runs.
- The palette has no "Undo" entry yet (Ctrl/⌘+Z and the card's button do it).
- Next: S4 Profiles "switch to" + `klefki-cli` stop/switch and the worktree fix.

## Review round 1 (cross-review on #104)
- B1: a one-project switch saved without the review dropped the review's notes (a profile that no longer puts the
  switch back, say). They are now said on the outcome, for the switch and its Undo. Regression test
  `a_quiet_switch_says_the_reviews_notes_on_its_outcome` (seen failing before the fix).
- B2: Space and Ctrl/⌘+Z counted the system's key repeat, so a held key saved again after each save. Both now act
  on a first press only, with exactly their modifiers (Shift+Space, Ctrl+Space and Ctrl+Shift+Z do nothing here).
  Regression tests `a_held_space_switches_once_and_shift_or_ctrl_space_not_at_all`, `a_held_ctrl_z_undoes_once`
  (seen failing before the fix).
- Polish: Turn on / Turn off hover texts name Space, and in one project say "Turn off in a (Space), saved at once
  with Undo". The toolbar hover test's expected Turn on / Turn off texts changed to "Turn on (Space)" /
  "Turn off (Space)" with them (the same check, new wording).
- Left for later (reviewer's non-blocking): announcing the outcome to screen readers (a live region; the known
  follow-up from earlier slices), and a "Saving…" sign during a quiet save.
- Checks: fmt, clippy exit 0; `cargo test --workspace` exit 0 (1115 core, 546 app, 28 + 48 CLI).
