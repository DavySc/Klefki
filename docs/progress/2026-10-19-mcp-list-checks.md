# Background checks through one `claude mcp list` (2026-10-19)
Goal: start-up checks took one `claude mcp get` per server, one after another (30 servers: 28.5 s). One `claude mcp list` per folder checks them all at once and says what `get` says. Davy's rule: a verdict must never differ from Claude Code's, so Claude still produces every verdict; the list only replaces many starts of Claude by one. (From the 2026-10-19 performance evaluation, item 1; items 2, 3, 4 and 6 were rejected by Davy, item 1's own-handshake variant was researched and left out.) Second round: Davy called a delay of up to 30 s unacceptable, and asked for the open issues to be fixed.

Branch `claude/mcp-list-checks`, from main `2279dd4` (PR #22 merged in; its `Job.now` and `Queue` changes sit beside this slice's `Job.alone` and `Queue::take_mates`). Not committed.

## What it does
- A worker that takes a background check first looks for other waiting background checks in the same folder (`Queue::take_mates`). With at least 4 in all (`MIN_LIST_BATCH`: a list starts every server of the folder, so for fewer, separate gets touch only those asked) it runs `claude mcp list` there.
- The list is run with `ENABLE_CLAUDEAI_MCP_SERVERS=false` (the account's 21 connectors added 12-25 s and are not servers of these files), `MCP_TIMEOUT=3000` and `MCP_SERVER_CONNECTION_BATCH_SIZE=10` (`LIST_ENV`). **Why:** a list prints only when its last server is done, so one hanging server held back every other status in the folder for Claude's 30 s. With a 3 s timeout, and 10 servers connected at once instead of Claude's 3, the same folder takes about 6-8 s (6 hanging servers: 13.5 s at 3 at once, 8.4 s at 10; the floor is Claude's 3 s probe plus the timeout).
- A short timeout must not change a verdict, so `parse::parse_list` returns `Listed::Settled` only for what it cannot have changed: Connected, needs sign-in, pending approval, and a failure or "tools fetch failed" whose reason says nothing about time (an issue containing "time" is not settled, nor is a failure without an issue). Everything else is `Listed::Open`, and `get` (30 s) decides. Unchanged from the first round: a server must name exactly one line and the line hold exactly one ` - <status>`.
- Servers the list did not settle (and the job that ran it, if it is one of them) go back to the queue as `alone`, ordered so that servers that answered the connection too slowly come first and those that never answered ("connection timed out", a hang, whose own `get` takes 30 s) last. A list that gives nothing readable (fails, times out, unreadable) switches listing off for the rest of the run: every check is a `get`.
- Checks the user asks for, single edits and the CLI's `status` are unchanged (`get`).

## Measured (Windows 11, Claude Code 2.1.303 native, lab of small Python servers)
- `get` x30 one after another: 28.5 s. `list` x1: 2.6 s. App on the 30-server lab config (debug build): all 30 started within 5.9 s of launch.
- The engine on the real `claude`, 13 lab servers with one that hangs (scratch test, not kept): before the change, every status at about 31 s. Now 11 servers have their final status at 6.3 s, a server that needs 4 s to start at 11.4 s (its own `get`), and the hanging server at 41.6 s: its own 30 s verdict, behind the others.
- Parity: `list` at `MCP_TIMEOUT=3000` against `get` over 34 lab servers: the 28 answers taken were identical to `get` in status and issue (0 differences); the 6 that ran out of time were left to `get`. Earlier, without the short timeout: 23 of 23 identical.
- Real configuration (3 servers, one `npx`), list without connectors: 6.8 s and 9.6 s; the three `get`s took 26.6 s together.
- Known cost left: a server that really needs more than 3 s to start (a cold `npx`) is checked with `get`, one at a time after the list, as before; and a hanging server's own verdict still takes 30 s.

## Tests (each failed first)
- `parse`: `a_real_list_gives_the_same_answers_as_get_for_each_server`, `a_list_run_with_a_short_timeout_answers_only_what_the_timeout_cannot_have_changed`, `however_a_timeout_is_worded_it_is_not_settled`, `a_failure_that_says_nothing_about_why_is_not_settled`, `an_open_answer_says_whether_the_server_ever_answered_the_connection`, `crlf_list_output_parses_the_same`, `other_lines_are_ignored`, `a_dash_in_the_command_is_not_a_status`, `the_issue_of_a_list_line_is_sanitised_and_clipped`; variants that must stay true: a server not named, named twice, a command or issue that looks like a status, a line without a status in Claude's words, a longer name for a shorter one, answers that need no connection.
- `engine`: `background_checks_in_one_folder_share_one_claude_list`, `each_folder_gets_its_own_list`, `a_server_the_list_does_not_answer_clearly_is_checked_on_its_own`, `a_list_answer_a_timeout_may_have_caused_is_checked_with_get`, `a_server_that_never_connected_in_the_list_is_checked_after_one_that_was_only_slow`, `the_list_is_asked_for_a_short_timeout_and_a_wide_batch_without_the_connectors` (closes the earlier "environment not tested" gap without touching the protected `fake-claude`), `when_the_list_fails_every_server_is_checked_with_get` (exit 1; exit 0 with nothing readable), `a_list_that_hangs_is_ended_and_the_servers_are_checked_with_get`, `a_check_the_user_asks_for_is_always_a_get`; guard `a_few_servers_are_checked_one_by_one`.

## Flaky engine tests on Windows (fixed)
- Symptom: with the default number of test threads, 1-4 of `background_checks_of_servers_on_screen_go_first`, `checks_nobody_asked_for_run_one_at_a_time`, `a_server_claude_checked_recently_is_left_alone_until_f5` (and this slice's hang test) failed on this machine, also on unchanged main (two runs: 1 and 3 failures).
- Cause, measured by timing the runner: not the pipe grace (pipes closed in microseconds) and not leaked handles (a lock around every process start did not help, reverted). Starting a process took 1-3 s while the whole engine suite started processes at once, and these tests start several one after another, so they passed their 15 s wait. `WAIT` is now 60 s (it only ends a hang).
- One more was a race in the test itself: the F5 test counted the `get`s after only `recent` had answered; `fresh`'s second check may not have started. It now waits for both.
- After: `cargo test -p klefki-core --lib -- engine::` 4 of 4 runs and `cargo test --workspace` 2 of 2 runs, default threads, no failure.

## Checks
- `cargo fmt --check`: exit 0. `cargo clippy --workspace --all-targets -- -D warnings`: exit 0. `cargo deny check`: advisories, bans, licenses, sources ok.
- `cargo test --workspace` (default threads, twice): core 622, app 303, cli 20, others, all passed.

## Open
- Not verified: macOS, Linux, OAuth/HTTP servers through the list (only unreachable hosts), a managed-policy allow/deny list.
- A hanging server's own check still takes 30 s (that is Claude's verdict for it), now after the others.
- Proposal for `fake-claude` (protected): log selected environment variables in its start line; then the engine test could assert what the spawned list got, instead of what `list_env` builds.

## Next
Review and open the pull request.
