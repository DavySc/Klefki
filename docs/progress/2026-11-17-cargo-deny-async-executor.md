# cargo deny: yanked async-executor (2026-11-17)

Goal: `cargo deny check` green on main with the smallest honest change.

Failure on main (8be95c9): yanked `async-executor` 1.14.0 (via accesskit_unix / egui-winit).

Done: `cargo update -p async-executor` → 1.14.1. Only `Cargo.lock` changed (version + checksum); no `deny.toml` ignore, no other bump.

Checks (all exit 0 after the change; `cargo deny check` exited 1 before): `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (1194 + 563 + 54 + 29 passed, 0 failed), `cargo deny check`.

Next: cross-review, then merge after green CI on all 3 systems.
