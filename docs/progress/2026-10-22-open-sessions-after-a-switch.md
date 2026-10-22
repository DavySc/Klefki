# What an open Claude Code session gets after a switch

Goal (slice S1 of the on/off plan, `docs/proposals/on-off-audit.md` on PR #101): say plainly what a Claude Code
session that is already open gets after Klefki writes a switch or a server, replacing "A running Claude Code session
may need a restart before it notices this." everywhere; and stop showing a server that is off in every known project
as simply off, since a project Claude Code opens later still loads it. How switching works is unchanged.

Facts this rests on (Claude Code 2.1.308, from the audit): a session never drops a server switched off from outside
and never starts one switched on or added, until it is restarted; `/mcp` in the session switches a server there at
once. A project Claude Code opens for the first time has no `disabledMcpServers`, so it loads every user, plugin and
connector server.

## Done
- Core: `ops::open_sessions` writes one sentence for every plan that writes servers or switches, carried in a new
  `PlanView::sessions` (not in `notes`). It names the projects when every edit is inside projects (folder names, at
  most three, then "and N more"), and adds "or switch the same servers there with /mcp" only when the plan is
  nothing but switches. Used for changes, profiles (use and stop) and Restore. Nothing written: `None`.
  Example: "Claude Code sessions already open in a keep the servers they started with until you restart them, or
  switch the same servers there with /mcp. New sessions get this."
- App: the review shows it in full text under the warnings (it was a muted note under the diff); once saved, the
  outcome toast says it again (only when something was written; not shown as a warning).
- CLI: `klefki-cli use` prints it as a `note:`.
- "Off" is not off for new projects: a user, plugin, organisation or claude.ai row off in every known project gets a
  `new: on` badge (hover: "Switched off in all N projects Claude Code knows. A project it opens for the first time
  still loads it."), the details' Enabled says "no, in all N projects; on in projects Claude Code opens later", and
  `klefki-cli list` adds "(off in all N projects, on in new ones)". The state stays `off`; JSON is unchanged.
- Tests: core `edits.rs` (one project with /mcp, user server naming both projects, user delete without projects,
  local delete without /mcp, nothing to do gives none), restore test asserts the sentence; app `review.rs` (review
  shows it; toast repeats it only when changed, not as a warning), `list.rs` (user, plugin and claude.ai rows; not a
  local row); CLI `render.rs` (note, state stays off, not for a local row). Goldens (core and CLI) and look
  snapshots updated: the only differences are the sentence and the badge.

## Checks (Linux)
- `cargo fmt --check`: exit 0. `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
- `cargo test --workspace`: exit 0; 1097 core, 516 app, 26 + 43 CLI tests passed.
- `cargo deny check`: advisories ok, bans ok, licenses ok, sources ok.
- Look snapshots: the review, toast and list images checked by eye (sentence under the warnings; `new: on` beside
  legacy-events).
- Not run here: macOS and Windows (CI); a real Claude Code session.

## Open / next
- Toast text is long (about five lines at 800 px); fine with today's toast, but S2 (Undo) also adds to it.
- The sentence is not given for a plan that only approves a project server (`settings.local.json`): whether a
  running session picks up an approval was not checked live.
- Next: S2 Undo for switches and profile use.
