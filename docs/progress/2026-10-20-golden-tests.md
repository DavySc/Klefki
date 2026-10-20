# Simplification safety net, part 1: golden tests (2026-10-20)

Goal: the owner wants the code simplified as far as possible without losing robustness, functionality or the
handling of unforeseen situations, and the test and quality suite made ready for that first, taking ripgrep and
rust-analyzer as models. This slice is the first safety net: tests that pin what users see and what lands on disk,
byte for byte, so a refactor that changes either fails. No product code changes.

## Done
- Core, `engine/tests/golden.rs`: 17 tests, one per kind of change (turn off a user and a local server, a server
  that is already on, delete user and local, edit with a hidden value kept, rename, duplicate to two places,
  duplicate a server with secrets in its arguments, move
  local to user, add a stdio and an http server, a mixed plan, approve a project server, migrate the old switch-off
  list, use a profile alone then stop it, restore a backup, add an OAuth server). Each writes
  `engine/tests/golden/<name>.txt`: the review as the dialog shows it (summary, warnings, notes, what it would run,
  each file's diff, the button and whether it is dangerous), the outcome of Apply, and every file the change may
  write afterwards (`.claude.json`, the repository's `settings.local.json`, Klefki's profile files), raw. The OAuth
  one also records the `claude mcp add-json` command line and that the client secret (by its hash) went through
  `MCP_CLIENT_SECRET`, and asserts the secret appears nowhere.
- CLI, `tests/cli.rs` `golden_output_of_every_command`: stdout, stderr and exit code of `list`, `list --json`,
  `status`, `status --json`, `export`, `profiles`, `use` (nothing to do; `--only` unconfirmed; `--only --yes` with
  the file it leaves), `--help`, a mistyped command and an unknown name, in `tests/golden/*.txt`.
- Uses `expect-test` (dev-dependency only, as rust-analyzer does): a difference fails with a diff; `UPDATE_EXPECT=1`
  rewrites the files when a change is meant, and the reviewer reads the change in the pull request.
- Stable on every system: the temporary folder becomes `<tmp>` (also macOS's `/private` spelling, JSON-escaped and
  Windows spellings, paths under it with `/`), a backup's time `<time>` / `<stamp>`, the fake CLI's path
  `<fake-claude>`, the project `<project>`.

## Checks
- Seen failing: a golden file edited by one character fails `golden_turn_a_user_server_off` with the diff; put back,
  it passes. Each new test ran 4 times in a row: same output.
- Full verify (`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`): exit 0; app 420, core 932 (915 + 17), cli 25 and 34 (33 + 1), deny ok (again after the review items).

## Cross-review (approved, two optional items folded in)
- A server with secrets in its arguments (`golden_duplicate_a_server_with_secrets_in_its_arguments`): the review
  shows the arguments in full, secrets too, as the owner decided on 2026-10-19 (`klefki-cli list` hides them); now
  pinned in core as well as in the CLI golden files.
- The OAuth golden file checks the secret Claude Code received by its hash, not only that one was passed.
- Left for later (reviewer): the edit diff that lists `+ TOKEN`, `+ MODE`, `- TOKEN` and hides `MODE=fast`.

## Next
- Part 2: a manual and weekly Linux-only quality workflow (coverage, mutation testing of changed code, start-up
  and memory baseline), and the simplification plan for the owner.
