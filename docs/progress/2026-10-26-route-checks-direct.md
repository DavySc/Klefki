# Route checks through the direct client (2026-10-26)

Goal: slice 3 of `docs/proposals/checks-without-claude.md`, the hybrid in the engine: Klefki starts a user or local
stdio server itself first, the way Claude Code would, and asks Claude Code about every other answer. With the
CLAUDE.md invariant line and decision 015. Open items carried over from slice 2: read replies narrowly before running
several checks at once, measure `tools/list` paging, share the `${VAR}` reading with pre-flight.

## Done
- `engine/direct.rs`: the first half of a check. Only for a user or local stdio server whose definition still has the
  fingerprint it was queued with; reads it and the project's trust with one read of `.claude.json`
  (`inventory::launch_entry`, which now keeps `hasTrustDialogAccepted`), builds the launch (`server_env::launch`),
  checks with Claude Code's limit (at most `LONGER_CHECK`). Only Connected is kept; everything else, and every server
  it does not start, goes to Claude Code. A timeout shows "It did not answer within N s. Asking Claude Code…" while
  Claude Code checks it.
- `engine/checks.rs`: `Job::direct` (set when the job is built and the setting is on), `Job::definition`,
  `Job::checking_detail`. A background job that is not Connected goes back in the queue as a Claude Code job (and can
  join a `mcp list` then); one the user asked for goes straight to `mcp get`. Background: three at once, of which at
  most one with Claude Code (`DIRECT_CHECKS`, `Queue::background_claude`). Servers waiting for a direct check are not
  taken into a list. Diagnose, Diagnose in a project, "Check with more time", project servers and non-stdio servers
  stay with Claude Code.
- `Event::Status::by_klefki`; `Command::CheckWithClaudeOnly`; `EngineConfig::direct_checks` (on in `api::start` unless
  the setting says otherwise, off for `klefki-cli` and in every existing test) and `EngineConfig::server_env`
  (Klefki's environment). `api::start` takes `claude_only`.
- `mcp/stdio/message.rs`: replies parsed into only what the check looks at (visitors that skip the rest, short texts
  kept, tools judged one by one). Measured (debug build, process peak, `manytools 19000`, a 1 MB line): 22 MB with one
  tool, 39 MB for the 1 MB line before, 22 MB after.
- The `mcp` and `server_env` modules are no longer `#[cfg(test)]`.
- `server_env::references`, used by `expand` and by pre-flight's unset-variable finding: `${1X}`-style references are
  no longer reported, and names set in the user's or organisation's settings `env` are looked up too
  (`Probe::with_settings`, case-insensitive on Windows).
- App: Status tab row "Checked by" (Klefki / Claude Code) for an answer of this run; Settings: "Check servers with
  Claude Code only" (saved as `checks.claude_only`, read before the engine starts).
- After the cross-review: a switched-off server is never started directly (`build_job` needs `row.enabled`;
  `inventory::launch_entry` reads the run folder's `disabledMcpServers` with the definition, so a server switched off
  after its check was queued is not started either), and background checks leave one worker free for a check the
  user asks for (`Queue::background_limit`, workers minus one: two in the app). Settings has a "Checks" heading and
  says what on and off do; the Status tab says "Klefki (started it as Claude Code would)" with what that means as the
  Detail; with no Claude Code found Klefki starts no server itself (the performance script's `npx` servers would
  have been started); the CLAUDE.md invariant is split into Which / How / What is sent / Fallback lines; decision 015 notes the
  peak memory.
- `fake-mcp-server`: modes `paged`, `pagedfail`, `pagedbadtool`.
- CLAUDE.md invariant line (one line, as the other bullets; it names `server/discover` first, then `initialize` and
  `notifications/initialized`, one `tools/list`, answers to `ping` and `roots/list`); decision 015; a note in 014; the
  proposal's header.

## Measured with Claude Code 2.1.310 (Linux, throwaway `CLAUDE_CONFIG_DIR`)
- `claude mcp get` of `paged`, `pagedfail`, `pagedbadtool`: Connected each time, and the server's log shows one
  `tools/list` with no cursor. Claude Code does not read past the first page; the direct check does the same (test).

## Tests
- `engine/tests/direct.rs` (9): connected by Klefki without Claude Code; exit, bad hello and tools failing are Claude
  Code's answer; a program not found or relative is Claude Code's; the setting at start and changed while running;
  Diagnose, more time and project servers never start the server; a local server gets its project's settings only
  when trusted; the "Asking Claude Code…" detail on a timeout; three background at once with one Claude Code; five
  servers in one folder never become a `mcp list`. Seen failing with `direct::first` forced to ask Claude Code (6 of 9
  fail; the other 3 check what stays with Claude Code).
- `engine/tests/direct.rs` after the review: `a_server_switched_off_where_it_runs_is_never_started_by_klefki` (local
  off in its project, user off in the home entry), `a_server_switched_off_after_the_check_was_queued_is_not_started`
  (`launch_entry` reads it again; off elsewhere only is still on), `a_check_the_user_asks_for_does_not_wait_behind_
  background_checks` (three servers that never answer, `MCP_TIMEOUT` 20 s), `without_claude_code_klefki_starts_no_
  server_itself`. The first, third and fourth seen failing with the fixes taken out (the third took 20 s).
- `mcp/stdio/tests.rs`: `a_message_is_read_as_claude_code_reads_it`, `a_second_page_of_tools_is_not_asked_for_…`;
  existing tests now read through `Message` (same cases).
- `config/preflight/tests.rs`: `a_variable_is_looked_up_as_claude_code_fills_it_in`.
- App: `the_status_tab_says_who_checked_the_server`, `checking_with_claude_code_only_is_told_to_the_engine_and_remembered`.
- Look pictures retaken (intended): `detail-failed-status`, `timeout-explained`, `timeout-needs-more-time` (the new
  "Checked by Claude Code" row) and `page-settings` (the setting); new `detail-checked-by-klefki`.
- Golden files: unchanged.

## Checks
See the pull request for the latest run.

Performance baseline (`scripts/perf-baseline.py --runs 5`, release builds, Linux, median, branch vs main): start-up
within noise (window 238 vs 227 ms small, 244 vs 222 ms large, both with wide ranges; first snapshot 146 vs 150 ms
small), RSS the same (127.6 vs 127.2 MB small). Private dirty memory read 41 vs 54 MB (small) and 45 vs 58 MB (large)
in both branch runs; not explained, so not claimed as a gain. The script runs without Claude Code, so no check runs
in it (now true of the direct check too).

## Open
- The row does not say how long a server that never answers will take in all (Klefki's limit, then Claude Code's):
  it says "Asking Claude Code…" after the first. Left for slice 4, which remembers verdicts and so can skip the
  direct check for a server that timed out before.
- Windows `.cmd`/`.bat` (slice 5), remembering verdicts (slice 4), `klefki-cli` direct checks, a local server's
  `${VAR}` from its trusted project's settings in pre-flight, live comparison on the owner's configuration, macOS and
  Windows.

## Next
Cross-review, then merge after green CI on all three systems.
