# Flaky tests: kept definition, callback port (2026-10-19)

Goal: make four tests that failed under load pass reliably, changing tests only.

## Causes
- `engine::tests::a_kept_definition_whose_change_got_through_is_not_offered`: the final `collect` stopped at the first snapshot. A watcher refresh still pending from `keep_sso` can arrive before the one for the test's own write, and that snapshot rightly still lists the kept definition.
- `engine::tests::the_engine_answers_whether_a_callback_port_is_held_for_that_port`, its sibling `a_callback_port_another_program_holds_is_warned_about_in_the_review_and_the_add_goes_on`, and `form::tests::a_callback_port_another_program_holds_is_warned_about_and_a_free_one_is_not`: every failure was the "free once let go" assertion. Right after the test drops its listener, the check can still find the port in use, while a bind a moment later succeeds (seen with a diagnostic build: free again on the next bind, nothing in `/proc/net/tcp`). Likely holders, inferred: a child process another test forks keeps a copy of the listener until it runs its program; and on a computer with IPv6, another test can take the same number on `::1`, which a listener on 127.0.0.1 leaves free (the macOS CI failure). The container used here has no IPv6, so only the first cause could happen here.

## Done
- The kept test collects until a snapshot whose `kept` is empty, bounded by `WAIT`, then asserts as before.
- The port tests ask again until the port reads free, up to `WAIT` (15 s in the form test). The held phase is unchanged: it must warn at once. A check that always warns still fails, by timing out on the free phase.
- No product code changed.

## Checks
- Before (main 2208801): the engine and form tests (`engine::tests form::tests`, `--test-threads=12`) run 400 times, four to eight loops at once. 57 of 400 runs had a failure: callback port review test 41, kept definition 8, callback port engine test 7, form port test 3; no other test failed.
- After: see the pull request; filled in below once run.
- `cargo fmt --check` 0; `cargo clippy --workspace --all-targets -- -D warnings` 0.

## Next
- Drive the pull request to green CI on all three systems.
