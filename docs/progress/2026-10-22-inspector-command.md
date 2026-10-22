# Copy MCP Inspector command (debugging proposal P6, roadmap item 10)

Goal: a button that copies the one line which opens a server in the MCP Inspector, so what Klefki does not do (call
tools, read replies) is one paste away. Never run by Klefki; no secret in the line. Decision 014.

## Done
- Tried the real Inspector 2.12.0 (packed from npm into a throwaway folder, `MCP_AUTO_OPEN_ENABLED=false`): the line
  forms below were started and `/api/config` read back: command, args (including `--foo` after `--`), `--cwd`, `-e`
  and, for `--transport sse --server-url`, the address were all filled in. Findings in decision 014.
- `crates/core/src/inspector.rs` (tests in `inspector/tests.rs`): `command(definition, project, windows)` builds the
  line and notes from a definition. Environment and header values never; names listed. Secret-looking arguments and
  address parts become `REPLACE_ME` (reusing Export's `rewritten_args` and `rewritten_url`). `${NAME}` kept, pointed
  out. `--cwd` for a local or project server's folder when it is a plain local absolute path. POSIX single quotes;
  Windows double quotes valid for PowerShell and cmd, with the words that differ refused. Control and bidi characters
  refused.
- `api`: `Command::InspectorCommand(ServerId)`, `Event::InspectorCommand { id, line }`, `InspectorCommand`. The
  engine reads the definition through Export's `sources` (one file read), writes nothing.
- App (`shell/inspector.rs`): the "MCP Inspector" section on the Status tab under the last starts (a short
  explanation, "Copy Inspector command", then the line and notes after the click), "Copied" for as long as the other
  Copy buttons say it, announced politely; a palette entry "Copy Inspector command for <name>". Offered for user,
  local and project servers of type stdio, http or sse (also for one Claude Code does not load); not for plugin,
  organisation or claude.ai servers or WebSocket. Forgotten when the configuration is read again.
- Look: new `inspector-command.png`; `detail-diagnosis.png`, `detail-failed-status.png` and
  `detail-issue-tracker-status.png` changed only because the new section sits at the end of the Status tab (the
  separator and heading now show in the window).
- PLAN item 10 marked done.

## Review fixes (cross-review of #96)
- B1: the address is parsed only to check it, and written as spelled (`${TENANT}` in the path and `${HOST}` were mangled
  and their note lost); tests for the path, the host and a query value.
- B2: every `${NAME}` (with or without a default) is written as `REPLACE_ME` on every system, with a note naming the
  variables; Windows no longer refuses the line for one.
- B3: the Inspector section is drawn after the diagnosis in the Status tab; test; `detail-diagnosis.png` redrawn.
- B4: the palette hint says it copies the line.
- Polish: a word that starts with `=` is quoted (zsh); what to do (`to_do`, normal text, action first: replace,
  enter, add) is apart from what is only good to know (`notes`, muted); the user name and password in an address are
  named as such, and an address in an argument keeps its host (`keep_address`); a line that arrives after the user
  left the Status tab or server is not copied; a comment on PowerShell 7.3's trailing backslash.
- The reviewer read `npx.ps1` (npm 10.9.4 and 12.2.1): the `--` reaches the Inspector under PowerShell; read, not run.

## Checks
- `cargo fmt --check`: exit 0. `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
- `cargo test --workspace`: exit 0 (app 488, core 1068, cli 26 + 43). New: 18 core + 2 engine + 9 app tests.
- `cargo deny check`: advisories FAILED on the yanked `tokio 1.54.0` in Cargo.lock, the same without this branch
  (the lock-file slice offered to the owner); bans, licenses, sources ok.
- Mutation check by hand (cargo-mutants is not installed in the cloud session): removing the single-quote escape and
  returning no header or env names each failed tests. The quality workflow runs the real thing.
- The tests were written with the code and passed at once, so none was seen failing before; the hand mutations above
  stand in for that.

## Not verified
- The Windows line (PowerShell 5, PowerShell 7, cmd): quoting is unit-tested; the line itself was not run there. The
  `--` after the Inspector's flags passes through `npx.ps1` on Linux-like parsing only.
- macOS and Windows look and the real Inspector there.

## Next
- `klefki-cli inspector <name>` with P9. Then P5 (copy debug report), P4.
