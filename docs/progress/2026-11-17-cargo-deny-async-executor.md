# cargo deny: yanked async-executor (2026-11-17)

Goal: `cargo deny check` green on main with the smallest honest change.

Failure on main (8be95c9): yanked `async-executor` 1.14.0 (via accesskit_unix / egui-winit).

Done: `cargo update -p async-executor` → 1.14.1. Only `Cargo.lock` changed (version + checksum); no `deny.toml` ignore, no other bump.

Also folded in (decision of the coordinator and the reviewer): Windows clippy on the newest stable (1.100) failed `needless_bool` in `is_program` (`crates/core/src/config/preflight.rs`), because the `#[cfg(unix)]` block is compiled out there. The unix branch now tests `!self.windows && meta.is_file()` and the tail is `meta.is_file()`; behaviour is the same on every platform. Windows view reproduced on Linux by deleting the unix block: old code gave the error, new code is clean. Real Windows result: CI.

Checks (all exit 0 after the change; `cargo deny check` exited 1 before): `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (1194 + 563 + 54 + 29 passed, 0 failed), `cargo deny check`.

Next: cross-review, then merge after green CI on all 3 systems.
