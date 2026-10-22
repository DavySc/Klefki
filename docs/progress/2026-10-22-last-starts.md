# Last starts by Claude Code (debugging proposal P1)

Goal: proposal P1 of the 2026-10-21 debugging research, roadmap item 8 reframed (the owner approved reading Claude
Code's log folder on 2026-10-22). On the Status tab, say how Claude Code's own last starts of a server went, in which
folder, and what the server printed, from Claude Code's MCP logs: "it failed in my session" answered without starting
anything.

## What Claude Code does
Measured with Claude Code 2.1.308 on Linux with a throwaway `HOME`; recorded in decision 013 (folder layout, the
name rule with its hash for names over 200 characters, the lines a start writes). Real logs kept as fixtures in
`fixtures/mcp-logs/`.

## Done
- `crates/core/src/last_start.rs` (tests in `last_start/tests.rs`): `folder_name` (Claude Code's rule, checked
  against its own function run in Node, long-name hash included), `root` (the cache folder per system), `read(Ask)`:
  the newest start in each folder Claude Code was started in, newest first, at most 3. Bounded: names listed (at most
  5,000 folders, 20,000 files a folder), at most 8 files read and of each only the first and last 64 KB (`ends`).
  Links, folders, pipes and odd names skipped; lines cut by where reading stopped, non-JSON and non-text skipped.
  Only the start, its outcome and the server's stderr are used; tool calls and headers never. Text sanitised, the
  server's own env and header values hidden, then Diagnose's redaction. Hints as Diagnose's, for failures only.
  A local or project server counts starts in its project and below (by the logged `cwd`); a user server leaves out
  projects where another server of its name loads.
- `api`: `LastStarts`, `LastStart`, `StartOutcome`; `Command::LastStarts(ServerId)`, `Event::LastStarts`;
  `EngineConfig::claude_logs`. The engine finds the name and project as the files spell them, the server's values,
  other servers whose names land in the same log folder, and says when the logs are another server's.
- App (`shell/last_starts.rs`): "Last starts by Claude Code" on the Status tab, under the status and its
  explanation: a line per start ("Today 07:32 in ~/proj: failed (CONNECTION_CLOSED)") that wraps, the failed one
  open with Claude Code's message, hints, what the server said, "Copy last starts". Asked for when the tab is shown,
  again after a check of the server ends or the files change, keeping what was shown meanwhile; nothing is drawn
  before the first answer.
- CLAUDE.md: one invariant line for the new read location (flagged for review). Decision 013. PLAN item 8 marked.
- New look picture `last-starts.png`; no other picture or golden file changed.
- Seven app tests that counted the commands sent after opening the Status tab now also expect the one
  `Command::LastStarts` for that server (one filters it out before checking `Inspect`); none was loosened otherwise.

## Checks
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`: exit 0
  (app 474, core 1042, cli 26 + 43). `cargo deny check`: only the yanked tokio 1.54.0 already in main's lock.
- New tests seen failing: with `read` returning nothing, 14 of the 20 reader tests fail.
- By hand: the debug app under Xvfb with a scratch `HOME` holding real 2.1.308 logs (a server that exits, one that
  connects, a timeout, a missing program, a refused HTTP server): the failed server's Status tab showed Klefki's own
  check from the home folder and the earlier start in the project folder, with the server's stderr (`token=***`) and
  the "file or module is missing" hint.
- A 1 GB (sparse) log is read in two 64 KB pieces (counted) and answered in well under 2 s.

## Not in this slice
- Telling Klefki's own checks from a session's start: both are starts; the hover says Klefki checks user servers from
  the home folder.
- macOS and Windows not tried live (folder from `env-paths` and decision 003).
- `klefki-cli`: no last starts there (its golden files would change); fits P9.

## Next
Pull request, mutants on `last_start.rs` (quality workflow), cross-review, merge; then P6 (Copy MCP Inspector command).
