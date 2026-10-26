# 015: Klefki checks stdio servers itself first (2026-10-26)

## Context
Every status check started Claude Code (`claude mcp get`, or one `claude mcp list` for four or more servers in a
folder): about 0.45 s and 196 MB per run on Linux with Claude Code 2.1.310, which is why background checks ran one at
a time. The proposal (`docs/proposals/checks-without-claude.md`, merged in #107) measured what Claude Code does when it
starts a stdio server, and a spike compared both verdicts on 25 test servers. The owner chose the hybrid on
2026-10-26 and approved a new invariant line the same day. Slice 2 (#109) built the client (`mcp/stdio.rs`) and the
environment and program builder (`config/server_env.rs`); this slice routes the app's checks through them.

## Decision
- **Who checks what.** A check of an enabled, loaded user or local stdio server, in the app, starts the server the way
  Claude Code would (`engine/direct.rs`): the definition the inventory resolved (its fingerprint must still match),
  the folder Claude Code uses (home for a user server, the project for a local one), Claude Code's environment (the
  project's own settings only where its trust dialog was accepted), the program found on the server's PATH and
  started by full path. Only "Connected" is taken from it.
- **Everything else is Claude Code's.** A server that fails, fails its tools, times out, cannot be started (program
  not found, no `#!` line, a `.cmd` or `.bat` on Windows) or changed since the check was queued is checked again with
  `claude mcp get` (batched with `mcp list` in the background, as before), and that answer is shown. HTTP, SSE and
  WebSocket servers, project (`.mcp.json`) servers, Diagnose, Diagnose in a project, "Check again with more time" and
  sign-in stay with Claude Code. The command-line tool (`klefki-cli`) stays with Claude Code too.
- **A server that never answers** waits Claude Code's limit twice (once for each check). After the first, its row
  says "It did not answer within 30 s. Asking Claude Code…" while Claude Code checks it.
- **How many at once.** Background checks run up to three at once while Klefki starts the servers itself (one fewer
  than the workers, so a check the user asks for never waits behind them: two in the app), and still at most one with
  Claude Code. A server can be heavy itself (a container, a JVM), so it stays a few. Peak memory while the app checks
  in the background is now those servers plus at most one `claude` (about 196 MB), where it was one `claude` and the
  server it started; a check the user asks for can add one more of either.
- **Only a server that is on.** A check of a switched-off server ("Check now" on it) stays with Claude Code, which
  answers "Disabled" without starting it; and the direct check reads the run folder's `disabledMcpServers` again when
  it starts, so a server switched off after its check was queued is not started either. With no Claude Code found,
  no check starts a server, as before.
- **Who answered** is on the Status tab: "Checked by: Klefki (started it as Claude Code would)", with what that means as its Detail, or "Claude Code".
- **A setting, "Check servers with Claude Code only"**, on the Settings page, off by default, kept with the app's
  other settings, sent to the engine at start and when changed (`Command::CheckWithClaudeOnly`).
- **Replies are read narrowly** (`mcp/stdio/message.rs`): only the fields the check looks at are kept, long texts are
  not, and tools are judged one at a time. A 1 MB `tools/list` line of 19,000 small tools took the check from 22 MB
  to 39 MB as a `serde_json::Value` (debug build, the process's peak); read narrowly it stays at 22 MB, the same as a
  server with one tool.
- **Pre-flight's `${VAR}` warnings** use the same reading of `${NAME}` as the check (`server_env::references`): a
  reference Claude Code never fills in (`${1X}`, `${HO-ME}`) is not reported as unset, and a name set in the user's or
  the organisation's settings `env` is not either.

## Measured (Claude Code 2.1.310, Linux)
- `claude mcp get` sends one `tools/list` and never follows `nextCursor`: a server whose second page fails, or holds a
  tool Claude Code would refuse, is Connected. The direct check sends one too (`fake-mcp-server` modes `paged`,
  `pagedfail`, `pagedbadtool`).
- The rest is in the proposal and in `docs/progress/2026-10-26-direct-check-client.md`.

## Safety
Unchanged: which servers are started and when (the same gate as before: user and local rows that are enabled, loaded
and not blocked; project servers only through Claude Code, on request). New: Klefki builds the command and the
environment itself. See the CLAUDE.md invariant line and the proposal's safety section. A request from the server
with an id that is not a number or a text gets no answer (JSON-RPC allows only those).

## Not done
- Windows `.cmd` and `.bat` commands (`npx`, `pnpm`) are still checked by Claude Code; slice 5 after a Windows
  measurement.
- Remembering verdicts across restarts (slice 4).
- A local server's `${VAR}` filled in from its trusted project's settings is still reported unset by pre-flight.
- `klefki-cli` checks with Claude Code only.
- The owner's own configuration, macOS and Windows were not compared live.
