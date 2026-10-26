# cargo deny advisories (2026-10-26)

Goal: `cargo deny check` green on main with the smallest honest change.

Failures on main: RUSTSEC-2026-0385 (`ctutils` 0.4.3, via digest 0.11.3 / secret-service) and yanked `clipboard-win` 5.4.1 (via arboard / egui-winit).

Done: `cargo update -p ctutils -p clipboard-win` → ctutils 0.4.6, clipboard-win 5.4.3. Only `Cargo.lock` changed; no `deny.toml` ignore needed, no dependency trimmed.

Checks (all exit 0): `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (1135 + 560 + 54 + 29 passed, 0 failed), `cargo deny check` (advisories, bans, licenses, sources ok).

Next: cross-review, then merge after green CI on all 3 systems.
