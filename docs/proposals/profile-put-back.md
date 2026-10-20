# Proposal: deleting a profile puts its servers back (2026-10-20)

Status: the owner chose option C on 2026-10-20, with the wording "In use in" / "Stop using" (a profile is something
you leave, so leaving it puts things back), and approved `profile-uses.json`. Built; decision 012 records it.

## The ask
The owner made a profile with one server off in the project Designs. That server was on everywhere before. Deleting
the profile should leave it on again, "exactly as it was before". The owner also floated a template profile (a
global default) that models each server's default state.

## Why it does not happen today
- Using a profile writes Claude Code's own switches (`disabledMcpServers` in each project entry of `.claude.json`,
  decision 002). With "Only these servers there", ticked at first since #62, it also turns off every other server.
- Klefki keeps no record of what a use changed (decision 012: "Using a profile records nothing as active"). The
  profile is only a name and a list of servers, so deleting it cannot know what to put back.
- The delete dialog says so: "Delete the profile X? Its servers stay as they are." (`crates/app/src/profiles.rs`).
- The only way back is Restore on the Activity log. It puts every project's switches back to the backup, so it also
  undoes anything changed since, by Klefki, by `/mcp` in Claude Code, or by hand (the review lists it, but the user
  has to spot it). The log is cut at 256 KB and backups rotate, so it is not a lasting answer.

## What "as before" should mean
"As before" is not "as the backup was": other changes made since must survive. It is: **each switch this profile
flipped goes back to what it was, unless something else changed it since.** That is selective undo: undo one
earlier action while later ones stay (Berlage 1994, ACM TOCHI). Where a later change touched the same switch, the
standard answer is to leave it and say so, not to guess.

## Options

### A. Global template profile (the owner's idea)
A special profile that says, per server, on or off by default; other profiles are layered on top, and deleting one
falls back to the template.

