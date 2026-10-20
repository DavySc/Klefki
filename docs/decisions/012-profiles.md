# 012: Profiles (2026-10-20)

## Context
Roadmap item 4 (`docs/PLAN.md`): named sets of servers, turned on together in a project. The owner calls them
profiles, as mcpm, MCPMate and Docker do, and the next release waits on them. Claude Code itself has no profiles: it
switches each server per project, by name, in `disabledMcpServers` (decision 002, 008 finding 9).

## Decision
- A profile is a name and a list of `ConnectionId`s, so it holds servers as the files spell them (scope, project,
  name), whatever is shown. Any server Klefki lists can be in one, plugin servers and claude.ai connectors included.
- Profiles are Klefki's own data: `profiles.json` beside the backups folder, like `activity.jsonl`, written whole and
  owner only (`fsx::write_whole`, `create_private_dir`). No Claude Code file is written to keep a profile. This is a
  new write target outside `config/docstore.rs`, since it is not Claude Code's configuration; the owner approved it
  (2026-10-20) and CLAUDE.md records the exception.
- The file is `{"version": 1, "profiles": [{"name", "members": [ids]}]}`, read as untrusted: at most 512 KB, 100
  profiles, 500 servers each, names of at most 64 characters with nothing a screen hides or reorders.
- Nothing in it is changed unless it was read in full. Each of these is said on the page and stops every save, delete
  and rename-follow until the file is fixed: a `version` other than 1 (a newer Klefki, say, after a downgrade); more
  profiles or servers than the limits; fields Klefki does not know, at the top or in a profile; ids that are not ones
  Klefki makes; names that are empty, too long, hidden or repeated (case aside). Klefki writes version 1 only.
- Using a profile is a change like any other (`Change::UseProfile`): Prepare → diff → Apply over
  `disabledMcpServers` in `.claude.json`, so the review, backups, the Activity log and restore apply. It is used in
  one project Claude Code knows, or in every one it knows today (the review says a project opened later is not
  covered).
- It turns its servers on there. With "Only these servers there" (`only`), every other server in that project goes
  off, and the review names them. A name a member shares with another server stays on, since Claude Code switches by
  name.
- The review says what Claude Code will really load. A member that still will not load there is named, with why:
  another server of its name loads instead (a local or approved project server over a user one), policy or the
  organisation holds it back, its plugin is off there, Claude Code leaves it out as a duplicate, or connectors are
  off. Turning on a project server does not approve it; the review says so too.
- A local or project server loads only in its own project: elsewhere it is left as it is, and the review says so.
  Servers no longer configured are kept in the profile (they may come back), named on its card and in the review; a
  profile with none left is refused.
- A server renamed or moved through Klefki stays in its profiles: the review of the rename or move says which
  profiles hold it, and once it is applied they hold it under its new name or place.
- Using a profile that would change nothing opens no review: its card says so, naming which of its servers are
  already on (or already the only ones on) where, with what would still keep one from loading. Unticked, it points to
  "Only these servers there", since its servers are usually on already. A review that changes nothing, for any change,
  names what is already so rather than only "the configuration already looks like this". "This also turns off" names
  only servers that are on now.
- Using a profile records nothing as "active": Klefki shows what is on from the files, so nothing can disagree with
  them. A profile's card names the projects where it is on alone: of the servers Claude Code switches there, its
  members are on and every other one is off, so using it there with "Only these servers there" would change nothing,
  and each member there loads (none held back by policy, another server of its name, its plugin, as a duplicate, or as
  a project server not approved there). It is worked out again for each new snapshot.
- `klefki-cli profiles` lists them and `klefki-cli use` uses one, through the same plan, after showing it and asking
  (decision 006; the owner chose this on 2026-10-20).
- The command palette offers the Profiles page, New profile, and "Use profile NAME…" for each profile, which opens the
  page with where to use it taking the keyboard. Each snapshot names the profiles, read with it like the list of
  kept definitions, so opening the palette reads nothing; a save or delete updates the names at once.

## Not yet
- Which profiles a project is on alone in, shown on the Connections page too.
- Undo after using a profile (Restore in the Activity log puts it back), and a backup of `profiles.json` before a
  profile is deleted.
- A rename or move made outside Klefki (by Claude Code's own commands, or by hand) leaves the profile naming the old
  server, shown as no longer configured.
