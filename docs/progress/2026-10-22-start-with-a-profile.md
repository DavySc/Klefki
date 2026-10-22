# Start Claude Code with a profile

Goal (slice S5 of the on/off plan, `docs/proposals/on-off-audit.md`, findings 1 and 6; approved by the owner): start a
session with only a profile's servers, writing nothing, so nothing goes stale, nothing needs putting back and sessions
side by side can differ: "Copy start command" on a profile card and `klefki-cli run PROFILE`. The audit's review found
that the `--settings` list is added to the lists of every settings source, so the result must be checked against all
of them and said plainly when it would still differ from the profile.

## Checked live (Claude Code 2.1.308, Linux)
Temp `HOME` and `CLAUDE_CONFIG_DIR`, a git project with user servers a, b, c, d (d off in the project) and a local
server loc, each a tiny stdio server that logs its start; `claude --settings '…' mcp list` in the project, and
`/etc/claude-code` for the organisation's files (removed after each run).
- Only `{"allowedMcpServers":[{"serverName":"a"},{"serverName":"d"}]}`: a loads, d stays "Disabled for this project".
- A list in the project's `.claude/settings.json`, in `.claude/settings.local.json`, in the user's `settings.json`
  or in `managed-settings.json` is added to the flag's: its server loads too. An empty user list adds nothing.
- A `serverCommand` rule in the user's list: the flag's name a no longer counts (only b, by command, loads). The same
  in the flag itself.
- A denied rule (user's or organisation's) wins over the flag.
- `allowManagedMcpServersOnly` with or without a managed list: the flag's list is ignored (with none, everything loads).
- An organisation server (`managedMcpServers`) loads with any flag list, `[]` too.
- End to end: `klefki-cli run Work --print` for a profile of a and loc printed the line; that line run in bash (with
  `mcp list` added) loaded a and loc only. For a profile of a and d with c allowed in `settings.local.json`, `run`
  said "loads from Mixed: a", d off, c loaded too, and exited 4 without a terminal; the printed line loaded a and c,
  d disabled, as said. `klefki-cli run Work` in tmux started the interactive Claude Code; only a and loc started.

## Done
- Core `start.rs`: `StartPlan { profile, project, args, line, to_do, loads, missing, extra }` and `plan()`. It reads
  the files twice, as now and as a session started with the list (`inventory::load_started_with`; `policy::load` takes
  the list as one more source, `Source::Flag`, which counts unless `allowManagedMcpServersOnly`), and compares what
  loads in the project (`ops::in_project`). Missing: off there (a start command turns nothing on), held back (the
  reason the list already gives), a name that does not count because of a command or address rule (naming the
  files), a plugin's server, a connector, a name with other characters, a local server of another project, a server
  no longer configured. Extra: the organisation's servers, and servers another file's allowed list lets in (naming
  the files). No line when no server can be named (organisation servers alone give `[]`) or with
  `allowManagedMcpServersOnly`. The line: POSIX `cd <project> && claude --settings '<json>'`; Windows
  `claude.exe --% --settings "<json with \">"` for PowerShell, plus "Run it in PowerShell, in <folder>." (no `cd`: a
  failed `Set-Location` would not stop the line). Only names go in the line, never commands or addresses, so no
  secret lands in a clipboard or shell history.
- Core `start::run` / `api::run_claude`: checks the arguments again (only `--settings` with only `serverName` rules
  of allowed names), the project folder, and the program (`locate::check`: no `.cmd`/`.bat`), then starts Claude
  Code with no shell in the project folder, without `MCP_CLIENT_SECRET`; `exec` on Unix; on Windows nothing is started yet (review round 1).
  `api::start_plan` for the CLI.
- Engine: `Command::StartCommand { profile, members, project }` → `Event::StartCommand { profile, project, plan }`.
- App (`profiles/start.rs`): with one project chosen on a card, "Copy start command" asks the engine; the line is
  copied when it arrives (not if the page was left), shown in a box, then either "Claude Code loads only these
  servers there: …" or, in the warning colour, "Not loaded, though in the profile:" / "Loaded too, though not in the
  profile:" with each reason, then "Nothing is saved…". Brought into view when it arrives (below the fold at
  800×480). A change to the files asks again, without copying. Screen readers hear "Copied" (live region).
