# End the whole process tree on a timeout (2026-10-18)
Goal: the open item in `docs/progress.md`: a timeout killed only `claude`, so a server it started (npx, then node) could stay behind holding memory.

## Done
- `cli/tree.rs` (new): `tree::spawn` starts a command so it can be ended with everything it starts. Unix: the child leads its own process group (`process_group(0)`); `Tree::kill` sends `SIGKILL` to the group. Windows: the child starts suspended (`CREATE_SUSPENDED`, plus the existing `CREATE_NO_WINDOW`), is put in a new job object, then its thread is resumed, so nothing it starts escapes the job; `Tree::kill` is `TerminateJobObject`. If the job cannot be made, only the process itself is killed, as before.
- `cli/runner.rs`: spawns through `tree::spawn`; on a timeout kills the tree before the child is waited for (the group id stays valid until then).
- `cli/login.rs`: `Tree::of_pty` for the sign-in terminal. Unix: portable-pty makes the child a session leader, so its group is ended. Windows: the process is put in a job right after it starts (portable-pty cannot start it suspended), so in theory a process started in its first instant escapes; `mcp login` starts none. The tree is ended only while the process has not been seen to end. `start` is split into `start` + `launch` so tests can launch another program in a real terminal.
- Only a timeout or cancel ends a tree. When Claude ends by itself, whatever it left running is left alone (it may start an update on purpose). No job kill-on-close.
- New direct dependencies: `libc` 0.2 (Unix) and `windows-sys` 0.61 (Windows), both already in the tree; `cargo deny` ok. This adds the first `unsafe` blocks in core (each with a SAFETY note): `kill`, and the job/thread calls on Windows.
- Test helper: `testutil::process_tree` runs the test binary itself (`--exact cli::testutil::tree_helper`) as a stand-in that starts N more levels below it, each ignoring SIGHUP and waiting forever; `tree_helper` does nothing in a normal run. `pid_alive` on Unix now uses `ps` and counts a zombie as ended (orphans may never be reaped in a container). `gone_soon` moved to testutil.

## Checks
- Reproducers first: `runner::tests::a_timeout_ends_everything_the_child_started` (depth 1 and 2) failed before the fix ("took 8.0s", helpers left running); `login::tests::cancel_and_timeout_end_everything_claude_started` (cancel and timeout, checked while the terminal is still open) failed with the group kill removed. Removing `process_group(0)` also fails the runner test.
- Cross-checked with clippy `-D warnings` for `x86_64-pc-windows-gnu` and `aarch64-apple-darwin`: clean. Windows and macOS tests run only in CI.
- Verify-all (`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`): exit 0, 658 passed, deny "advisories ok, bans ok, licenses ok, sources ok". No helper process left after the run.

## Open
- Not tried with real Claude Code starting a real stdio server that hangs.
- A process that leaves the group itself (`setsid`, a daemon) or a job (breakaway) is not ended.

## Next
Cross-review, CI green on all three systems, merge.
