# Diagnose in a project (debugging proposal P4)

Goal: proposal P4 of the 2026-10-21 debugging research. Claude Code starts a user server in the folder each session
starts in, while Klefki checks it from the home folder, so a server that names `./server.js`, reads a `.env` file or
runs `npx` against a project's own packages can pass Klefki's check and fail in a session. Let the user run it from a
project's folder on request, and see what a session there sees.

## What Claude Code does
Measured with Claude Code 2.1.308 on Linux, a throwaway `HOME` and `CLAUDE_CONFIG_DIR`, a test server that prints its
folder and environment:
- `claude mcp get <name>` run in a project's folder starts a user server there: `node ./rel.js` connected from the
  project that has the file and failed (`CONNECTION_CLOSED`, `Cannot find module`) from the home folder.
- Off in that project (`disabledMcpServers`): not started; it answers "Disabled for this project".
- A local server of the same name there: `mcp get` answers for that one instead.
- The project's `.claude/settings.json` and `settings.local.json` `env` did not reach the server, trusted or not.
  So the folder is the difference, not per-project settings. Not measured: an interactive session.

## Done
- `api`: `Command::DiagnoseIn { id, project }` (a project as `Snapshot::projects` lists it), and
  `Diagnosis::folder`: where it ran, `None` for Klefki's usual place.
- Engine (`command_loop.rs::diagnose_in`): refused with a "not diagnosed: …" note, nothing run, for a server that is
  not a user one, one that is not stdio (the folder plays no part), a project Claude Code does not know, one where it
  is off, where another server of its name loads, where policy holds it back, or whose folder is gone; also every
  refusal of a usual check (not loaded, unsafe name). Otherwise Diagnose's own run (`claude --debug mcp --debug-file
  … mcp get`), from the folder as `.claude.json` spells it (`swap::spelled_in`). It does not say "Checking…" on the
  row and does not change its status: that stays the home folder's. A server's check waiting in the queue is not
  dropped for it (`Queue::push_now`). Time limit, redaction and hints are Diagnose's.
- `inventory::shown_project`: how rows name a project, used by both.
- App (`shell/diagnose_in.rs`): "Diagnose in a project" on the Status tab of a user stdio server, between the
  explanation and the last starts, once Claude Code knows a project: a short why, a "Project" drop-down of the
  projects where Claude Code starts it (by folder name, the Profiles page's list, with its find box from 10
  projects; `profiles::project_choices` is that list, shared), the first chosen until another is, and "Diagnose
  there". While it runs: the button is off and "Diagnosing in <project>…" is said (announced politely). The result
  is the usual diagnosis panel, headed "Diagnosed in <project>: <status>" with Claude Code's detail. "Not listed: N
  projects where it is off or another server called X loads." when some are left out; "Claude Code knows no project
  where it starts this server." when all are.
- The diagnosis's copy and the debug report say "diagnosed in: <project>".
- The General tab's relative-path note points at it; the last starts' line about Klefki's own checks says they are
  in `~` "unless diagnosed in a project".
- No invariant change: the same `claude` run as Diagnose, on request, in a folder Klefki already runs local servers'
  checks in, with the user's own definition.

## Checks
- Engine tests `engine/tests/diagnose_in.rs` (3) failed before (no answer: timed out), pass now;
  `checks::a_diagnose_in_a_project_keeps_the_servers_waiting_check` failed with the old `push_now`.
- App tests `shell/tests/diagnose_in.rs` (5): four failed with the section not drawn.
- Look: new `diagnose-in-project.png`, `diagnosed-in-project.png`; `last-starts.png` and `debug-report-copied.png`
  changed for the reworded line about Klefki's own checks only.
- By hand against real Claude Code 2.1.308 (a scratch program on `start_headless` with the throwaway `HOME`):
  `rel` diagnosed in the project connected, from home failed with "A file or module is missing"; `off` in the project
  where it is off and `abs` where a local one loads were refused with their reasons; `abs` elsewhere connected.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`: exit 0. `cargo test --workspace`:
  exit 0 (app 509, core 1093, cli 26 + 43).
- `cargo deny check`: advisories FAILED on `tokio 1.54.0` in Cargo.lock (yanked, and now RUSTSEC-2026-0384), the
  same on main; bans, licenses, sources ok.
- Not run: cargo-mutants (not installed here; the quality workflow runs it).

## Not done
- `klefki-cli`: no diagnose in a project (fits P9).
- A palette entry.
- Windows and macOS not tried.

## Next
Pull request, cross-review, merge; then P9 (klefki-cli status exit codes).
