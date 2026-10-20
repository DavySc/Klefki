# 012: Server sets (2026-10-20)

## Context
Roadmap item 4 (`docs/PLAN.md`): named groups of servers, switched on together for a project. The owner calls them
profiles; the next release waits on them. Claude Code itself has no sets: it switches each server per project, by
name, in `disabledMcpServers` (decision 002, 008 finding 9).

## Decision
- A set is a name and a list of `ConnectionId`s, so it holds the servers as the files spell them (scope, project,
  name), whatever is shown. Any server Klefki lists can be in one, plugin servers and claude.ai connectors included.
- Sets are Klefki's own data: `sets.json` beside the backups folder, like `activity.jsonl`, written whole and owner
  only (`fsx::write_whole`, `create_private_dir`). No Claude Code file is written to keep a set. This is a new write
  target, outside `config/docstore.rs` because it is not Claude Code's configuration.
- The file is read as untrusted: at most 512 KB, 100 sets, 500 servers a set, names of at most 64 characters with
  nothing a screen hides or reorders; ids that are not ones Klefki makes, and repeated names (case aside), are left
  out and said. While part of it cannot be read Klefki changes no set, so a save never loses what it skipped.
- Using a set is a change like any other (`Change::UseSet`): Prepare → diff → Apply over `disabledMcpServers` in
  `.claude.json`, so the review, backups, the Activity log and restore apply. It is used in one project Claude Code
  knows, or in every one it knows today (the review says a project opened later is not covered).
- It turns its servers on there. With "Turn off every other server there" (`only`), every other server in that
  project goes off, and the review names them. A name a member shares with another server stays on, since Claude Code
  switches by name.
- A local or project server loads only in its own project: elsewhere it is left as it is, and the review says so.
  Turning on a project server does not approve it; the review says so. Servers no longer configured are kept in the
  set (they may come back) and named in the review; a set with none left is refused.
- Turning a set on does not record that it is "active": Klefki shows what is on from the files, as before, so nothing
  can disagree with them.

## Not yet
- Sets from the command palette and `klefki-cli` (the CLI only reads today; writing from it is a new capability).
- Making a set from the servers selected on the Connections page.
