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

## Review fixes (cross-review of #95)
- B1: a header's last word (`Bearer <token>`, `Basic <credentials>`) is hidden on its own too, when at least 6
  characters; the longest values go first. Test `a_header_token_is_hidden_without_its_scheme_too` (Bearer, Basic
  variant, a short last word kept).
- B2: a start that connected keeps only what the server printed before the connect line: what it prints while serving
  tool calls is not shown or copied. Failures keep their stderr. Test with the chatter after the connect, and in the
  tail of a log over 128 KB.
- B3: the project's folder-name prefix is compared in any ASCII case on Windows (`starts_like`, testable on every
  system), as the `cwd` check already was.
- Optional hardening taken: on Unix the log is opened with `O_NOFOLLOW | O_NONBLOCK` (no separate test: the
  check before it catches the same files in the tests).
- UX: the caveat about Klefki's own checks is a muted line under the heading, not a hover; Copy says "Copied" for a
  few seconds (announced to screen readers) and its hover says known secrets are hidden, check before sharing; the
  section reads again whenever it is shown again (tab or server chosen); the empty state no longer promises 30 days.
  `last-starts.png` redrawn for the caveat line; no other picture changed.

## Checks
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`: exit 0
  (app 474, core 1042, cli 26 + 43); after the review fixes exit 0 (app 476, core 1046, cli 26 + 43).
- Review fixes: the three reproducers and the re-read test failed before their fixes.
- Mutants: the quality run on 98fff6c was cut off by the runner after 6 misses, all in the app section (`said`, the
  header toggle, the exact-time hover). Locally `cargo mutants` on `last_start.rs` and `shell/last_starts.rs`
  (tests filtered to the last-start ones) missed 17 more: date checks, the read-whole boundary, the folder-name
  prefix with no `cwd`, the Copied timing. Tests added for all; a re-run of those leaves one missed, `|` to `^`
  in the open flags (the same value: the bits do not overlap). Two `ends` mutants time out on the 1 GB test, and
  `base36`'s `/=` hangs its test (counted as missed by the filtered run, killed by the test timeout).
- After them: fmt, clippy, `cargo test --workspace` exit 0 (app 478, core 1048, cli 26 + 43). `cargo deny check`: only the yanked tokio 1.54.0 already in main's lock.
- New tests seen failing: with `read` returning nothing, 14 of the 20 reader tests fail.
- By hand: the debug app under Xvfb with a scratch `HOME` holding real 2.1.308 logs (a server that exits, one that
  connects, a timeout, a missing program, a refused HTTP server): the failed server's Status tab showed Klefki's own
  check from the home folder and the earlier start in the project folder, with the server's stderr (`token=***`) and
  the "file or module is missing" hint.
- A 1 GB (sparse) log is read in two 64 KB pieces (counted) and answered in well under 2 s.

## Not in this slice
- Telling Klefki's own checks from a session's start: both are starts; a line under the heading says Klefki checks
  user servers from the home folder.
- Diagnose's "Copy report" hover makes the same too-strong promise and gives no feedback (review UX 2): follow-up.
- `${VAR}` values in env or headers are hidden as written, not as Claude Code expands them (review note): follow-up.
- macOS and Windows not tried live (folder from `env-paths` and decision 003).
- `klefki-cli`: no last starts there (its golden files would change); fits P9.

## Next
Pull request, mutants on `last_start.rs` (quality workflow), cross-review, merge; then P6 (Copy MCP Inspector command).