- For: matches a "modes" mental model (like Apple's Focus modes, where settings go back when the Focus is turned
  off, [Apple support](https://support.apple.com/guide/mac-help/mchl613dc43f/)).
- Against, and why I do not recommend it:
  - It is a second source of truth. Claude Code's real default already exists: a server is on unless named in
    `disabledMcpServers` for that project. Claude Code's `/mcp` toggles, and edits by hand, write that list without
    Klefki knowing, so the template and the files drift, and Klefki must pick a winner on every snapshot. That is the
    disagreement decision 012 avoided by recording nothing as active.
  - It cannot do the one thing a template promises: projects opened later. Claude Code makes a project entry itself
    when it first opens a folder; Klefki is not running then (decision 012, "projects opened later are not covered").
  - Visible modes are only safe when their state is always shown and always true
    ([NN/g, UI modes](https://www.nngroup.com/videos/ui-modes-modals/)). A layer that Claude Code can silently
    override would not be.
  - It adds a concept every user must learn and keep up to date, for a need most users never have (servers are on
    by default; the default rarely needs modelling).

### B. Delete just deletes (today), with Undo after use
Keep delete as is; add an Undo on the toast after a use and point to Restore.

- For: smallest. Undo beats a warning
  ([Raskin, "Never use a warning when you mean undo"](https://alistapart.com/article/neveruseawarning)).
- Against: it does not meet the ask. An Undo shown for seconds after the use does not help days later, when the
  user deletes the profile.

### C. Remember what each use changed; put it back on delete (recommended)
- When a profile is used, Klefki records each switch it flipped: project, server name, and the state before. Only
  the switches it really changed, so a use that changed nothing records nothing.
- **Delete** asks: "Delete the profile Writing? It changed 3 servers in Designs. **Put them back and delete** /
  **Delete only** / Cancel." With nothing to put back (never used, or already put back), it stays the plain
  "Delete profile" of today. Buttons name their result, not "OK"
  ([Primer, confirmation dialog](https://primer.style/product/components/confirmation-dialog/guidelines);
  [NN/g, heuristic 3, user control and freedom](https://www.nngroup.com/articles/ten-usability-heuristics/)).
- The card also gets **Put back in Designs** (or "in 4 projects"), so the user can stop using a profile without
  deleting it. That is the same action; delete only offers it.
- Putting back is a change like any other: Prepare → diff → Apply over `disabledMcpServers`, with the usual review,
  backup and Activity log line, and Restore can undo it in turn. Nothing new writes Claude Code's files.
- A switch goes back only if it is still as the profile left it. One changed since (by `/mcp`, by hand, by another
  profile, by a Restore) is left alone and named in the review: "memory: you turned it on again since; left as it
  is." This is the check that makes "as before" safe.
- The use's review says so up front: "Klefki remembers what this changes, so deleting the profile can put it back."
  This sets the expectation before the act (feedforward; [NN/g heuristic 1, visibility of system
  status](https://www.nngroup.com/articles/ten-usability-heuristics/)).

## Edge cases under C
| Case | What happens |
|---|---|
| Changed since, by `/mcp`, by hand or a Restore | That switch is left and named. The rest go back. |
| Profile used twice in one project | The record keeps the first "before" of each switch, so putting back returns to before the first use. |
| Writing used, then Research used in the same project | Each records only what it flipped. Putting back Research returns to Writing's state. Putting back Writing first skips any switch Research changed later, and names it ("Research set it later"). |
| Used in every project | One record per project. Put back offers every project at once; the review lists them. |
| Server added after the use | Not in the record, so untouched. It is on, as Claude Code starts every new server. |
| Server removed after the use | Its switch is skipped and said ("no longer configured"). |
| Server renamed or moved through Klefki | The record follows it, as profiles do today. Outside Klefki: treated as removed. |
| Project gone from `.claude.json` | Skipped and said. |
| Uses made before this exists (0.2.0) | No record: delete says it cannot put them back, and points to Restore on the Activity log. |
| Delete only | The record is dropped with the profile. |
| Profile edited (servers added or removed) after a use | The record is about switches, not members, so put back still reverses exactly what the use did. |
| `klefki-cli use` | Records the same way. A `klefki-cli put-back NAME` can follow later. |

## Cost and risk
- Data: a few dozen bytes per switch. Read only with the profiles (already read per snapshot for the palette), so no
  start-up cost and negligible RAM.
- Storage needs the owner's call (it is Klefki's own data, not Claude Code's, but a write target is an invariant):
  1. **A new `profile-uses.json` beside `profiles.json`** (recommended), same rules as `profiles.json` (decision 012:
     size-limited, read as untrusted, written whole and owner only, only when read in full). `profiles.json` keeps
     version 1, so Klefki 0.2.0 can still edit profiles after a downgrade. Needs the CLAUDE.md exception widened to
     this file.
  2. Inside `profiles.json` as version 2. One file, but Klefki 0.2.0 then refuses to change any profile, by design.
- Size: M. Core (record on apply, put-back plan with the three-way check, rename follow), app (dialog, card button),
  tests for every row in the table above.

## Recommendation
Option C, stored in `profile-uses.json`. It does what was asked ("back on after deleting"), keeps Claude Code's files
the single truth, never undoes a change the user made since, and adds no new concept to learn: the user sees
"Put back", not "template", "layer" or "default". If a global default is still wanted later, the useful form is
narrower, "new projects start with these off", and it depends on something Klefki cannot do while it is closed.

Sources: [NN/g, 10 usability heuristics](https://www.nngroup.com/articles/ten-usability-heuristics/);
[NN/g, UI modes and modals](https://www.nngroup.com/videos/ui-modes-modals/);
[A. Raskin, A List Apart (2007)](https://alistapart.com/article/neveruseawarning);
[Primer, confirmation dialog guidelines](https://primer.style/product/components/confirmation-dialog/guidelines);
[Apple, Focus on Mac](https://support.apple.com/guide/mac-help/mchl613dc43f/);
T. Berlage, "A selective undo mechanism for graphical user interfaces based on command objects", ACM TOCHI 1(3),
1994 ([record](https://publica.fraunhofer.de/handle/publica/185571)).
