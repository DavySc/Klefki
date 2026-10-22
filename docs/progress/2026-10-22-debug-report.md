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

## Review fixes (cross-review of #97)
- B1: one scrub for everything the report holds. `diagnose/redact.rs::scrubbed` (the last starts' own scrub, moved)
  hides the server's values, then the pattern rules; `config/entry.rs::hidden_values` is the list: environment and
  header values as written and filled in, the arguments and address parts an export replaces (taken from
  `rewritten_args` / `rewritten_url`). Used for the last starts, for Diagnose (its status detail, the server's stderr
  and the debug text, before hints and the report are made) and, through `Command::RedactedDefinition { also }`, for the
  status line in the report. The pattern rules now also cover `user:password@` for any scheme (also an empty user),
  a secret parameter in any address's query (`?key=`, `access_token=`), and "API key <value>" in words. Reproducers:
  one test per leak in `redact.rs`, `diagnose_hides_the_servers_own_values_and_the_parts_an_export_replaces`
  (failed before), and the `also` test; each with a nearby variant that must stay readable.
- B2: the report is made only when the definition and a fresh read of the last starts have both arrived (the starts
  are asked for first); a test from the palette on the General tab, with the definition answering first.
- S1: "diagnosis: not run (Diagnose on the Status tab runs it)" and "last starts by Claude Code: not read".
- S2: "Show what was copied", collapsed, under the button after a copy: the same text, selectable.
- S3: a default or a referenced variable's value is hidden only when it is not plainly a setting (`localhost`,
  `fast`, a folder such as `${HOME}`): tests for both sides. A host name with a dot counts as not plain and is hidden.
- S4: the section's text says "where Klefki can read it".
- Nit not taken: `~` for the home folder in the report's paths (the starts' folders already have it).
- Known gap: a hint's `points_at` excerpt of the status text is not scrubbed (it holds only the part of the
  server's own address the hint is about).

## Checks
- `cargo fmt --check`: exit 0. `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
- `cargo test --workspace`: exit 0 (app 499, core 1081, cli 26 + 43). New: 3 entry, 1 last-starts and 2 definition
  engine tests, 8 app tests, 1 look test.
- `cargo deny check`: advisories FAILED on the yanked `tokio 1.54.0` in Cargo.lock, the same on main; bans, licenses,
  sources ok.
- Seen failing first: the filled-in value test (the value showed in the log text before the change).
- Not run: macOS and Windows; cargo-mutants (not installed in the cloud session; the quality workflow runs it).
- After the review fixes: fmt, clippy `-D warnings` and `cargo test --workspace` exit 0 (app 503, core 1089, cli 26 + 43);
  `cargo deny` as above (only the yanked tokio).
