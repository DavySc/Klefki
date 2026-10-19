# Flaky tests: F5 check count, callback port, on-screen order (2026-10-19)

Goal: find and fix the cause of the engine tests that still failed now and then: the callback port test, `a_server_claude_checked_recently_is_left_alone_until_f5`, and `background_checks_of_servers_on_screen_go_first` (red on Windows in #29).

## Causes
- `a_server_claude_checked_recently_is_left_alone_until_f5`: a test race. After F5 it waited only for `recent` to answer, then counted the `mcp get` runs. F5 runs both checks side by side, and under load `recent` can answer before the second `fresh` process has started and logged itself. #29 carries the same change; it is brought to main here with the same text, so the merge is clean.
- Callback port tests (`the_engine_answers_whether_a_callback_port_is_held_for_that_port` and its siblings): already fixed on main by 87d38be (it asks again until a port it let go reads free). No failure in 440 loaded runs here (below). This container has no IPv6, so the `::1` case is covered only by macOS CI.
- `background_checks_of_servers_on_screen_go_first` on #29 (Windows): not a flake. It is a bug in #29's new `check_listed`. When the on-screen list arrives before the first background check starts, s3 runs the list for all five servers. The list gives no answer, so all five are queued again, and s3 goes in behind its mates. Reproduced on #29 every time by pausing 500 ms at worker start: `["s4", "s3", "s0", "s1", "s2"]`, the same as Windows CI. The fix is to queue the job ahead of its mates (`again.insert(0, …)`). With that fix and the pause, all 167 engine tests pass. The patch went to Davy, who owns #29. It is not in this branch, because main has no `check_listed`.

## Done
- F5 test: waits for both servers to answer before counting.
- New variant `f5_checks_one_at_a_time_still_check_every_one`: the same scenario with `max_checks: 1`. It checks that F5 still checks every server when its checks run one after another. It does not reproduce the race: the cross-review found it also passes with the old wait. The failing evidence is the stress run.
- No product code changed.

## Checks
- Stress (Linux, 4 cores): `engine::tests form::tests --test-threads=12`, 10 loops at once, 200 runs each.
  - main 7becd59: 2 of 200 failed, both `a_server_claude_checked_recently_is_left_alone_until_f5` at the F5 count.
  - This branch: 0 of 200 failed.
  - Earlier, at 6 loops at once: main 0 of 120.
  - The callback port tests did not fail in any of these 520 runs.
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`: exit 0 (app 314, core 655, cli 18 + 25; deny ok).

- Cross-review (#26 author): no objection; the variant's comment was corrected as asked. Main f306c48 merged in (be5627c, 379a3e8, merge 88f4669); the full check line re-run gives exit 0 (app 315, core 657, cli 18 + 25; deny ok).
- CI on be5627c: green on Linux, macOS and Windows.

## Open
- macOS and Windows: covered only by CI.
- #29 needs the one-hunk fix above before it can be green on Windows.

## Next
- Merge once CI is green on the merge commit.
