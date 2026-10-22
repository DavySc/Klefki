# Copy debug report (debugging proposal P5)

Goal: proposal P5 of the 2026-10-21 debugging research: one button that copies what a person needs to put in a bug
report or show to whoever helps: versions, status, the definition with secrets replaced, what the server said, how
Claude Code's own starts went. Plus two leftovers of the last-starts slice (P1).

## Done
- `config/entry.rs`: `values_filled_in(entry, vars)`: the values to hide in text read elsewhere now include what each
  `${NAME}` / `${NAME:-default}` stands for (Claude Code fills them in from its environment, so a server prints the
  filled-in value, not the reference): the filled-in env and header values, every referenced variable's value, and
  defaults. Klefki's own environment stands in for Claude Code's. Used by the last starts (P1 leftover: values were hidden
  only as written). Engine test `a_value_filled_in_for_a_reference_is_hidden_in_what_the_server_said` failed before.
- `api`: `Command::RedactedDefinition(ServerId)`, `Event::RedactedDefinition { id, result }`: Export's redaction of
  one server (values that may be secrets become `${VAR}` references, the notes say which). Reads the files; writes and
  starts nothing. No new invariant: same read as Export and the Inspector command.
- App (`shell/debug_report.rs`): a "Debug report" section at the end of every Status tab (also for plugin,
  organisation and claude.ai servers, whose report says why there is no definition) with "Copy debug report", "Copied"
  for as long as the other Copy buttons say it (announced politely), and a palette entry "Copy debug report for
  <name>". The report: Klefki and Claude Code versions and the system; server, transport, status (last known when it
  is); the status explanation; what keeps it from starting; the definition and what each `${VAR}` stands for; the
  diagnosis if one was run; the last starts. Built in the app from what the Status tab already holds, so nothing is
  read twice; a line that arrives after the user left the tab or server is not copied.
- P1 leftover: Diagnose's "Copy report" now says "Copied", and its hover no longer says "Secrets are already removed":
  known secrets are hidden, check before sharing; it points at the debug report for the rest.
- `diagnosis_details` and `starts_text` are the old report code, shared (text unchanged).
- Look: new `debug-report-copied.png`; `detail-*-status.png` and `inspector-command.png` changed only because the new
  section follows the Inspector section (a scroll bar / the next heading).

## Not done
- `REPLACE_ME_<NAME>` placeholders (P6 reviewer's idea): the report is read by people, and `${VAR}` references plus the
  list of what each stands for say the same; not taken.
- The operating-system version (only the system and architecture): reading it needs IO in the app or a new engine call.
- `klefki-cli` has no report (fits P9).

## Next
Pull request, cross-review, merge; then P4 (check from a project folder).

## Checks
- `cargo fmt --check`: exit 0. `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
- `cargo test --workspace`: exit 0 (app 499, core 1081, cli 26 + 43). New: 3 entry, 1 last-starts and 2 definition
  engine tests, 8 app tests, 1 look test.
- `cargo deny check`: advisories FAILED on the yanked `tokio 1.54.0` in Cargo.lock, the same on main; bans, licenses,
  sources ok.
- Seen failing first: the filled-in value test (the value showed in the log text before the change).
- Not run: macOS and Windows; cargo-mutants (not installed in the cloud session; the quality workflow runs it).
