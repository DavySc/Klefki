# A signal while a check starts still ends it

Goal: Ctrl+C (SIGINT), SIGTERM or SIGHUP that reaches Klefki or klefki-cli while a check is being started must still
end that check and what it started. Found by the golden-tests slice: `status_ended_early_leaves_no_check_running`
failed 3 times in 80 under load.

## Cause
`tree::spawn` put the check's process group on the list that the signal handler ends only after `Command::spawn`
returned. By then the check runs (std returns once it has exec'd), so a signal in between ended Klefki and left the
check, which leads a group of its own and so never sees the terminal's Ctrl+C, running.

## Done
- `tree.rs` (Unix): a start with `end_leftovers` is counted in `live::STARTING` from before the spawn until its group
  is listed (or the spawn failed). The signal handler, when a start is in flight, records the signal in the same
  word and returns; the last start to finish then ends every listed group and Klefki as that signal would. One
  atomic word, so the handler and the finishing start cannot each leave the signal to the other. The handler now has
  `SA_RESTART`, as a held signal returns from it. No new kinds of unsafe: the existing `signal`/`raise` block moved
  into `end_as`, shared by the handler and the finishing start.
- Windows unchanged: the child is created suspended and resumed only once it is in its job, so it cannot run (or
  start anything) outside it. If Klefki is killed in between, the suspended child is left, but it never runs.
- Test hook `tree::AFTER_SPAWN` (test builds, Unix only) runs between the spawn and the listing.
- Regression test `cli::runner::tests::a_signal_while_a_check_starts_ends_the_check_and_what_it_started`: the helper
  process raises SIGINT/SIGTERM/SIGHUP from that hook once the check's tree runs. Deterministic, not a stress test.
  Without the fix it fails every time ("SIGINT (signal: 2 (SIGINT)): [...] still ran").

## Checks
- Full verify (fmt, clippy -D warnings, test --workspace, deny): exit 0.
- New test with the fix disabled (start not counted): fails, twice tried. With the fix: passes, 5 of 5.
- Stress, `status_ended_early_leaves_no_check_running`, 80 runs 16 at a time on 4 cores: main 3 of 80 failed (twice);
  with the fix 0 of 80, four rounds (320 runs).

## Open
- Not covered: `exit` (the `atexit` path) while another thread is starting a check; the check started in that
  instant is not ended. Same as before; exit cannot wait.

## Next
Cross-review, CI on all three systems, merge.
