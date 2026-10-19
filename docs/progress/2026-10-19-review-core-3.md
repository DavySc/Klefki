# Review of core, part 3: files no open pull request touches (2026-10-19)
Goal: the third PR of the overnight review of `crates/core`, `crates/cli` and `crates/fake-claude` (part 1: `2026-10-18-review-core-1.md`, part 2: `2026-10-18-review-core-2.md`, #30). Davy asked for reviews for correctness, performance and simpler code while the merges wait. This part covers the files that #26, #28, #29 and #30 leave alone: `cli/login.rs`, `cli/runner.rs`, `cli/locate.rs`, `cli/parse.rs`, `cli/tree.rs`, `config/fsx.rs`, `config/jsonspan.rs`, `model.rs`, `secrets.rs`, `diagnose/report.rs`, `engine/sign_in.rs` and `klefki-cli`.

Made on main `5c27260` and first kept as patches. Davy asked for it as a pull request on 2026-10-19 08:08Z: applied with `git am` onto main `2208801` (after the history rewrite) on `claude/project-thread-ievcsk`, without conflicts.

## Done
- **The suggested check command could run shell code from `.mcp.json`** (`crates/cli/src/render.rs`). `klefki-cli status` leaves project servers out and says "to check it: klefki-cli status <name>". The name comes from the repository, and the quoting kept `$(…)` and backticks live inside double quotes and left `-a$HOME` unquoted. Copying the suggestion ran what the name held. The engine checks only names `ServerName::parse` accepts, so the command is now offered only for those, and such a name is the same word in any shell. Any other name says how to get it checked and which rule the name breaks ("to check it, rename it in the repository's .mcp.json (a name can hold only A-Z a-z 0-9 _ -, not '$')"; a space, a leading dash, a reserved word and the length each get their own wording). Test: `a_project_server_gets_a_check_command_only_when_its_name_can_be_checked` (failed first: `to check it: klefki-cli status "x$(echo INJECTED)"`).
- **A long line the server printed twice showed twice in Diagnose** (`diagnose/report.rs`). A repeat was compared with the line as cut for showing (300 characters), so a longer line never matched its earlier copy. The client logs a stdio server's stderr twice, so every long stderr line was doubled and used the 20-line limit twice as fast. Test: `a_long_line_the_server_printed_twice_is_shown_once`, at 299, 300, 301 and 601 characters (failed first at 301).
- **A notice after a pasted sign-in address read as Claude refusing it** (`cli/login.rs`). `refusal` skipped fewer lines than `last_message`: an `[mcp-sdk]` notice, "Waiting for authorization", "Starting authentication" or "Visit this URL" printed after the paste was reported as Claude not taking the address. Both now use one filter (`said`). Whether Claude prints such a line after a paste is unknown. Test: `a_notice_after_a_paste_is_not_taken_for_claudes_refusal` (failed first: `Some("[mcp-sdk] SEP-2352 notice")`).
- **Simpler, same behaviour:**
  - `login.rs`: the sign-in no longer cleans the whole kept output (up to 64 KB) for every chunk Claude prints. It is cleaned once, when Claude ends.
  - `runner.rs`: a timed-out run keeps no exit status to throw away later.
  - `fsx.rs`: a byte-order mark is removed in place instead of copying the file (up to 64 MB).
  - `report.rs`: the fallback keeps the last 25 lines without collecting every line first.
  - `klefki-cli`: sorting splits each name once instead of at every comparison (the performance thread measured `ordered` on this code: 3,950 rows, release, median of 9 runs, 5.50 ms on main and 1.69 ms here, same order) and borrows the project name instead of copying it; `transport_key` names its JSON values like its siblings do, so a changed table label cannot change the JSON; the duplicate `serde_json` dev-dependency is gone.
  - Comments: `jsonspan.rs` said it never recurses (it recurses into the objects `descend` accepts), and `Secret::edit` promised that no plain copy is made.

## Pre-review (performance thread, by message, 02:33Z)
Two should-fixes, both done:
- The line for a name that can't be checked used "checked" in two senses and gave no way out (NN/g's error-message rubric, guidelines 6 and 7). It now says to rename the server in the repository's `.mcp.json` and words each rule itself; `NameError`'s own text is left alone, since the app's name field shows it.
- Optional, done (the sort commit, checked by the performance thread): the borrow alone did not speed up the sort; splitting each name once does. Test `rows_in_a_group_sort_as_people_count` checks `ordered` against `natural` (fails when the name order is reversed).
- Taking the captured output instead of copying it let a reader thread still draining a pipe (a grandchild holds it) keep another full buffer. The copy stays; it was one copy of up to 64 KiB per run.

## Checks (Linux, 2026-10-19)
- Each new test failed before its fix (quoted above).
- Verify all (`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`): exit 0, deny ok; 703 tests before and after the pre-review fixes, 704 with the sort test.
- End to end: `klefki-cli status` with fake-claude as `claude`, a temporary `CLAUDE_CONFIG_DIR` and a repository whose `.mcp.json` names `docs`, `x$(echo INJECTED)` and `-a;echo INJECTED`. `docs` gets "to check it: klefki-cli status docs"; the other two get the rename line naming `'$'` and `';'`. Exit 0.
- Merges cleanly with #26, #28, #29 and #30 (`git merge-tree`).
- On main `2208801` with the patches applied: verify all exits 0, deny ok, 801 tests passed, 0 failed (Linux, 2026-10-19).
- Not run: Windows and macOS. The command is offered only for `[A-Za-z0-9_-]` names without a leading dash, which are inert in sh, cmd and PowerShell (reasoned, not run).

## Open
- `command_word` in `render.rs` now only ever gets a valid name, which it returns unchanged. Removing it means removing its test `names_in_the_check_command_survive_a_shell`, whose expected outputs (`"my server"`, `-- -x`) are the unsafe quoting. CLAUDE.md asks for Davy's agreement before removing a test, so both stay for now.

## Next
- Another thread reviews the pull request; then merge after CI is green on all three systems.
