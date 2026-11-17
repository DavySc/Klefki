# is_program: clippy needless_bool on Windows (2026-11-17)

Goal: Windows CI clippy green again with newest stable (1.100) without changing behaviour.

Failure: `needless_bool` at `crates/core/src/config/preflight.rs` `is_program`. On Windows the `#[cfg(unix)]` block is compiled out, leaving `if !meta.is_file() { return false; } true`. Seen on PR #113's `check (windows-latest)`; main's last Windows run (8be95c9, older clippy) passed.

Done: the unix branch now tests `!self.windows && meta.is_file()` and the tail is `meta.is_file()`. Same results on every platform; no test changed.

Checks (Linux, exit 0): `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (1194 + 563 + 54 + 29 passed). Windows view reproduced by temporarily deleting the unix block: the old code gave the `needless_bool` error, the new code is clean. Real Windows result: CI.

Next: cross-review, green CI on all 3 systems, merge. `cargo deny` stays red on main until #113 (async-executor) merges.