- CLI: `run PROFILE [--project DIR] [--print] [--yes] [--claude PATH]`, own usage line; `--all`, `--only` refused;
  exit codes in decision 006. Decisions 006 and 012 updated.

## Tests
- Core `start/tests.rs` (14, 15 after review round 1): each live case above; names that cannot be let in; no line; unknown project; Windows
  line; `run` refuses arguments Klefki would not make. Seen failing by mutation: with the flag's list not counting (7
  fail) and with it replacing the user's list (2 fail).
- App `shell/tests/start_command.rs` (6, 9 after the reviews): offered only for one project; asks once and copies on arrival; differences
  said; no line says why and copies nothing; a file change asks again without copying (seen failing with the re-ask
  removed); an answer after leaving the page copies nothing.
- CLI (3): `--print` and a real start through fake-claude (argv and folder checked, `.claude.json` unchanged); a
  profile that differs is said and starts only with `--yes`; usage errors.
- Look: new `profile-start-command`, `profile-start-command-800x480`; `profile-switch-from` changed (the new button
  in the card's row, intended). Golden `help.txt` changed for `run` (intended).

## Checks (Linux)
- `cargo fmt --check`: exit 0. `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
  `cargo clippy -p klefki -- -D warnings`: exit 0.
- `cargo test --workspace`: exit 0 (1134 core, 557 app, 29 + 54 CLI).
- `cargo deny check`: advisories ok, bans ok, licenses ok, sources ok.
- Not run here: macOS and Windows (CI); the PowerShell line on a real Windows; connector names with a signed-in
  account; a plugin's server under a start command.

## Review round 1 (cross-review on #106)
- B1: on Windows, `run` waited on Claude Code with no console control handler, so the first Ctrl+C would end
  `klefki-cli` and leave Claude Code running under the shell. Now `run` on Windows prints the line, says to paste it
  in PowerShell, and exits 4 (`start::run` starts nothing there, `api::START_NOT_HERE`). A handler (as cargo's
  `exec_replace`) would need more `unsafe` in core, still an open decision for the owner.
- B2: PowerShell runs npm's `claude.ps1` for a bare `claude`, and `--%` reaches only a program: the line now names
  `claude.exe`. Core, CLI tests updated.
- B3: after a profile edit the panel kept the old line and "loads only…". `ProfilesPage::set` now asks again (without
  copying) for a profile still there, and drops the panel of one renamed or deleted. Regression test
  `an_edited_profile_is_asked_for_again` (seen failing with the re-ask removed), variant
  `a_renamed_profile_takes_its_start_command_with_it`.
- S1: when the session would not load just the profile's servers, nothing is copied until "Copy anyway", under what
  differs (as `run` asks first). S2: the card says what loads first, in normal text. S3: the project's own server of
  a name the profile lets in is said as such (core test).
- Pictures `profile-start-command(-800x480)` changed (Copy anyway, summary first). Not tried on a real Windows.

## Review round 2
- Approved at 9f85d8a (CI green on ubuntu, macOS, windows). Its one leftover, folded in: a profile edited after the
  ask but before the answer copied that first answer, about the servers before the edit. `Seen` counts the answers
  on their way and copies only the last. Regression test `an_answer_from_before_an_edit_is_not_copied` (seen failing
  first). A re-ask after a file change is no longer lost when another card asked for something in the same frame.
- 9f85d8a: the Windows CLI test expects "paste the line above in PowerShell" where Unix says "add --yes".

## Open / next
- A plugin's server can only be let in by its command; left out on purpose (see above). Said on the card.
- Starting Claude Code from `run` on Windows needs a console Ctrl+C handler (more `unsafe` in core, an owner
  decision); until then it prints the line.
- The lens menu (S3) has no "Copy start command"; the card has it.
- Next in the plan: S6 (overlap and cost) after the tools view.
