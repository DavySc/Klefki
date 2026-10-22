# Timeout explainer (debugging proposal P2)

Goal: proposal P2 of the 2026-10-21 debugging research. When a check runs out of time, say how long it ran, how long
Claude Code waits and where that limit is set, and offer "Check again with 2 minutes" to tell a server that is slow
to start (a first npx or uvx download, a container) from one that never answers; once it connected with more time,
say how to give it that time in Claude Code. Does not read Claude Code's log folder (that is P1).

## What Claude Code does (measured with Claude Code 2.1.308, Linux, throwaway `CLAUDE_CONFIG_DIR`)
- `mcp get` of a server that answers after 40 s: `Issue: Request timed out` after 27.7 s with nothing set.
  With `MCP_TIMEOUT=120000` it says Connected after 40.6 s.
- `MCP_TIMEOUT` is read like JavaScript's `parseInt`: `3000x` is 3 s, `2.5e3` 2 ms, ` 2000` 2 s; `abc`, `0`, `-5`
  and an empty value leave the default.
- `env.MCP_TIMEOUT` in the user's `settings.json` wins over the environment (both ways round), and a project's
  `.claude/settings.local.json` over the user's file. A file that sets it to `null`, `true`, `{}` or `"abc"` still
  decides: 30 s, over `MCP_TIMEOUT=2000` in the environment.
- `claude --settings '{"env":{"MCP_TIMEOUT":"8000"}}' mcp get …` wins over the user's file (2000 there).
  The organisation's settings are documented to win over it; not measured.

## Done
- `config/timeout.rs` (tests in `timeout/tests.rs`): `limit(paths, folder, vars)` gives the limit for a Claude Code
  started in a folder, where it is set (`LimitSetIn`: default, environment, a file, the organisation's file),
  whether the value there is unreadable, and which file to change it in. Order: organisation, the folder's
  `.claude/settings.local.json`, its `.claude/settings.json`, the user's `settings.json`, then the environment. The
  files are size-limited and only `env.MCP_TIMEOUT` is read. `policy::home_as_tilde` split out of `policy::shown`.
- `api`: `TimeLimit`, `LimitSetIn`, `Timing { took, limit, given, timed_out }`, `LONGER_CHECK` (2 minutes),
  `Command::CheckLonger(ServerId)`, `Event::Status::timing`, `words::duration` (`60 ms`, `2.5 s`, `41 s`,
  `2 minutes`).
- Engine: each `mcp get` check and Diagnose is timed. A timing goes with an answer that ran out of time (Claude
  Code's "timed out", or Klefki's own wait) and with every check given more time. `CheckLonger` runs
  `claude --settings {"env":{"MCP_TIMEOUT":"120000"}} mcp get <name>` (fixed text in argv, no secrets) and says
  "giving it up to 2 minutes to connect" while it runs. Klefki's own wait is longer by as much as the limit is above
  30 s, so a server the user gave 45 s is answered for by Claude Code and not cut off at 30 s (before: Unknown,
  "check timed out after 30.0s"). With nothing set, the wait is as before.
- App (`explain/timing.rs`, Status tab): the explanation of a timeout gets "It gave up after 28 s. Claude Code
  waits 30 s for a server to connect (where)", a line on first npx/uvx runs for stdio servers, and the button
  "Check again with 2 minutes" (not when the organisation sets the limit or it is already 2 minutes or more).
  Given more time: connected after more than the limit is a warning with the value to set (half as long again, in
  whole 10 s) and the file to set it in; connected within the limit is a muted note; still silent is "probably stuck
  rather than slow". The server list is unchanged.
- New look pictures `timeout-explained.png`, `timeout-needs-more-time.png`; no other picture or golden file changed.
- Fixture `fixtures/mcp-get/timed-out-stdio.txt`: real 2.1.308 output.

## Checks
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`: exit 0
  (app 465, core 1019, cli 26 + 43). `cargo deny check`: only the yanked tokio 1.54.0 already in main's lock.
- New tests seen failing: the engine's timing tests fail (5 of 7) with the timing turned off and the wait not
  lengthened.
- By hand with real Claude Code 2.1.308 through `api::start_headless` (scratch test, not kept), server answering after
  40 s: Check gives Failed "Request timed out" with took 27.6 s, limit 30 s default; CheckLonger gives Connected,
  took 40.6 s. With the user's file at 2000, CheckLonger of a 5 s server connects (the settings flag wins). With the
  user's file at 45000, a plain Check of the 40 s server says Connected (Klefki waited long enough).

## Not in this slice
- `klefki-cli`: no timing in its output (its golden files would change); fits P9.
- Writing `MCP_TIMEOUT` into a settings file for the user: a new write target, owner's call.

## Next
Pull request, cross-review, merge; then P1 (last real start, reading Claude Code's log folder, approved 2026-10-22).
