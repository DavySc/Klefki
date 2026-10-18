# Fresh status at startup

Goal: Davy reported that klefki opens with stale connection state. Fix it without slowing the launch.

## Cause
At startup the engine ran `refresh(.., live_all: false)`: only servers Claude's caches (`mcp-health-cache.json`,
`mcp-needs-auth-cache.json`) did not know were checked. A server the cache called healthy (or failed, or needing
auth) kept that answer, however old, until F5. The row looked like a fresh answer.

## Done
- **Core** (`engine.rs`): startup checks every enabled user and local server, in the background. Servers with
  nothing to show are queued first (as before); the ones the cache knows follow and keep their cached status until
  their own answer comes (no "Checking…" flash). Project (`.mcp.json`) servers are still only checked on request;
  switched-off servers are not checked. Same worker pool and `max_checks` limit; the window opens no later.
- **App** (`shell.rs`): a status that only a check can establish (Connected, Degraded, Needs auth, Failed) shown
  from Claude's cache, with no answer from this session yet, is drawn as a ring `○` in the status colour with muted words, instead of a dot and coloured words, with the
  tooltip "What Claude Code last saw, from its own cache, which can be old…". The Status tab says
  "Connected (last known)". The column (176 px) has no room for the words; "Connected (last known)" was cut off. A first try faded the status colour, which broke the palette's 4.5:1
  contrast in the light theme (green on white about 2.4:1); the ring keeps the palette's own colours.
- Tests: `servers_claude_already_knows_are_not_rechecked_until_a_full_refresh` encoded the reported behaviour; it is
  replaced by `servers_claude_already_knows_are_rechecked_at_startup_keeping_their_last_known_status` (fails on the
  old code: the cached server was never asked) and the variant
  `servers_with_nothing_to_show_are_checked_before_the_ones_claude_knows` (one check at a time, order unknown →
  needs-auth → healthy, the switched-off one not checked). App: `a_status_from_claudes_cache_says_it_is_last_known_until_its_check_answers`,
  `only_statuses_a_check_establishes_are_marked_last_known`. All four failed before the fix.

## Checks
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`: exit 0; 338 core + 153 app tests passed.
- `cargo clippy -p klefki -- -D warnings`: 0. No `std::fs`, `process::Command`, `std::net` or `open::` in `crates/app/src`.
- Real window (`cargo run`-built binary under Xvfb, temp `CLAUDE_CONFIG_DIR`, fake-claude as `claude` on PATH, a
  cached-healthy server whose check answers Failed after 6 s): at 2.5 s it shows `○ Connected` with the tooltip; at 8.5 s `● Failed` with the explanation. Ring and
  muted words checked in both themes (`--theme dark|light`).

## Open / next
- Cost: each check is a `claude` (Node) process; startup now runs one per enabled user/local server instead of one
  per unknown server, still at most `max_checks` (3) at once. The assessment's proposal (fewer concurrent processes,
  rows in view first) fits on top: it only changes the queue order and limit.
- Claude's health cache may carry a timestamp per entry; if it does, the tooltip could say how old it is. Not checked.
- Not verified on Windows/macOS.
- After merging main (#17 paste to add, `3321582`): full verify exit 0, 361 core + 161 app tests passed. Merged on Davy's go.
