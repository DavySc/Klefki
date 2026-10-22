# Turning servers on and off: audit and plan (2026-10-22)

Audit only; no code changed. Read with decisions 002 (where switches live), 008-011 (plugin, organisation, policy and
claude.ai connector servers) and 012 (profiles).

## The goal, as assumed
Klefki's point is to keep loaded only the servers a task needs, so context is not spent on unused or overlapping
servers, **without** the model ever missing a server it needs or working from a stale set. Two failure kinds follow,
and this audit ranks them in that order:

1. **Quality loss**: a needed server is off, or the session the user is in does not have what Klefki says is on.
2. **Waste**: an unneeded or duplicate server stays loaded.

"Servers" here covers every kind Klefki lists: user, local, project (`.mcp.json`), plugin, organisation and
claude.ai connectors. All of them are switched the same way, by name, in `projects[P].disabledMcpServers`
(decision 002), so they are one on/off story, and Profiles are the natural way to drive it.

## What was checked, and how
- Code: every path that writes a switch: `Change::SetEnabled` (toolbar, palette), `UseProfile` / `StopProfile`
  (Profiles page, palette, `klefki-cli use`), `MigrateLegacyDisabled`, `Approve`, `Restore`; the list and detail views.
- Live, Claude Code **2.1.308** on Linux, temp `HOME` and `CLAUDE_CONFIG_DIR`, three small stdio servers that log each
  start and request with a timestamp (two of them expose the same tool names). An interactive session ran in tmux and
  was read through `/mcp` and `/context all`. `klefki-cli` (main at e2f6c50) ran against the same files.
- Not checked live: macOS, Windows, claude.ai connectors (they need a signed-in account), and plugin servers in a
  running session.

## Live facts (2.1.308)
| # | Fact | Evidence |
|---|---|---|
| L1 | A **running session never notices a server Klefki turns off.** It keeps the process and its tools until it is restarted. | `notes` added to `disabledMcpServers` from outside: still connected in `/mcp` and listed in `/context all` 20 s later and after other events; its process kept running. |
| L2 | A running session also ignores a server Klefki turns **on**, unless something makes it reload MCP settings. | `gh_b` taken off the list: not started. |
| L3 | A change to an MCP setting in `settings.json` (`deniedMcpServers`, even naming a server that does not exist) makes a running session **start** servers that are now on. It does **not** stop servers that are now off or denied. Rewriting `settings.json` without an MCP key did nothing. A server newly added to `.claude.json` was not picked up either. | Timestamps: settings written at t, `gh_b` started at t+1; `notes` (off) and a denied server stayed connected. |
| L4 | `/mcp` → Disable in the session stops the process at once, and re-reads `.claude.json` before writing: a switch Klefki wrote meanwhile survives. | `['notes']` + Disable `gh_a` gave `['notes', 'gh_a']`. |
| L5 | `/mcp` shows the session's own view, not the file: after L1/L2 it shows the old state. | Screens captured. |
| L6 | **Per-session sets need no file write**: `claude --settings '{"allowedMcpServers":[{"serverName":"gh_a"}]}'` starts only `gh_a`, interactive and in `mcp list`. A `deniedMcpServers` list works the same. | Only `gh_a.log` was created. |
| L7 | An `allowedMcpServers` given that way is **added to** one in the user's `settings.json` (union), and does **not** turn on a server that is switched off in the project. | User allowed `gh_b` + flag allowed `gh_a` loaded both; flag allowed `notes` stayed "Disabled for this project". |
| L8 | A **git worktree** uses the main checkout's project entry: its switches, no entry of its own. | `mcp list` in the worktree showed the main project's off switch; no new entry appeared. |
| L9 | With tool search on (the default here), MCP tools cost **0 tokens up front**: "MCP tools · loaded on-demand · 7 tools · 0 tokens". Tool search is off for some models and endpoints and can be turned off (`ENABLE_TOOL_SEARCH`), and then every tool schema is paid in full. | `/context`; strings in the binary. |
| L10 | Two servers with the same tool names are both offered (`mcp__gh_a__create_issue`, `mcp__gh_b__create_issue`); nothing warns. | `/context all`. |

