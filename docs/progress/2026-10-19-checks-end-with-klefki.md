# No server outlives a check (2026-10-19)
Goal: two open items from `2026-10-18-kill-process-tree.md` and `2026-10-18-cli-list-status.md`. (1) Ctrl+C during `klefki-cli status` ended klefki-cli but not the running checks or the servers they started: a check leads a process group (Windows: a job) of its own, so the terminal's Ctrl+C never reaches it. (2) When Claude's own wait for a server ran out before Klefki's timeout, Claude exited 0 and ended its server but not what the server started (npx, then node), and those stayed.

## Done
- `RunSpec::end_leftovers` (new). `true` for `mcp get` checks and Diagnose, `false` for every other run (`--version`, add-json, remove, sign-in), which keep the earlier rule: what Claude starts on purpose, such as an update, is left alone.
- `cli/tree.rs`:
  - `has_ended` sees that the process ended without waiting for it (Unix: `waitid` with `WNOWAIT`), so the group id is still its own when the runner ends the leftovers; then it is waited for. Windows: `try_wait` (a job keeps its own handle).
  - Unix: a tree started with `end_leftovers` goes on a fixed list of 64 atomic slots (a signal handler reads it; no lock, no allocation). The first such start installs, once per process, an `atexit` function and handlers for SIGINT, SIGTERM and SIGHUP that only replace the default action (a signal ignored under `nohup`, or one with a handler, is left alone). Each SIGKILLs the listed groups; the handler then restores the default and re-raises, so the shell sees an interrupted program. A group is taken off the list before its leader is waited for. SIGKILL of Klefki cannot be caught.
  - Windows: such a tree's job also gets `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so it ends with Klefki however Klefki ends (Ctrl+C, closed window, Task Manager). Not used for sign-in, whose terminal may open a browser.
- This also covers klefki-cli returning while checks still run, and closing the app during a check; no new public API, no new dependency.
- Test helpers: `testutil::process_tree_left_behind` (the first level ends by itself once every level below it runs) and `end_pid`.

## Checks
- Reproducers, each seen failing on main first:
  - `runner::tests::a_check_ends_what_claude_left_running_when_it_ends_by_itself` (depth 1 and 2): "process … (depth 1) still runs".
  - `runner::tests::klefki_ending_during_a_check_ends_the_check_and_what_it_started` (exit; Unix also SIGINT, SIGTERM, SIGHUP; runs the test binary as `ends_during_a_check`): "exit (exit status: 0): […] still ran".
  - `klefki-cli` `status_ended_early_leaves_no_check_running` (Unix: SIGINT, SIGTERM, SIGHUP, and klefki-cli must end by the signal; Windows: klefki-cli ended outright): "SIGINT: the check … still ran after klefki-cli ended".
  - Kept behaviour: `runner::tests::other_runs_leave_what_claude_started_alone`.
- Mutation checks: no `atexit` fails the exit case; no signal handlers fail SIGINT in both tests; no kill on a normal end fails the first test.
- Cross-checked with clippy `-D warnings` for `x86_64-pc-windows-gnu` (core, cli) and `aarch64-apple-darwin` (core): clean. Windows and macOS tests run only in CI.
- Verify-all (`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`): exit 0; core 607 (608 after review), app 303, cli 17 + 23 passed; deny "advisories ok, bans ok, licenses ok, sources ok". No helper or fake-claude process left after a run.

## Open
- Diagnose passes `end_leftovers: true` but has no test of its own; the check path is covered end to end by the klefki-cli test.
- Unchanged from before: a process that leaves the group (`setsid`, a daemon) or the job (breakaway) is not ended.

## Cross-review (on cad5973): one fix, polish folded in once
- Reviewer ran real Claude Code 2.1.302 with `klefki-cli status` and a hanging stdio server that starts a child: with Claude's own timeout (`MCP_TIMEOUT=3000`) and with SIGINT mid-check, main leaves processes behind and this branch leaves none (exit 130 on SIGINT).
- Fixed: on macOS `waitid(WEXITED)` also reports a stopped process (golang/go#19314), so a stopped `claude` would have been taken for ended, its group killed and its leader waited for without a timeout. `has_ended` now counts only `CLD_EXITED`, `CLD_KILLED` and `CLD_DUMPED`. Reproducer `runner::tests::a_check_that_is_stopped_for_a_while_is_not_taken_for_ended` (SIGSTOP, then SIGCONT): with Linux made to report stops too (`WSTOPPED`) and no `si_code` check it fails ("exit_code: None"), with the check it passes.
- Polish: check and Diagnose runs get `DISABLE_AUTOUPDATER=1`, so a check never starts an update that it would then end; one name (`end_leftovers`) in tree.rs too; the `end_all` SAFETY note names the remaining instant between a handler's load and the runner's wait.

## Next
CI green on all three systems, merge.
