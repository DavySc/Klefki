# Fresh status at startup

Goal: Davy reported that klefki opens with stale connection state. Fix it without slowing the launch or raising
peak memory (bar from the assessment thread: under about 350 MB with 30 servers).

## Cause
- At startup the engine checked only servers Claude's caches did not know. A cached status stayed until F5 and
  looked like a fresh answer.
- Claude Code 2.1.302 no longer writes `mcp-health-cache.json` (no such string in the binary): what is in it was left
  by an older version and never changes. That is why it stayed stale.
- `mcp-needs-auth-cache.json` is still written, as `{name: {timestamp, id?, ttlMs?}}`. Claude trusts an entry for
  `ttlMs`, else 15 minutes (4 hours for claude.ai proxy and plugin servers), allowing 60 s of clock skew, and ignores
  it after (read from the 2.1.302 bundle: `Gr=900000, Wr=14400000, Yr=60000`). klefki took any entry as Needs auth.

## Done
- **Inventory:** `ServerRow::status_cached`: the status is only Claude's cache's last-known one. Health-cache statuses
  always are. A needs-auth entry within its time to live is fresh (not checked again until F5); past it, it is
  ignored as Claude ignores it; without a usable time, last known.
- **Engine:** startup and changes from outside check the rows that need it (unknown or last known), as background
  jobs: one at a time (`BACKGROUND_CHECKS`), on-screen rows first (`Command::Visible`), then those with nothing to
  show, then the rest. F5 and Check now / Diagnose are not background: up to `max_checks` (3) at once, ahead of the
  queue. A last-known row is not announced as "Checking…" and keeps its status if it cannot be checked (unsafe name);
  an unknown one still says "not checked: why". A server switched on or added from outside is checked even when the
  cache knows it (cross-review finding).
- **App:** a last-known row (`status_cached`, no answer from this session yet) shows a ring `○` in the status colour
  with muted words, tooltip "What Claude Code last saw, from its own cache, which can be old."; the Status tab says
  "Connected (last known)". Fading the colour broke the palette's 4.5:1 contrast in the light theme, so the ring keeps
  the palette's colours. The grid tells the engine which servers it drew (only rows in view are drawn), when that
  changes; two reused buffers, no per-frame allocation.
- **Tests** (each seen failing against the old or a mutated implementation):
  - Replaced `servers_claude_already_knows_are_not_rechecked_until_a_full_refresh` (it asserted the reported bug). Its
    F5 half is restored as `a_server_claude_checked_recently_is_left_alone_until_f5`.
  - `at_most_three_checks_run_at_once` now triggers the checks with F5 (startup checks are one at a time); its ≤3
    and ≥2 assertions are unchanged. **For review.**
  - `status_comes_from_claudes_caches`: its needs-auth entry had `"timestamp":1` (1970), which Claude ignores; now
    written "now". **For review.**
  - New core: `servers_claude_already_knows_are_rechecked_at_startup_keeping_their_last_known_status`,
    `servers_with_nothing_to_show_are_checked_before_the_ones_claude_knows`, `checks_nobody_asked_for_run_one_at_a_time`,
    `a_check_the_user_asks_for_does_not_wait_for_the_background_ones`, `background_checks_of_servers_on_screen_go_first`,
    `a_known_server_switched_on_from_outside_is_checked`, `a_known_server_that_cannot_be_checked_keeps_its_last_known_status`,
    `a_needs_auth_entry_counts_only_as_long_as_claude_trusts_it`.
  - New app: `a_status_from_claudes_cache_says_it_is_last_known_until_its_check_answers`,
    `only_statuses_from_claudes_cache_are_marked_last_known`, `the_engine_is_told_which_servers_are_on_screen_when_that_changes`.
    The app tests' `drain` helper leaves out `Command::Visible` (the grid sends it by itself).

## Not done, on purpose
- Deduplicating identical local servers across projects (review item 5): `claude mcp get` runs in each project's
  folder, so the same definition can answer differently (relative paths, `.env`). With one background check at a
  time it costs time, not memory; on-screen rows go first.

## Checks
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`:
  exit 0; 368 core + 162 app tests passed (main merged in at `3321582`). Engine tests run 5 more times: all passed.
- `cargo clippy -p klefki -- -D warnings`: 0. No `std::fs`, `process::Command`, `std::net` or `open::` in `crates/app/src`.
- Peak memory, release builds under Xvfb, temp `HOME` and `CLAUDE_CONFIG_DIR`, 30 user stdio servers all
  "healthy" in the health cache, the real `claude` 2.1.302 on PATH, summed RSS of klefki and its descendants sampled
  every 50 ms for 60 s:
  - main: 124 MB, no checks (the bug: stale statuses).
  - this branch: 379 MB = klefki 121 + one `claude mcp get` 213 + the server it starts 40; all 30 checked by 24 s.
  - The previous version of this branch (three at once) measured about 700 MB in the assessment thread.
  - One check at a time is the floor short of not checking: here the `claude` binary alone is 213 MB, so the
    350 MB bar cannot be met with any check running. On a machine where `claude` is ~190 MB it is about 350 MB.
- Real window earlier in this slice: `○ Connected` at 2.5 s turning into `● Failed`, both themes.

## Open / next
- Not verified on Windows/macOS (CI runs the tests there).
- The 350 MB bar: met only if checks stop starting the server's own process, which `claude mcp get` always does.
