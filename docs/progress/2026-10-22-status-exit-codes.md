# `klefki-cli status` exit codes (debugging proposal P9)

Goal: proposal P9 of the 2026-10-21 debugging research: a non-zero exit when a server a script names does not work,
so `klefki-cli status github && …` can be trusted. Also the CLI leftover of P3 that fits the same change: pre-flight
findings in `status`.

## Before
`status` already exited 1 for a checked server that was not connected and 3 when Claude Code could not be used. But
a **named** server that was not checked (off, rejected for its project, not loaded, a plugin's, the organisation's,
a claude.ai connector) left the run at 0: `klefki-cli status delta` with `delta` off said "Off" and passed.

## Done
- Exit codes (`crates/cli/src/main.rs`, `render::Outcome::will_not_run` / `not_checkable`):
  - Named and off, rejected or not loaded: **1**. Claude Code does not run it, so it does not work there.
  - Named and one Klefki does not check (plugin, organisation, claude.ai): new code **5**, "nothing is known".
  - Order when several apply: 3 (every check broke down), then 1, then 5, then 0.
  - Without names nothing changes: only what was checked counts; a server switched off is not a failure.
- Pre-flight findings (P3's `ServerRow::preflight`, filled by the engine's probed reading) under a `status` row that
  is not connected, after the hint: `<name>: <problem>` then `- <fix>`. JSON: `"preflight": [{"kind", "problem",
  "fix"}]` on every `status` row (additive; schema stays 1). As in the app (`preflight_shown`), "not found" is left
  out once the server started.
- Help text, `docs/decisions/006-cli-output.md` (exit table, P9 paragraph), golden files `help.txt`, `status.txt`,
  `status-json.txt` (the unnamed project server `docs` now shows that `node` is not on the test's PATH).

## Tests
- Seen failing first (5 of 47 in `crates/cli/tests/cli.rs`), then passing:
  `a_named_server_that_is_off_is_not_checked_and_fails_the_run` (also with a connected server beside it),
  `a_rejected_project_server_is_not_checked_even_when_named`, `a_named_server_klefki_does_not_check_has_its_own_exit_code`
  (organisation server 5, with a connected one 5, a not-loaded user server 1, text and JSON),
  `status_says_what_in_a_failing_definition_keeps_it_from_starting` (unset `${VAR}` in a header, text and JSON),
  `a_server_that_connects_shows_no_pre_flight_finding`. Variant that passes before and after:
  `an_unnamed_server_that_is_off_does_not_fail_the_run`. Unit test:
  `a_server_left_unchecked_is_either_not_run_or_not_checkable_never_both`.
- **Changed expectations, for review:** three existing assertions expected exit 0 for a named server Claude Code does
  not run (`a_named_server_that_is_off…` text and JSON, `a_rejected_project_server…`, and the terminal test's
  `status delta`). P9 changes exactly that behaviour, so they now expect 1. Nothing else was loosened.

## Review (#100)
- B1: `status beta` with the organisation's `beta` and the user's replaced one exited 1 (the user's row is not
  loaded), though Claude Code runs the organisation's `beta`. A not-loaded row now does not fail the run when the
  organisation's row of that name is among those named (`main.rs::replaced_by_organisation`): exit 5. Test changed to
  expect 5 (seen failing first); variant `a_named_server_the_organisation_does_not_allow_fails_the_run` (allowed list
  leaves `beta` out, nothing loads in its place: 1).
- Folded in: the summary counts `off`, `rejected` and `not loaded` apart from `not checked`, after what the checks
  found (golden `status.txt`: `… 1 needs auth, 1 off, 1 not checked.`); help and decision 006 say `&&` and `set -e`
  treat 5 as a failure; a unit test pins the five pre-flight kind names.

## Not in this slice (CLI leftovers from other progress files)
- `list` has no pre-flight notes: `api::load_snapshot` reads without looking on the PATH, and `list` is the fast one.
- No timing / `MCP_TIMEOUT` retry in the CLI (P2), no last starts (P1), no `klefki-cli inspector NAME` (P6), no
  debug report (P5), no `status --project DIR` (P4). Each is its own command or option, best one per slice;
  `inspector` is the smallest (core makes the line already).

## Checks
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`: exit 0
  (app 514, core 1094, cli 28 + 48 after the review fixes).
- `cargo deny check`: fails only on the yanked tokio 1.54.0 already in main's lock (its fix is #99, in flight).
- Start-up, RAM: `status` reads nothing new (the findings are in the engine's snapshot already); `list` unchanged.

## Next
Re-review, merge. Release notes line: "`klefki-cli status NAME` now fails (exit 1) for a named server Claude Code
does not run, exits 5 for one Klefki does not check, and says what in a failing definition keeps it from starting."
