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
- `mcp/stdio/tests.rs`: `a_message_is_read_as_claude_code_reads_it`, `a_second_page_of_tools_is_not_asked_for_…`;
  existing tests now read through `Message` (same cases).
- `config/preflight/tests.rs`: `a_variable_is_looked_up_as_claude_code_fills_it_in`.
- App: `the_status_tab_says_who_checked_the_server`, `checking_with_claude_code_only_is_told_to_the_engine_and_remembered`.
- Look pictures retaken (intended): `detail-failed-status`, `timeout-explained`, `timeout-needs-more-time` (the new
  "Checked by Claude Code" row) and `page-settings` (the setting); new `detail-checked-by-klefki`.
- Golden files: unchanged.

## Checks
See the pull request for the latest run.

## Open
- Windows `.cmd`/`.bat` (slice 5), remembering verdicts (slice 4), `klefki-cli` direct checks, a local server's
  `${VAR}` from its trusted project's settings in pre-flight, live comparison on the owner's configuration, macOS and
  Windows.

## Next
Cross-review, then merge after green CI on all three systems.