L1-L3 matter most: Claude Code's own notice ("re-enable via /mcp") and Klefki's ("A running Claude Code session may
need a restart") both understate it. In a running session an off switch is **never** honoured, and an on switch only
by accident.

L9 changes what "waste" means. With tool search on, an extra server costs a process, start-up time, its server
instructions and a place in the search catalogue, and it adds **ambiguity**: overlapping tools lower the model's
chance of picking the right one ([ToolScope, arXiv 2510.20036](https://arxiv.org/abs/2510.20036), 8.4 to 38.6 %
better tool selection after merging overlapping tools and filtering by context). With tool search off, the full schemas are paid on every
request too. So overlap is a **quality** problem as well as a cost one, which supports the owner's framing.

## Findings, ranked by risk
**R = quality risk, W = waste, F = friction.** "Verified" means reproduced here; "read" means from the code.

### High
1. **R: a running session keeps the old set, and Klefki says it only "may" need a restart** (L1, L2; read
   `ops.rs` note). Using a profile with "Only these servers there" while a session runs leaves every server running
   there; turning one on gives the session nothing. Klefki does not know whether a session is running in that project
   and never says which sessions are stale. Verified.
2. **R: the main switch of a user server, plugin server, organisation server or connector acts on every project at
   once** (`switch_everywhere`). There is no "off in this project" on the Connections page; only a profile can do
   that. A single **Turn on** wipes every per-project off switch, including ones a profile set, which that profile then
   forgets (`docs/decisions/012-profiles.md`, line 68: a switch written by another change is the user's from then on). A single **Turn off** removes a server from every project, including those whose work
   needs it. Read; the review does list each project, but as a diff.
3. **R: "off" is not off for new projects.** A user server shown "off" (off in every known project) loads in any
   project Claude Code opens later, and in any folder outside git; `klefki-cli list` prints `off`. The review warns once
   ("a project it first opens later is not covered"), the list never does. Verified with `klefki-cli list`.
4. **R/F: no Undo for switches or profile use.** Undo exists only for Move. "Only these servers there" (ticked by
   default) can turn off 11 servers, connectors and plugin servers included, with no Undo in the toast (found in the UX pass before v0.2.0, 2026-10-20).
   Restore from the Activity log is the only way back, and it restores the whole `.claude.json` state. Read.
   Undo is the expected safety net for reversible actions; confirmations are for irreversible ones
   ([NN/g, Confirmation dialogs can prevent user errors, if not overused](https://www.nngroup.com/articles/confirmation-dialog/);
   [Blender HIG, Dialogs](https://developer.blender.org/docs/features/interface/human_interface_guidelines/dialogs/):
   people who see confirmations often click them away, so a dialog should not be the only protection;
   [NN/g, User control and freedom](https://www.nngroup.com/articles/user-control-and-freedom/)).
   Undo commands themselves don't usually prompt, even for undoing a copy
   ([Raymond Chen](https://devblogs.microsoft.com/oldnewthing/20160802-00/?p=94005)): a weaker, anecdotal source.

### Medium
5. **F: every switch is a modal review with a diff.** One server, one project: select, click, read, confirm. Frequent
   reversible actions behind a confirmation teach people to click through it, which empties the review of meaning
   when it does matter (NN/g, above). PLAN.md promises **Space** to toggle; it is not wired (no handler in `shell.rs`).
6. **R: parallel sessions share one set.** All sessions, and all worktrees (L8), of a project read one
   `disabledMcpServers`. Switching for task B changes what task A gets at its next start. Per-session sets (L6) avoid
   this, but Klefki cannot make them yet.
7. **Bug: `klefki-cli use` refuses inside a git worktree** ("Claude Code does not know …/wt as a project: start Claude
   Code there once"), and that advice cannot work (L8). `git_root` stops at the worktree's `.git` file. Verified.
8. **W: no help deciding what to turn off.** Klefki shows no overlap (same tool names, same package or address under
   two names) and no cost. It does know same command/address between plugin and hand servers (`same_as`), nothing else.
   Claude Code itself now nudges people to turn servers off; Klefki should be where they act on that.
9. **F: profile switching is layered, not exclusive.** Using Writing after Code leaves Code "in use"; stopping Writing
   returns to Code's set. Sensible as a stack, but nothing says so, and there is no "switch to" that stops the
   current profile in that project. Verified via `profile-uses.json`.
10. **F: CLI and app differ.** The app ticks "Only" by default, `klefki-cli use` needs `--only`; the CLI has no
    `stop`, no on/off of single servers, and no "in use" (known follow-ups).

### Low
11. Turning on a project server that is not approved says it still won't run, but offers no Approve in the same flow.
12. The legacy `mcpServersDisabled` list and `settings.local.json` approvals are not covered by Restore (known).
13. Turning a connector on where connectors are off (API key, `disableClaudeAiConnectors`) is written but does
    nothing; the review says so only for policy/plugin cases. Read; not checked live.

## Proposed slices, in order
Each is one thread and PR. A slice that changes an invariant says so first.

**S1. Say what the running session gets (findings 1, 3).** After any switch, say plainly: "New sessions in proj load
this. A session already open there keeps its old servers until you restart it (or run `/mcp` → Disable/Enable)."
Replace the "may need a restart" note. In the list, show a user server that is off in every known project as
"off in all N projects, on in new ones". Small, no new IO.

**S2. Undo for switches and profile use (finding 4).** The outcome toast gets Undo for any plan that only flips
switches: the inverse switches, through Prepare → Apply, skipping one changed since (the 3-way check `StopProfile`
already does). The CLI already prints the backup it kept; it needs nothing more here.

**S3. A project lens on the Connections page (findings 2, 5).** A "Where" picker at the top: *All projects* (today's
view) or one project, most recently used first. In one project the list shows what Claude Code loads **there**
(what is off, held by policy, shadowed, not approved), and Turn on / Turn off and **Space** act in that project
only. Switches made in one project skip the diff dialog and show an Undo toast instead (S2); switches across all
projects keep the review.
Approved by the owner (2026-10-22): it relaxes "every write shows a review" for one-project switches
(the plan, docstore, backup and Activity log all stay).

**S4. Profiles as the main way to switch (findings 6, 9, 10).** "Switch to" a profile in a project: stop the profile
in use there and use the new one, as one plan, one Undo. A project's lens (S3) shows which profile is in use there.
`klefki-cli use` gains `stop` and `switch`, and finds the project of a worktree (finding 7, with a reproducer).

**S5. Start a session with a profile (findings 1, 6).** "Copy start command" on a profile: `claude --settings
'{"allowedMcpServers":[…]}'` for that profile, plus `klefki-cli run PROFILE` that starts Claude Code with it. Nothing
is written, nothing goes stale, nothing needs putting back, and parallel sessions can differ. Klefki first checks the
members are on in that project (L7), and that no other settings source has an `allowedMcpServers` list: the flag's
list is added to the user's `settings.json` one (L7) and, as the cross-review found, to the project's
`.claude/settings.local.json` one, and the organisation's policy can narrow it. If one does, Klefki says so.
Connector names under `allowedMcpServers` need checking with a signed-in account first. Approved by the owner
(2026-10-22): a new way to start Claude Code; the binary rules stay.

**S6. Overlap and cost (finding 8), after the tools view (roadmap item 3).** Per project: servers that offer the same
tool names, the same package or the same address under two names; a token estimate per server, and whether tool
search is on for the user's model. Suggest "turn off one of these here", one click, with Undo. Without the tools view,
the same-package/address part can ship first.

**S7. Usage-based suggestions (optional, needs the owner's OK).** Count `mcp__<server>__` tool calls in Claude
Code's session transcripts per project (names only, never content) to say "not used here in 30 days". New read
target; it would need its own decision and invariant line.

Not recommended: writing an MCP key to `settings.json` to make running sessions start servers (L3). It is
undocumented, only works for turning on, and writes a file the user did not ask to change.

## Open questions for the owner
- S3: may a switch in one project apply with Undo instead of a review dialog? **Answered yes** (owner, 2026-10-22).
- S5: may Klefki offer a command (and `klefki-cli run`) that starts Claude Code with a profile's servers only?
  **Answered yes** (owner, 2026-10-22).
- S7: may Klefki read tool names (only) from Claude Code's session transcripts?
- "Turn off for good" through `deniedMcpServers` in user `settings.json` (still open from item 2) would close
  finding 3 for servers someone never wants: still wanted?
